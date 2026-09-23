//! `claude`（Anthropic Claude Code）adapter —— 上游 `server/pkg/agent/claude.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildClaudeArgs`（`claude.go` L730） |
//! | `BLOCKED` | `claudeBlockedArgs`（L715） |
//! | [`ClaudeStreamDecoder`] | `handleAssistant` / `handleUser` / `handleResult`（走 [`super::claude_family`]，与 codebuddy 共用） |
//! | 运行骨架 | [`super::cli_core`]（`Execute` 的 spawn / stdout 循环 / 终态归因） |
//!
//! # 传输形态
//!
//! `-p --input-format stream-json --output-format stream-json`：**prompt 走 stdin**
//! 的 stream-json 信封（[`PromptTransport::StdinJsonEnvelope`]），stdout 是逐行
//! JSON。prompt 绝不进 argv。
//!
//! # 有意不做的两件事
//!
//! 1. **不发 `--append-system-prompt`**（上游注释明确：`CLAUDE.md` 已经承载 runtime
//!    brief，再内联一次等于每 turn 重复一遍）。上游在 Claude Code 2.1.220 上验过。
//! 2. **`--strict-mcp-config` 只在上游有 managed MCP 配置时才加**：本 crate 的
//!    [`LaunchRequest`] 里没有 MCP 配置字段，因此永远不加（加了会把用户本地
//!    MCP server 全部关掉）。

use std::path::Path;

use super::claude_family::ClaudeStreamDecoder;
use super::cli_core::args::{filter_extra_args, push_flag_value, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "claude";

/// 守护进程管理、不允许被 `extra_args` 覆盖的参数（逐条对齐 `claudeBlockedArgs`）。
///
/// 覆盖它们等于改写 daemon↔CLI 的通信协议；`--effort` 归 `thinking_level` 选择器所有
/// （上游同款：宁可丢掉用户写的重复项，也不让 CLI 收到两个互相打架的 `--effort`）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("-p", ArgValueMode::Standalone),
    ("--output-format", ArgValueMode::WithValue),
    ("--input-format", ArgValueMode::WithValue),
    ("--permission-mode", ArgValueMode::WithValue),
    ("--mcp-config", ArgValueMode::WithValue),
    ("--effort", ArgValueMode::WithValue),
];

/// 已知的取值参数（只用于"这个 flag 后面那坨是不是它的值"）。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--model", ArgValueMode::WithValue),
    ("--disallowedTools", ArgValueMode::OptionalValue),
    ("--allowedTools", ArgValueMode::OptionalValue),
    ("--add-dir", ArgValueMode::OptionalValue),
    ("--resume", ArgValueMode::WithValue),
    ("--max-turns", ArgValueMode::WithValue),
    ("--append-system-prompt", ArgValueMode::WithValue),
    ("--settings", ArgValueMode::WithValue),
];

/// `extra_args` 的过滤策略（prompt 走 stdin ⇒ 位置参数必须剔除）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（`buildClaudeArgs` 的顺序逐条保留，便于对照上游日志）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "bypassPermissions",
        // 交互式提问工具在 headless stream-json 下没有 UI 可渲染，调它只会拿到空答案
        // 让模型"自己猜"（上游 GitHub #2588）⇒ 直接禁掉，澄清问题走 issue 评论。
        "--disallowedTools",
        "AskUserQuestion",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    push_flag_value(&mut args, "--model", request.model.as_deref());
    // `--effort` 紧跟 `--model`（上游注释：让 `agent command` 日志里的启动行可读）。
    push_flag_value(&mut args, "--effort", request.thinking_level.as_deref());
    push_flag_value(&mut args, "--resume", request.resume_session.as_deref());
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// Claude Code adapter。
#[derive(Debug)]
pub struct Claude {
    config: CliCoreConfig,
}

impl Claude {
    /// 默认配置：可执行文件 `claude`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(LABEL),
        }
    }

    /// 指定可执行文件（绝对路径便于测试 / 多版本共存）。
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

impl Default for Claude {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Claude {
    fn kind(&self) -> AgentType {
        AgentType::Claude
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::StdinJsonEnvelope,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::StreamJson,
                streaming: true,
                thinking: true,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            prompt_write_is_fatal: true,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(ClaudeStreamDecoder::new(
            request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
        ))
    }
}

impl TestableAdapter for Claude {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "2.1.220 (Claude Code)\n".to_owned(),
            expected_version: Some("2.1.220".to_owned()),
            success_stdout: concat!(
                r#"{"type":"system","subtype":"init","session_id":"sess-claude-1"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-sonnet-4","usage":{"input_tokens":1,"output_tokens":2},"content":[{"type":"text","text":"ok"}]}}"#,
                "\n",
                r#"{"type":"result","subtype":"success","session_id":"sess-claude-1","result":"ok","is_error":false,"modelUsage":{"claude-sonnet-4":{"inputTokens":3,"outputTokens":4,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            // assistant 累加 1+2，result 的 modelUsage 覆盖成 3+4。
            expected_usage_tokens: Some(7),
            junk_stdout: concat!(
                "not json at all\n",
                r#"{"type":"unknown_future_event","payload":{"nested":true}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "claude exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_upstream_build_claude_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("claude-opus-4")
            .with_thinking_level("high")
            .with_resume_session("sess-9");
        assert_eq!(
            build_args(&request),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--disallowedTools",
                "AskUserQuestion",
                "--model",
                "claude-opus-4",
                "--effort",
                "high",
                "--resume",
                "sess-9",
            ]
        );
    }

    #[test]
    fn protocol_flags_cannot_be_overridden_by_extra_args() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--output-format",
            "text",
            "--effort",
            "low",
            "注入的位置参数",
        ]);
        let args = build_args(&request);
        assert!(!args.contains(&"text".to_owned()));
        assert!(!args.contains(&"注入的位置参数".to_owned()));
        assert_eq!(args.iter().filter(|arg| *arg == "--effort").count(), 0);
        assert!(args.contains(&"--permission-mode".to_owned()));
    }

    #[test]
    fn prompt_never_lands_in_argv() {
        let request = LaunchRequest::new("SECRET-PROMPT");
        assert!(!build_args(&request)
            .iter()
            .any(|arg| arg.contains("SECRET")));
    }

    crate::adapter_conformance!(Claude);
}
