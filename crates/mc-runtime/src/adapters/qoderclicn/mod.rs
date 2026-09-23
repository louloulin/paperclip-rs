//! `qoderclicn`（Qoder CLI，国内版）adapter —— 上游 `server/pkg/agent/qoder.go`。
//!
//! 与 [`super::qoder`] **同一个上游 backend**，argv / 封锁表 / resume / prompt
//! 字段 / 工具名表全部相同（见 [`super::qoder_family`]）；区别只有两处：
//!
//! 1. 默认可执行文件名 `qoderclicn`（`qoder.go` 的 `qoderDefaultBinary`：按
//!    providerType 选 binary，两个 binary **不是**同一个文件）；
//! 2. [`catalog::AgentType`] 的 key 与 `AgentType::ALL` 的位置。
//!
//! # 为什么值得单独一个 adapter
//!
//! 国内版有独立的登录态与镜像源（同一份 ACP 客户端、不同的 credential 存储），
//! 上游也按两个 providerType 注册。合成一个会让"跑的是哪个 binary"变成隐式
//! 全局状态，也会让 `runtime_profiles` 无法按 provider 配环境变量。

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
pub(crate) const LABEL: &str = "qoderclicn";

/// 本 provider 的 ACP 差异表（除 `kind` / `label` 外与 [`super::qoder`] 逐字段相同）。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::QoderCliCn,
    label: LABEL,
    resume: AcpResume::Resume,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
};

/// 组装 argv（与 [`super::qoder::build_args`] 逐字相同）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    qoder_family::build_args(request)
}

/// qoderclicn adapter。
#[derive(Debug)]
pub struct QoderCliCn {
    config: CliCoreConfig,
}

impl QoderCliCn {
    /// 默认配置：可执行文件 `qoderclicn`（走 `PATH`）。
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

impl Default for QoderCliCn {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for QoderCliCn {
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

impl TestableAdapter for QoderCliCn {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "qoderclicn 0.8.7\n".to_owned(),
            expected_version: Some("0.8.7".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout(
                "qoderclicn-ses-1",
                "ok",
                false,
            ),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "qoderclicn exploded".to_owned(),
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
            .with_resume_session("ses-old")
            .with_extra_args(["--verbose"]);
        assert_eq!(build_args(&request), vec!["--yolo", "--acp", "--verbose"]);
    }

    #[test]
    fn the_two_qoder_binaries_are_distinct_executables() {
        // 上游 `qoderDefaultBinary`：两个 providerType → 两个 binary 名。
        assert_eq!(
            QoderCliCn::new().config().executable,
            std::path::Path::new(LABEL)
        );
        assert_ne!(
            LABEL,
            super::super::qoder::LABEL,
            "两个 binary 不是同一个可执行文件"
        );
    }

    crate::adapter_conformance!(QoderCliCn);
}
