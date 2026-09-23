//! `reasonix`（DeepSeek-Reasonix CLI）adapter —— 上游 `server/pkg/agent/reasonix.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `reasonixACPLaunchArgs`（`reasonix.go` L36）+ `filterCustomArgs` |
//! | `BLOCKED` | `reasonixBlockedArgs`（L17） |
//! | [`FLAVOR`] | `reasonixBackend.Execute` 的会话 / `set_model` / effort 三段 |
//! | [`super::acp_core::AcpDecoder`] | 复用的 `hermesClient`（L155 起） |
//!
//! # 命令面
//!
//! `reasonix acp` 后面**必须**跟一串 daemon 自己定的策略开关（`--profile balanced`
//! `--planner auto` `--sandbox-* auto` `--workspace-only`）：Reasonix 的默认姿态比
//! 我们的无人值守任务更宽松，这串 argv 把它按"可降级的沙箱 + 只动工作区"钉住。
//! 它们同时也在封锁表里 —— 用户不能通过 `extra_args` 把沙箱改成 `off`。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 `session/resume`（带 `cwd` + `mcpServers`）；
//! * 模型走 `session/set_model`，**失败致命**（`--model` 也在封锁表里）；
//! * 推理等级走 `session/set_config_option {configId: "effort"}`，失败**不致命**
//!   （上游注释：降级运行好过报错）；
//! * 工具名在 hermes 归一后再过一遍 `reasonixToolNameFromTitle`：Reasonix 的
//!   title 是首字母大写的（`"Read file: /x"`），hermes 的表只认小写前缀
//!   ⇒ 复用 [`AcpToolAliases::Kimi`] 那张表，效果等价。
//!
//! # 有意不搬的部分
//!
//! 上游用 `streamingCurrentTurn` 把**历史回放**挡在输出之外：Reasonix 在
//! `session/resume` 后会把上一轮整段 transcript 重播一遍，不挡就会把旧答案拼进
//! 本次输出。本 crate 的解码器按相位收事件（只认握手完成之后的 `session/update`），
//! 且 `RunOutcome::output` 是"本轮 Text 事件拼接"—— 重播发生在 `session/prompt`
//! 之前，会被相位丢掉，所以**不需要**额外的开关。权限收窄
//! （`selectReasonixPermissionOption` / protected decision / 提问阻塞）与
//! `--workspace-only` 之外的 lease 冲突分类同样留给 M4，见 `docs/33` §11。

use std::path::Path;

use super::acp_core::{
    AcpAuth, AcpFlavor, AcpModelSelection, AcpPromptFields, AcpProvider, AcpResume,
    AcpResumeParams, AcpToolAliases,
};
use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use super::cli_core::CliCoreConfig;
use crate::adapter::LaunchRequest;
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字（也是默认可执行文件名）。
pub(crate) const LABEL: &str = "reasonix";

/// daemon 固定的策略开关（`reasonixACPLaunchArgs` 的后半段）。
const LAUNCH_ARGS: &[(&str, &str)] = &[
    ("--profile", "balanced"),
    ("--planner", "auto"),
    ("--sandbox-network", "auto"),
    ("--sandbox-bash", "auto"),
];

/// `reasonixACPLaunchArgs` 里的独立开关（顺序在 [`LAUNCH_ARGS`] 之后）。
const WORKSPACE_ONLY: &str = "--workspace-only";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`reasonixBlockedArgs`）。
///
/// 除 `acp` 之外全是上表那几个策略开关：把 `--sandbox-bash` 改成 `off` 或者
/// 摘掉 `--workspace-only`，就等于替 daemon 关掉了无人值守任务的边界。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("acp", ArgValueMode::Standalone),
    ("--model", ArgValueMode::WithValue),
    ("--profile", ArgValueMode::WithValue),
    ("--planner", ArgValueMode::WithValue),
    ("--sandbox-network", ArgValueMode::WithValue),
    ("--sandbox-bash", ArgValueMode::WithValue),
    ("--workspace-only", ArgValueMode::Standalone),
];

/// 已知取值参数（`acp` 子命令没有别的 daemon 认识的取值 flag）。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 ACP 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Reasonix,
    label: LABEL,
    resume: AcpResume::Resume,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    // Reasonix 的 effort 旋钮叫 `effort`（上游 `applyACPEffortOption`）。
    thinking_config: Some("effort"),
    tool_aliases: AcpToolAliases::Kimi,
    model_selection: AcpModelSelection::SetModel,
    session_meta_key: None,
    session_configs: &[],
    resume_params: AcpResumeParams::SessionAndCwd,
};

/// 组装 argv（`reasonixACPLaunchArgs` ++ 过滤后的 `extra_args`）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned()];
    for (flag, value) in LAUNCH_ARGS {
        args.push((*flag).to_owned());
        args.push((*value).to_owned());
    }
    args.push(WORKSPACE_ONLY.to_owned());
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// reasonix adapter。
#[derive(Debug)]
pub struct Reasonix {
    config: CliCoreConfig,
}

impl Reasonix {
    /// 默认配置：可执行文件 `reasonix`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(LABEL),
        }
    }

    /// 指定可执行文件。
    pub fn with_executable(executable: impl Into<std::path::PathBuf>) -> Self {
        Self {
            config: CliCoreConfig::new(executable),
        }
    }
}

impl Default for Reasonix {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Reasonix {
    fn flavor() -> &'static AcpFlavor {
        &FLAVOR
    }

    fn build_args(request: &LaunchRequest) -> Vec<String> {
        build_args(request)
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }
}

impl TestableAdapter for Reasonix {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "reasonix 0.4.2\n".to_owned(),
            expected_version: Some("0.4.2".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout(
                "reasonix-ses-1",
                "ok",
                false,
            ),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "reasonix exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_pins_the_sandbox_policy_before_custom_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("deepseek-v3")
            .with_thinking_level("high")
            .with_extra_args(["--verbose"]);
        assert_eq!(
            build_args(&request),
            vec![
                "acp",
                "--profile",
                "balanced",
                "--planner",
                "auto",
                "--sandbox-network",
                "auto",
                "--sandbox-bash",
                "auto",
                "--workspace-only",
                "--verbose",
            ]
        );
    }

    #[test]
    fn sandbox_policy_cannot_be_loosened_by_extra_args() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--sandbox-bash",
            "off",
            "--workspace-only",
            "--model",
            "other",
        ]);
        let args = build_args(&request);
        // `--planner auto` 也是 "auto"，所以不能数 "auto" 的个数，要按标志定位。
        for flag in ["--sandbox-network", "--sandbox-bash"] {
            let index = args
                .iter()
                .position(|arg| arg == flag)
                .unwrap_or_else(|| panic!("argv 里必须有 {flag}：{args:?}"));
            assert_eq!(args[index + 1], "auto", "{flag} 必须是加固后的取值");
            assert_eq!(
                args.iter().filter(|arg| *arg == flag).count(),
                1,
                "{flag} 不能被重复注入：{args:?}"
            );
        }
        assert_eq!(
            args.iter().filter(|arg| *arg == "--workspace-only").count(),
            1,
            "`--workspace-only` 同样不能被用户改写：{args:?}"
        );
        assert!(!args.contains(&"off".to_owned()));
        assert_eq!(
            args.iter().filter(|arg| *arg == "--model").count(),
            0,
            "--model 是封锁参数（模型走 set_model）"
        );
    }

    crate::adapter_conformance!(Reasonix);
}
