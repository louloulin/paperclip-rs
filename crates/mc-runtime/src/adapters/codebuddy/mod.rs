//! `codebuddy`（腾讯 `CodeBuddy` CLI）adapter —— 上游 `server/pkg/agent/codebuddy.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildCodebuddyArgs`（`codebuddy.go` L39） |
//! | `BLOCKED` | `codebuddyBlockedArgs`（L27） |
//! | 事件解码 | [`super::claude_family`]（与 `claude` 共用：上游两个 provider 的 SDK 消息结构体逐字段一致） |
//!
//! # 传输形态
//!
//! 与 `claude` 同款：`-p --input-format stream-json --output-format stream-json`，
//! **prompt 走 stdin** 的信封，绝不进 argv。
//!
//! # 三处刻意与 claude 不同（都是上游明确的选择）
//!
//! 1. **禁三个交互工具**：`--disallowedTools AskUserQuestion EnterPlanMode ExitPlanMode`。
//!    `CodeBuddy` 的 `bypassPermissions` **不**自动放行这三个，它们会走到权限桥上等一个
//!    没人能给的回答，把 turn 挂死（上游 GitHub #6012）。一次一个值：它是 variadic 且
//!    逐项精确比对工具名，逗号拼串匹配不到任何东西。
//! 2. **永不传 `--strict-mcp-config`**：它的语义是"只用 `--mcp-config` 里的 server"，
//!    会把用户的 user/project/local 三档 MCP 全部关掉（上游用进程 spawn 当 oracle 实测）。
//! 3. **可传 `--append-system-prompt`**：上游在 `opts.SystemPrompt` 非空时追加。本 crate
//!    的 [`LaunchRequest`] 没有 system prompt 字段 ⇒ 永不追加。

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
pub(crate) const LABEL: &str = "codebuddy";

/// 守护进程管理、不允许被 `extra_args` 覆盖的参数（`codebuddyBlockedArgs`）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("-p", ArgValueMode::Standalone),
    ("--output-format", ArgValueMode::WithValue),
    ("--input-format", ArgValueMode::WithValue),
    ("--permission-mode", ArgValueMode::WithValue),
    ("--mcp-config", ArgValueMode::WithValue),
    ("--effort", ArgValueMode::WithValue),
];

/// 已知的取值参数（判断"flag 后面那坨是不是它的值"）。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--model", ArgValueMode::WithValue),
    ("--disallowedTools", ArgValueMode::OptionalValue),
    ("--allowedTools", ArgValueMode::OptionalValue),
    ("--resume", ArgValueMode::WithValue),
    ("--max-turns", ArgValueMode::WithValue),
    ("--append-system-prompt", ArgValueMode::WithValue),
];

/// `extra_args` 过滤策略（prompt 走 stdin ⇒ 位置参数必须剔除）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（顺序逐条对齐 `buildCodebuddyArgs`）。
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
        "--disallowedTools",
        "AskUserQuestion",
        "EnterPlanMode",
        "ExitPlanMode",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    push_flag_value(&mut args, "--model", request.model.as_deref());
    push_flag_value(&mut args, "--effort", request.thinking_level.as_deref());
    push_flag_value(&mut args, "--resume", request.resume_session.as_deref());
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// `CodeBuddy` adapter。
#[derive(Debug)]
pub struct Codebuddy {
    config: CliCoreConfig,
}

impl Codebuddy {
    /// 默认配置：可执行文件 `codebuddy`（走 `PATH`）。
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

impl Default for Codebuddy {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Codebuddy {
    fn kind(&self) -> AgentType {
        AgentType::Codebuddy
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

impl TestableAdapter for Codebuddy {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "codebuddy 2.1.7\n".to_owned(),
            expected_version: Some("2.1.7".to_owned()),
            success_stdout: concat!(
                r#"{"type":"system","subtype":"init","session_id":"sess-codebuddy-1"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"codebuddy-4","content":[{"type":"text","text":"ok"}]}}"#,
                "\n",
                r#"{"type":"result","subtype":"success","session_id":"sess-codebuddy-1","result":"ok","is_error":false,"usage":{"input_tokens":5,"output_tokens":6}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(11),
            junk_stdout: concat!(
                "not json at all\n",
                r#"{"type":"unknown_future_event","payload":{"nested":true}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "codebuddy exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_upstream_build_codebuddy_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("codebuddy-4")
            .with_thinking_level("low")
            .with_resume_session("sess-1");
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
                "EnterPlanMode",
                "ExitPlanMode",
                "--model",
                "codebuddy-4",
                "--effort",
                "low",
                "--resume",
                "sess-1",
            ]
        );
    }

    #[test]
    fn interactive_tools_are_disabled_one_value_per_tool() {
        // 带一个尾部 flag：variadic 的值不能被当成 flag 的值吃掉
        // （逗号拼串在 CodeBuddy 里匹配不到任何工具名）。
        let args = build_args(&LaunchRequest::new("p").with_extra_args(["--verbose"]));
        let idx = args
            .iter()
            .position(|arg| arg == "--disallowedTools")
            .expect("必须禁交互工具");
        assert_eq!(
            &args[idx + 1..idx + 4],
            ["AskUserQuestion", "EnterPlanMode", "ExitPlanMode"]
        );
        // 三个值之后回到 flag，而不是把 `--verbose` 也当成工具名。
        assert_eq!(args[idx + 4], "--verbose");
    }

    #[test]
    fn strict_mcp_config_is_never_passed() {
        let args = build_args(&LaunchRequest::new("p"));
        assert!(!args.iter().any(|arg| arg == "--strict-mcp-config"));
    }

    crate::adapter_conformance!(Codebuddy);
}
