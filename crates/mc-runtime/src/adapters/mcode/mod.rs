//! `mcode`（`MiniMax` Code）adapter —— 上游 `server/pkg/agent/mcode.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `mcode.go` L92（`["acp"] ++ ExtraArgs ++ CustomArgs`） |
//! | `BLOCKED` | `mcodeBlockedArgs`（L19） |
//! | [`FLAVOR`] | `mcodeBackend.Execute` 的会话建/恢复两段（L236 起） |
//! | [`super::acp_core::AcpDecoder`] | 复用的 `hermesClient` |
//!
//! # 命令面
//!
//! `mcode acp`。`MiniMax` Code 把 Runtime / Session / 权限 / 问卷全托管在自己的
//! ACP server 里，public CLI 的 `acp` 子命令**没有任何选项**，唯一的子命令是交互式
//! `login`（在无人值守任务里跑不起来，所以进封锁表）。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 `session/load`，但当前公开的 ACP 面**声明 `loadSession: false`**
//!   —— 上游会在 `initialize` 结果里查到 `agentCapabilities.loadSession` 为假时直接
//!   以 `"mcode ACP does not support session loading; retry with a fresh session"`
//!   失败（并把会话标成 `ResumeRejected`，让 daemon 换新会话重试）。本 crate 不做
//!   能力闸门：**照样发 `session/load`**，让对端的错误走通用 `AgentError` 通道
//!   （结果同是"这个 run 失败"），重试编排留给 M4。见 `docs/33` §11。
//! * **不发** `session/set_model`：模型由 `MiniMax` Code 自己管
//!   ⇒ [`AcpModelSelection::Unsupported`]；
//! * 没有 effort 旋钮，也没有 `_meta`、没有固定配置链；
//! * `mcodeReaderDrainGrace = 2s` 与 `mcodeSessionStartupReadyDelay = 100ms`
//!   （上游在 `session/new` 前刻意等 100ms）都是 daemon 侧计时，属于 M3-3 看门狗，
//!   本片不做。
//!
//! # 有意不搬的部分
//!
//! 上游把"session/new 之前 ACP 进程就退了"这一类失败单独归因成一句长诊断
//! （`mcodeSessionStartupExited`，含升级建议）；本 crate 的**进程级失败**统一由
//! 退出码 + stderr 尾巴归因（与批 1 口径一致）。

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
pub(crate) const LABEL: &str = "mcode";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`mcodeBlockedArgs`）。
///
/// `login` / `-h` / `--help` 会把协议进程换成认证或帮助界面；`--region` 是 CLI
/// 自己的区域开关，由 operator 的安装决定，不能让 per-agent 的 `custom_args` 改。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("acp", ArgValueMode::Standalone),
    ("login", ArgValueMode::Standalone),
    ("--region", ArgValueMode::WithValue),
    ("-h", ArgValueMode::Standalone),
    ("--help", ArgValueMode::Standalone),
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
    kind: AgentType::Mcode,
    label: LABEL,
    resume: AcpResume::Load,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
    // 模型由 `MiniMax` Code 自己管（公开 ACP 面不暴露会话级模型选择）。
    model_selection: AcpModelSelection::Unsupported,
    session_meta_key: None,
    session_configs: &[],
    resume_params: AcpResumeParams::SessionAndCwd,
};

/// 组装 argv（`["acp"] ++ filtered`）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// mcode adapter。
#[derive(Debug)]
pub struct Mcode {
    config: CliCoreConfig,
}

impl Mcode {
    /// 默认配置：可执行文件 `mcode`（走 `PATH`）。
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

impl Default for Mcode {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Mcode {
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

impl TestableAdapter for Mcode {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "mcode 1.4.0\n".to_owned(),
            expected_version: Some("1.4.0".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout("mcode-ses-1", "ok", false),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "mcode exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_acp_subcommand_plus_custom_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("minimax-m2")
            .with_extra_args(["--verbose"]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "--verbose"],
            "模型 / cwd 都不进 argv"
        );
    }

    #[test]
    fn login_and_help_modes_cannot_be_smuggled_in() {
        let request = LaunchRequest::new("p")
            .with_extra_args(["login", "--help", "-h", "--region", "us", "pos"]);
        assert_eq!(build_args(&request), vec!["acp"]);
    }

    crate::adapter_conformance!(Mcode);
}
