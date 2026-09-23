//! `codex`（`OpenAI` `Codex` CLI）adapter —— 上游 `server/pkg/agent/codex.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildCodexArgs`（`codex.go` L361） |
//! | `BLOCKED` | `codexBlockedArgs`（L32） |
//! | [`CodexDecoder`] | `codexClient` 的 `handleResponse` / `handleRawNotification` / `handleItemNotification` |
//! | 运行骨架 | [`super::cli_core`]（`Execute` 的 spawn / stdout 循环 / 终态归因） |
//!
//! # 为什么 `codex` 是 `AppServer` 族
//!
//! 它跑的是 `codex app-server --listen stdio://`：一条 stdin/stdout 上的
//! **JSON-RPC 2.0** 长连接，而不是逐行事件流。因此 prompt 不是启动时写下去的，
//! 而是 `thread/start` 的响应带回 `threadId` 之后，由解码器排一个 `turn/start`
//! 帧写回去（见 [`super::codex::stream`] 的模块文档）。
//!
//! # 与上游的差异
//!
//! 1. **不写 `$CODEX_HOME/config.toml`**：上游为了 MCP 与沙箱设置会生成一份 per-task
//!    配置，本 crate 的 [`LaunchRequest`] 里没有 MCP/沙箱字段，因此什么都不写，
//!    用户的 `~/.codex/config.toml` 保持权威（与 `-c` 覆盖的 last-wins 语义一致）。
//! 2. **不做 `--enable fast_mode` 的 service tier 注入**：那是上游多租户的运营选项，
//!    不属于本片范围。

mod stream;

pub(crate) use stream::CodexDecoder;

use std::path::Path;

use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "codex";

/// 守护进程管理、不允许被 `extra_args` 覆盖的参数（`codexBlockedArgs`）。
///
/// `--listen stdio://` 是 daemon↔CLI 的通信通道；改掉它 daemon 就失联了。
const BLOCKED: &[(&str, ArgValueMode)] = &[("--listen", ArgValueMode::WithValue)];

/// 已知的取值参数（`-c` 之外大多是"改配置"，都带值）。
const MODES: &[(&str, ArgValueMode)] = &[
    ("-c", ArgValueMode::WithValue),
    ("--config", ArgValueMode::WithValue),
    ("--enable", ArgValueMode::WithValue),
    ("--disable", ArgValueMode::WithValue),
    ("-p", ArgValueMode::WithValue),
    ("--profile", ArgValueMode::WithValue),
    ("-m", ArgValueMode::WithValue),
    ("--model", ArgValueMode::WithValue),
    ("-s", ArgValueMode::WithValue),
    ("--sandbox", ArgValueMode::WithValue),
    ("-a", ArgValueMode::WithValue),
    ("--ask-for-approval", ArgValueMode::WithValue),
    ("-C", ArgValueMode::WithValue),
    ("--cd", ArgValueMode::WithValue),
];

/// `extra_args` 过滤策略（prompt 走 JSON-RPC ⇒ 位置参数必须剔除）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（`buildCodexArgs`：固定传输前缀 + 用户参数）。
///
/// `--model` / `--effort` 不在这里 —— 它们是 `thread/start` / `turn/start` 的
/// 请求字段（上游同款：`applyCodexReasoningEffort`）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = ["app-server", "--listen", "stdio://"]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// Codex adapter。
#[derive(Debug)]
pub struct Codex {
    config: CliCoreConfig,
}

impl Codex {
    /// 默认配置：可执行文件 `codex`（走 `PATH`）。
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

    /// 当前配置。
    pub fn config(&self) -> &CliCoreConfig {
        &self.config
    }
}

impl Default for Codex {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Codex {
    fn kind(&self) -> AgentType {
        AgentType::Codex
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::JsonRpc,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::AppServer,
                streaming: true,
                // codex 的 app-server 不上报推理增量（上游 `codex.go` 里没有
                // `MessageThinking`），因此这里如实自报 false。
                thinking: false,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            // JSON-RPC：对端先退出导致 EPIPE 是常态，退出码才是权威。
            prompt_write_is_fatal: false,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(CodexDecoder::new(
            request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
            request.prompt.clone(),
            request.cwd.clone(),
            request.thinking_level.clone(),
            request.resume_session.clone(),
        ))
    }
}

impl TestableAdapter for Codex {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "codex-cli 0.147.0\n".to_owned(),
            expected_version: Some("0.147.0".to_owned()),
            success_stdout: concat!(
                r#"{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th_conf_1"}}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":"turn_conf_1"}}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":"o"}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":"k"}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"item_1","type":"agentMessage","text":"ok"}}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"turnId":"turn_conf_1","tokenUsage":{"total":{"inputTokens":10,"outputTokens":5,"cachedInputTokens":1,"cacheWriteInputTokens":0},"last":{}}}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"turn_conf_1","status":"completed"}}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(16),
            junk_stdout: concat!(
                "codex: warning banner\n",
                r#"{"jsonrpc":"2.0","method":"unknown/future","params":{}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"itemId":"i9","delta":"ok"}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "codex exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_transport_prefix_plus_user_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("gpt-5-codex")
            .with_extra_args(["-c", "model_reasoning_summary=detailed"]);
        assert_eq!(
            build_args(&request),
            vec![
                "app-server",
                "--listen",
                "stdio://",
                "-c",
                "model_reasoning_summary=detailed",
            ]
        );
    }

    #[test]
    fn listen_flag_cannot_be_overridden_and_positionals_are_stripped() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--listen",
            "tcp://0.0.0.0:1",
            "注入的位置参数",
        ]);
        let args = build_args(&request);
        assert_eq!(
            args.iter().filter(|arg| *arg == "--listen").count(),
            1,
            "{args:?}"
        );
        assert!(!args.iter().any(|arg| arg.starts_with("tcp://")));
        assert!(!args.iter().any(|arg| arg == "注入的位置参数"));
    }

    #[test]
    fn model_and_effort_are_not_argv_flags() {
        let request = LaunchRequest::new("p")
            .with_model("gpt-5-codex")
            .with_thinking_level("high");
        let args = build_args(&request);
        assert!(!args.iter().any(|arg| arg == "--model" || arg == "--effort"));
    }

    crate::adapter_conformance!(Codex);
}
