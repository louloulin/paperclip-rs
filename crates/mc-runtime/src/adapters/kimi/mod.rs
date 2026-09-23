//! `kimi`（Moonshot `Kimi` Code CLI）adapter —— 上游 `server/pkg/agent/kimi.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `kimiBackend.Execute`（`kimi.go` L41 起，argv 在 L67） |
//! | `BLOCKED` | `kimiBlockedArgs`（L21） |
//! | [`FLAVOR`] | `kimi.go` 的 `initialize` / resume / `set_model` / thinking |
//! | [`super::acp_core::AcpDecoder`] | 复用 `hermesClient`（L218 起） |
//!
//! # 命令面
//!
//! `kimi acp`：`acp` 是这个 CLI 的**子命令**，也是唯一能让它说 ACP 的开关。
//! 上游注释还点明一件事：`--yolo` / `--auto-approve` 是**根命令**的 flag，
//! `acp` 子命令根本不认，所以非交互授权不走 argv，而是在 ACP 层由
//! `session/request_permission` 的应答完成（见
//! [`super::acp_core::client`] 的权限选择）。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 `session/resume`（`kiro`/`traecli`/`grok` 用 `session/load`）；
//! * 推理等级走 `session/set_config_option {configId: "thinking"}`（L357），
//!   **失败不致命**：上游只记 warning，照常发 prompt（L363）；
//! * 不宣告 `terminal` 能力（上游 kimi 是唯一宣告它的；本片一律不宣告，见
//!   `docs/33-M3-ADAPTERS.md` §6）；
//! * 工具名在 hermes 归一之后再过一遍 `kimiToolNameFromTitle`（L169）——见
//!   [`super::acp_core::AcpToolAliases::Kimi`]。
//!
//! # 有意不搬的部分
//!
//! 上游在 stdout 之外还扫 stderr 嗅探 provider 错误（`newACPProviderErrorSniffer`）
//! 与会话恢复被拒的重试分类（`classifyACPResumeFailure` / `ResumeRejected`）。
//! 这些属于"失败归因 + 重试编排"，与批 1 的口径一致：本片只把**进程级**失败
//! 交给退出码与 stderr 尾巴，恢复被拒不自动重试（`docs/33` §6.2）。

use std::path::Path;

use super::acp_core::{
    AcpAuth, AcpFlavor, AcpPromptFields, AcpProvider, AcpResume, AcpToolAliases,
};
use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use super::cli_core::CliCoreConfig;
use crate::adapter::LaunchRequest;
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字（也是默认可执行文件名）。
pub(crate) const LABEL: &str = "kimi";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`kimiBlockedArgs`）。
///
/// 只有 `acp` 一项：它是协议子命令，用户把它换掉或重复一次都会毁掉
/// daemon↔Kimi 的 ACP 契约。
const BLOCKED: &[(&str, ArgValueMode)] = &[("acp", ArgValueMode::Standalone)];

/// 已知取值参数。`acp` 子命令自身没有 daemon 认识的取值 flag，未知长参数由
/// [`filter_extra_args`] 的兜底规则保留其值。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 JSON-RPC 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Kimi,
    label: LABEL,
    resume: AcpResume::Resume,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    // kimi.go L357：唯一的 ACP 原生推理等级旋钮。
    thinking_config: Some("thinking"),
    tool_aliases: AcpToolAliases::Kimi,
};

/// 组装 argv（上游 `kimi.go` L67：`["acp"] ++ custom_args`）。
///
/// 模型与思考等级都不进 argv：模型走 `session/set_model`，思考等级走
/// `session/set_config_option`（两者都要等会话建好）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// kimi adapter。
#[derive(Debug)]
pub struct Kimi {
    config: CliCoreConfig,
}

impl Kimi {
    /// 默认配置：可执行文件 `kimi`（走 `PATH`）。
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

impl Default for Kimi {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Kimi {
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

impl TestableAdapter for Kimi {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "kimi 1.9.0\n".to_owned(),
            expected_version: Some("1.9.0".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout("kimi-ses-1", "ok", false),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "kimi exploded".to_owned(),
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
            .with_cwd("/work/task")
            .with_model("kimi-k2")
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
        let request = LaunchRequest::new("p").with_extra_args(["acp", "run", "--yolo"]);
        let args = build_args(&request);
        assert_eq!(args.iter().filter(|arg| *arg == "acp").count(), 1);
        assert!(args.contains(&"--yolo".to_owned()), "未知长参数原样保留");
        assert!(!args.contains(&"run".to_owned()), "位置参数被剔除");
    }

    crate::adapter_conformance!(Kimi);
}
