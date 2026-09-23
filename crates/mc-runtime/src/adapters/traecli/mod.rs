//! `traecli`（字节 TRAE CLI）adapter —— 上游 `server/pkg/agent/traecli.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `traecliBackend.Execute`（`traecli.go` L110：`["acp", "serve", "--yolo"] ++ custom_args`） |
//! | `BLOCKED` | `traecliBlockedArgs`（L16） |
//! | [`FLAVOR`] | traecli 的 resume / prompt 字段 / 工具名表 |
//! | [`super::acp_core::AcpDecoder`] | 复用 `hermesClient` |
//!
//! # 命令面
//!
//! 可执行文件 `traecli`，**两个 token 的子命令 + 一个动作**：`acp serve`，
//! 外加 daemon 拥有的 `--yolo`（官方 CLI 默认把非只读工具挡在权限确认后面，
//! 无人值守必须绕过）。
//!
//! 上游注释特别澄清过一件事：这是**官方** TRAE CLI（`https://docs.trae.cn/cli`），
//! **不是**开源项目 `bytedance/trae-agent` 的 `trae-cli`（那个没有 ACP 传输）。
//! 别按开源仓库的参数写 argv。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 **`session/load`**（kimi/qoder 是 `session/resume`）；
//! * 工具名后处理用 kimi 那张表；
//! * 不宣告 `terminal` 能力（与批 2 其余 provider 一致）。

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
pub(crate) const LABEL: &str = "traecli";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`traecliBlockedArgs`）。
///
/// `acp` + `serve` 是协议子命令/动作；`-y`/`--yolo` 由 daemon 拥有；
/// `-p`/`--print`/`--output-format` 会把 CLI 切到 print 模式、毁掉 ACP 契约；
/// `--permission-mode` 是 `--yolo` 的手工版本，一并不许改。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("acp", ArgValueMode::Standalone),
    ("serve", ArgValueMode::Standalone),
    ("-y", ArgValueMode::Standalone),
    ("--yolo", ArgValueMode::Standalone),
    ("-p", ArgValueMode::Standalone),
    ("--print", ArgValueMode::Standalone),
    ("--output-format", ArgValueMode::WithValue),
    ("--permission-mode", ArgValueMode::WithValue),
];

/// 已知取值参数（daemon 认识的选项都在封锁表里）。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 JSON-RPC 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::TraeCli,
    label: LABEL,
    resume: AcpResume::Load,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
};

/// 组装 argv（上游 `traecli.go` L110）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned(), "serve".to_owned(), "--yolo".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// traecli adapter。
#[derive(Debug)]
pub struct TraeCli {
    config: CliCoreConfig,
}

impl TraeCli {
    /// 默认配置：可执行文件 `traecli`（走 `PATH`）。
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

impl Default for TraeCli {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for TraeCli {
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

impl TestableAdapter for TraeCli {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "traecli 0.4.2\n".to_owned(),
            expected_version: Some("0.4.2".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout("trae-ses-1", "ok", false),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "traecli exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_acp_serve_subcommand_plus_custom_args() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/work/task")
            .with_model("doubao-1.5-pro")
            .with_resume_session("ses-old")
            .with_extra_args(["--verbose"]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "serve", "--yolo", "--verbose"]
        );
    }

    #[test]
    fn print_mode_switches_cannot_be_smuggled_in() {
        let request = LaunchRequest::new("p").with_extra_args([
            "acp",
            "serve",
            "-y",
            "--yolo",
            "-p",
            "--print",
            "--output-format",
            "json",
            "--permission-mode",
            "acceptEdits",
            "--keep",
        ]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "serve", "--yolo", "--keep"]
        );
    }

    crate::adapter_conformance!(TraeCli);
}
