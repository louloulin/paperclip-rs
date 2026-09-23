//! `hermes`（Hermes CLI）adapter —— 上游 `server/pkg/agent/hermes.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `hermesCLIArgsFrom`（`hermes.go` L147：`["acp"] ++ filtered`） |
//! | `BLOCKED` | `hermesBlockedArgs`（L37） |
//! | [`FLAVOR`] | `hermesBackend` 的 `initialize` / `buildHermesSessionParams` / effort |
//! | [`super::acp_core::AcpDecoder`] | `hermesClient`（L218 起，就是这个文件的 3322 行主体） |
//!
//! # 命令面
//!
//! `hermes acp` —— `acp` 是协议子命令，[`super::acp_core`] 那份共享骨架本来就是
//! 从 hermes 的客户端抽出来的，所以这里的差异表最"少".
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 `session/resume`；
//! * 模型**塞进 `session/new` 的 params**，不发 `session/set_model`（见
//!   [`AcpModelSelection::SessionParam`]，偏离说明在 `docs/33` §11）；
//! * 推理等级走 `session/set_config_option {configId: "thought_level"}`
//!   （上游经 `applyACPEffortOption` 发现式下发，本 crate 用静态 id，同批 2 取舍）。
//!
//! # 有意不搬的部分
//!
//! 上游对 `-p` / `--profile` 有一套**只在 daemon 建了 per-task overlay 时**才生效的
//! 剥离逻辑（`StripHermesProfileSelectors`，L195 起）：它要先按 Hermes 自己的
//! argv 扫描规则认出「哪一个 token 是 profile 选择」，再把整个选择从 argv 里摘掉。
//! 这套逻辑的存在前提是 daemon 侧的 overlay 与 `HERMES_HOME`（`docs/33` §2 的
//! 范围之外），本片没有 overlay ⇒ **不剥离**，`-p` / `--profile` 按普通未知参数
//! 原样透传（与上游"没有 overlay 的任务保持原行为"一致）。
//!
//! 会话恢复被拒的错误分类（`classifyACPResumeFailure` / `ResumeRejected`）同样留给
//! M4 的重试编排，见 `docs/33` §6.2。

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
pub(crate) const LABEL: &str = "hermes";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`hermesBlockedArgs`）。
///
/// 只有 `acp`：它是协议子命令。上游特意**不**在这里放 `-p` / `--profile`
/// （没有 overlay 的任务必须原样透传，注释见 `hermes.go` L27-35）。
const BLOCKED: &[(&str, ArgValueMode)] = &[("acp", ArgValueMode::Standalone)];

/// 已知取值参数（`acp` 子命令自身没有 daemon 认识的取值 flag）。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 ACP 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Hermes,
    label: LABEL,
    resume: AcpResume::Resume,
    // initialize 不带 authMethods ⇒ 不走 authenticate（上游 L2198 同解）。
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    // hermes 的 effort 旋钮是 `thought_level`（上游 `applyACPEffortOption`）。
    thinking_config: Some("thought_level"),
    tool_aliases: AcpToolAliases::Kimi,
    // 模型下发走 `session/new` 参数（恢复会话保持原模型）。
    model_selection: AcpModelSelection::SessionParam,
    session_meta_key: None,
    session_configs: &[],
    resume_params: AcpResumeParams::SessionAndCwd,
};

/// 组装 argv（上游 `hermesCLIArgsFrom`：`["acp"] ++ filtered`）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// hermes adapter。
#[derive(Debug)]
pub struct Hermes {
    config: CliCoreConfig,
}

impl Hermes {
    /// 默认配置：可执行文件 `hermes`（走 `PATH`）。
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

impl Default for Hermes {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Hermes {
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

impl TestableAdapter for Hermes {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "hermes 0.5.0\n".to_owned(),
            expected_version: Some("0.5.0".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout(
                "hermes-ses-1",
                "ok",
                false,
            ),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "hermes exploded".to_owned(),
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
            .with_model("gpt-5.1")
            .with_thinking_level("high")
            .with_resume_session("ses-old")
            .with_extra_args(["--debug"]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "--debug"],
            "模型 / 思考等级 / 恢复会话都不进 argv"
        );
    }

    #[test]
    fn the_acp_subcommand_cannot_be_shadowed() {
        let request = LaunchRequest::new("p").with_extra_args(["acp", "run"]);
        let args = build_args(&request);
        assert_eq!(args.iter().filter(|arg| *arg == "acp").count(), 1);
        assert!(!args.contains(&"run".to_owned()));
    }

    /// 没有 overlay 时 profile 选择**不被剥离**（上游的剥离只在 overlay 存在时）。
    ///
    /// 注意取值形式：crate 级的位置参数剔除（防 prompt 注入）会吃掉空格分隔的值，
    /// 所以只有内联写法能原样到达 CLI —— 见 `docs/33` §11 的偏离表。
    #[test]
    fn profile_selection_passes_through_when_there_is_no_overlay() {
        let request = LaunchRequest::new("p").with_extra_args(["--profile=research"]);
        assert_eq!(build_args(&request), vec!["acp", "--profile=research"]);
        assert_eq!(
            build_args(&LaunchRequest::new("p").with_extra_args(["-p", "research"])),
            vec!["acp", "-p"],
            "空格分隔的值会被位置参数剔除吃掉"
        );
    }

    crate::adapter_conformance!(Hermes);
}
