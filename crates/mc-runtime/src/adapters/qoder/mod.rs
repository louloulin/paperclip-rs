//! `qoder`（Qoder CLI，海外版）adapter —— 上游 `server/pkg/agent/qoder.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`super::qoder_family::build_args`] | `qoderBackend.Execute`（`qoder.go` L100） |
//! | [`super::qoder_family::BLOCKED`] | `qoderBlockedArgs`（L20） |
//! | [`FLAVOR`] | qoder 的 resume / prompt 字段 / 工具名表 |
//! | [`super::acp_core::AcpDecoder`] | 复用 `hermesClient` |
//!
//! # 命令面
//!
//! 可执行文件 `qodercli`，argv `--yolo --acp`：Qoder 进 ACP 模式用的是**全局**
//! flag，不是 `acp` 子命令（这点与 kimi/kiro/traecli 不同）。
//!
//! # 与 `qoderclicn` 的关系
//!
//! 参数面 100% 共用（见 [`super::qoder_family`]）；唯一区别是默认可执行文件名
//! 与 provider key。上游正是同一个 backend 结构体按 `providerType` 换
//! `defaultExecutable`。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 `session/resume`（与 kimi 同；kiro/traecli/grok 是 `session/load`）；
//! * 工具名后处理用 kimi 那张表（`qoder.go` 复用 `kimiToolNameFromTitle`）；
//! * 不宣告 `terminal` 能力（与批 2 其余 provider 一致）。

use std::path::Path;

use super::acp_core::{
    AcpAuth, AcpFlavor, AcpPromptFields, AcpProvider, AcpResume, AcpToolAliases,
};
use super::cli_core::CliCoreConfig;
use super::qoder_family;
use crate::adapter::LaunchRequest;
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字（也是默认可执行文件名）。
pub(crate) const LABEL: &str = "qoder";

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Qoder,
    label: LABEL,
    resume: AcpResume::Resume,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
};

/// 组装 argv（见 [`super::qoder_family::build_args`]）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    qoder_family::build_args(request)
}

/// qoder adapter。
#[derive(Debug)]
pub struct Qoder {
    config: CliCoreConfig,
}

impl Qoder {
    /// 默认配置：可执行文件 `qodercli`（走 `PATH`）。
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

impl Default for Qoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Qoder {
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

impl TestableAdapter for Qoder {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "qodercli 0.8.7\n".to_owned(),
            expected_version: Some("0.8.7".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout("qoder-ses-1", "ok", false),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "qoder exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_two_global_flags_plus_custom_args() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/work/task")
            .with_model("qwen3-coder")
            .with_thinking_level("high")
            .with_resume_session("ses-old")
            .with_extra_args(["--verbose"]);
        assert_eq!(build_args(&request), vec!["--yolo", "--acp", "--verbose"]);
    }

    #[test]
    fn qoder_and_qoderclicn_share_the_same_argv_shape() {
        let request = LaunchRequest::new("p").with_extra_args(["--x"]);
        assert_eq!(
            build_args(&request),
            super::super::qoderclicn::build_args(&request)
        );
    }

    crate::adapter_conformance!(Qoder);
}
