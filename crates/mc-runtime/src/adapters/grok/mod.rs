//! `grok`（xAI Grok Build CLI）adapter —— 上游 `server/pkg/agent/grok.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `grokBackend.Execute`（`grok.go` L136 起） |
//! | `BLOCKED` | `grokBlockedArgs`（L22） |
//! | [`FLAVOR`] | grok 的 resume / 认证 / prompt 字段 / 工具名表 |
//! | [`super::acp_core::AcpDecoder`] | 复用 `hermesClient` |
//!
//! # 命令面（顺序有讲究）
//!
//! ```text
//! grok --no-auto-update agent --always-approve [--effort <level>] <custom_args…> stdio
//! ```
//!
//! * `--no-auto-update` 是**全局** flag，必须排在 `agent` 子命令**之前**
//!   （xAI 建议无人值守/CI 打开它，免得后台更新检查打扰任务）；
//! * `--always-approve` 与 `--effort` 属于 `agent` 子命令；
//! * `stdio` 是**传输子命令**，排在最后（上游注释：flags such as
//!   `--always-approve` and `--effort` belong on the `agent` command,
//!   before the transport subcommand）。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 唯一需要 `authenticate` 的 provider：`initialize` 之后按
//!   [`AcpAuth::XaiApiKey`] 从对端广告的 `authMethods` 里挑一个
//!   （有 `XAI_API_KEY` 且对端给了 `xai.api_key` 就选它，否则 `cached_token`）；
//! * 推理等级走 argv 的 `--effort`（**不**是 ACP 的
//!   `session/set_config_option`，那是 kimi 独占的旋钮）；
//! * 恢复会话用 `session/load`，工具名后处理用 kimi 那张表。

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
pub(crate) const LABEL: &str = "grok";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`grokBlockedArgs`，逐条对齐）。
///
/// 三类：**模式开关**（`agent`/`stdio`/`headless`/`serve`/`leader`：换一个就毁掉
/// ACP 契约）、**daemon 拥有的授权与自更新**（`--always-approve`/`--yolo`/
/// `--no-auto-update`/`--no-alt-screen`）、**模型与思考的下发点**（`-m`/`--model`/
/// `--effort`/`--reasoning-effort`：模型走 `session/set_model`、思考走 `--effort`，
/// 不能让 `custom_args` 再插一份）。会话相关（`-r`/`--resume`/`-c`/`--continue`/
/// `-s`/`--session-id`/`--fork-session`）同样封锁：恢复走 `session/load`。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("agent", ArgValueMode::Standalone),
    ("stdio", ArgValueMode::Standalone),
    ("headless", ArgValueMode::Standalone),
    ("serve", ArgValueMode::Standalone),
    ("leader", ArgValueMode::Standalone),
    ("--always-approve", ArgValueMode::Standalone),
    ("--yolo", ArgValueMode::Standalone),
    ("--no-auto-update", ArgValueMode::Standalone),
    ("--no-alt-screen", ArgValueMode::Standalone),
    ("-p", ArgValueMode::Standalone),
    ("--print", ArgValueMode::Standalone),
    ("--single", ArgValueMode::WithValue),
    ("--output-format", ArgValueMode::WithValue),
    ("--permission-mode", ArgValueMode::WithValue),
    ("-m", ArgValueMode::WithValue),
    ("--model", ArgValueMode::WithValue),
    ("--reasoning-effort", ArgValueMode::WithValue),
    ("--effort", ArgValueMode::WithValue),
    ("-r", ArgValueMode::WithValue),
    ("--resume", ArgValueMode::WithValue),
    ("-c", ArgValueMode::Standalone),
    ("--continue", ArgValueMode::Standalone),
    ("-s", ArgValueMode::WithValue),
    ("--session-id", ArgValueMode::WithValue),
    ("--system-prompt-override", ArgValueMode::WithValue),
    ("--cwd", ArgValueMode::WithValue),
    ("-w", ArgValueMode::OptionalValue),
    ("--worktree", ArgValueMode::OptionalValue),
    ("--ref", ArgValueMode::WithValue),
    ("--fork-session", ArgValueMode::Standalone),
];

/// 已知取值参数（grok 的取值项全在封锁表里）。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 JSON-RPC 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Grok,
    label: LABEL,
    resume: AcpResume::Load,
    // 批 2 里唯一要认证的 provider（`selectGrokAuthMethod`）。
    auth: AcpAuth::XaiApiKey,
    prompt_fields: AcpPromptFields::Prompt,
    // 思考等级走 argv 的 `--effort`，不占 ACP 的 config 通道。
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
};

/// 组装 argv（上游 `grok.go` L136 起）。
///
/// 顺序不可换：全局 flag → `agent` → `--always-approve` → `--effort` →
/// `custom_args` → `stdio`（传输子命令必须在最后）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--no-auto-update".to_owned(),
        "agent".to_owned(),
        "--always-approve".to_owned(),
    ];
    if let Some(level) = request.thinking_level.as_deref().filter(|l| !l.is_empty()) {
        // 上游直接把 `opts.ThinkingLevel` 透传成 `--effort` 的值（不做白名单校验：
        // 等级词表由 provider 自己演进，daemon 只负责传）。
        args.push("--effort".to_owned());
        args.push(level.to_owned());
    }
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args.push("stdio".to_owned());
    args
}

/// grok adapter。
#[derive(Debug)]
pub struct Grok {
    config: CliCoreConfig,
}

impl Grok {
    /// 默认配置：可执行文件 `grok`（走 `PATH`）。
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

impl Default for Grok {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Grok {
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

impl TestableAdapter for Grok {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "grok 0.2.3\n".to_owned(),
            expected_version: Some("0.2.3".to_owned()),
            // `with_auth = true`：grok 是唯一会先 `authenticate` 的 provider，
            // 回放里那条 `authMethods: [{id: "cached_token"}]` 让它**不依赖**
            // 环境里有没有 `XAI_API_KEY`。
            success_stdout: super::acp_core::conformance_success_stdout("grok-ses-1", "ok", true),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "grok exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_puts_the_transport_subcommand_last() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/work/task")
            .with_model("grok-code-fast-1")
            .with_thinking_level("high")
            .with_resume_session("ses-old")
            .with_extra_args(["--verbose"]);
        assert_eq!(
            build_args(&request),
            vec![
                "--no-auto-update",
                "agent",
                "--always-approve",
                "--effort",
                "high",
                "--verbose",
                "stdio"
            ],
            "模型 / 恢复会话不进 argv；stdio 必须最后"
        );
    }

    #[test]
    fn without_a_thinking_level_effort_is_omitted() {
        let request = LaunchRequest::new("p");
        assert_eq!(
            build_args(&request),
            vec!["--no-auto-update", "agent", "--always-approve", "stdio"]
        );
    }

    #[test]
    fn mode_and_session_flags_cannot_be_shadowed() {
        let request = LaunchRequest::new("p").with_extra_args([
            "headless",
            "stdio",
            "--no-auto-update",
            "--model",
            "gpt",
            "-r",
            "old",
            "--fork-session",
            "--keep",
        ]);
        assert_eq!(
            build_args(&request),
            vec![
                "--no-auto-update",
                "agent",
                "--always-approve",
                "--keep",
                "stdio"
            ]
        );
    }

    crate::adapter_conformance!(Grok);
}
