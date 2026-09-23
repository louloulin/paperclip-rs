//! `kiro`（AWS Kiro CLI）adapter —— 上游 `server/pkg/agent/kiro.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `kiroBackend.Execute`（`kiro.go` L64：`["acp", "--trust-all-tools"] ++ custom_args`） |
//! | `BLOCKED` | `kiroBlockedArgs`（L26） |
//! | [`FLAVOR`] | kiro 的 resume / prompt 字段 / 工具名表 |
//! | [`super::acp_core::AcpDecoder`] | 复用 `hermesClient` |
//!
//! # 命令面
//!
//! 可执行文件是 **`kiro-cli`**（不是 `kiro`），协议子命令是 `acp`，外加
//! `--trust-all-tools` 让 CLI 层的工具闸门放行（ACP 层的
//! `session/request_permission` 另由共享客户端自动应答）。
//! `kiro.go` L22 的注释还点明：Kiro CLI 2.1.1 里 `-a` 是 `--trust-all-tools`
//! 的短写、**不是** `--agent`，所以 `-a` 进封锁表而 `--agent` 留给用户。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 **`session/load`**（kimi/qoder 是 `session/resume`）；
//! * `session/prompt` 的块里 **`prompt` + `content` 两个键都发**（上游
//!   `kiro.go` L374 起两个字段都给值，见 [`AcpPromptFields::PromptAndContent`]）；
//! * 工具名后处理用 kiro 自己那张表（比 kimi 的多 `code` / `todo list` 两个别名）；
//! * 不宣告 `terminal` 能力（与批 2 其余 provider 一致）。

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

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "kiro";
/// 默认可执行文件名（`AgentType::Kiro.cli_command()` 同源，见 `catalog.rs`）。
const EXECUTABLE: &str = "kiro-cli";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`kiroBlockedArgs`）。
///
/// `acp` 是协议子命令；`-a` / `--trust-all-tools` 是 CLI 层工具闸门；
/// `--trust-tools` 吃值（用户给一部分工具授权也算改契约）。`--agent` 故意
/// **不**封锁：它是用户选自定义 Kiro agent 的正当入口。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("acp", ArgValueMode::Standalone),
    ("-a", ArgValueMode::Standalone),
    ("--trust-all-tools", ArgValueMode::Standalone),
    ("--trust-tools", ArgValueMode::WithValue),
];

/// 已知取值参数（`--trust-tools` 已在封锁表里，这里只留 daemon 认识的选项）。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 JSON-RPC 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Kiro,
    label: LABEL,
    // kiro.go：resume 走 session/load。
    resume: AcpResume::Load,
    auth: AcpAuth::None,
    // kiro.go L374：prompt 块里 prompt 与 content 两个键都发。
    prompt_fields: AcpPromptFields::PromptAndContent,
    // 推理等级无 ACP 旋钮（上游不传 thinking）。
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kiro,

    model_selection: AcpModelSelection::SetModel,
    session_meta_key: None,
    session_configs: &[],
    resume_params: AcpResumeParams::SessionAndCwd,
};

/// 组装 argv（上游 `kiro.go` L64：`["acp", "--trust-all-tools"] ++ custom_args`）。
///
/// 模型 / 思考等级 / 恢复会话都不进 argv：模型走 `session/set_model`，
/// 恢复会话走 `session/load`。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned(), "--trust-all-tools".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// kiro adapter。
#[derive(Debug)]
pub struct Kiro {
    config: CliCoreConfig,
}

impl Kiro {
    /// 默认配置：可执行文件 `kiro-cli`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(EXECUTABLE),
        }
    }

    /// 指定可执行文件。
    pub fn with_executable(executable: impl Into<std::path::PathBuf>) -> Self {
        Self {
            config: CliCoreConfig::new(executable),
        }
    }
}

impl Default for Kiro {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Kiro {
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

impl TestableAdapter for Kiro {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "kiro-cli 2.1.1\n".to_owned(),
            expected_version: Some("2.1.1".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout("kiro-ses-1", "ok", false),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "kiro exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_acp_plus_the_tool_trust_flag() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/work/task")
            .with_model("claude-sonnet-4")
            .with_thinking_level("high")
            .with_resume_session("ses-old")
            .with_extra_args(["--agent", "reviewer"]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "--trust-all-tools", "--agent", "reviewer"],
            "模型 / 思考等级 / 恢复会话都不进 argv，--agent 是用户正当入口"
        );
    }

    #[test]
    fn the_tool_gate_flags_cannot_be_shadowed() {
        let request = LaunchRequest::new("p").with_extra_args([
            "acp",
            "-a",
            "--trust-all-tools",
            "--trust-tools",
            "read,write",
        ]);
        assert_eq!(build_args(&request), vec!["acp", "--trust-all-tools"]);
    }

    crate::adapter_conformance!(Kiro);
}
