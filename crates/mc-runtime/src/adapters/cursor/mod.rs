//! `cursor`（Cursor Agent CLI）adapter —— 上游 `server/pkg/agent/cursor.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildCursorArgs`（`cursor.go` L1035） |
//! | `BLOCKED` | `cursorBlockedArgs`（L1009） |
//! | [`CursorStreamDecoder`] | `Execute` 里的 scan 循环 + `handleCursorAssistant` + `parseCursorToolCall`（L607 / L711，本片自己一份，见 `stream.rs`） |
//! | 运行骨架 | [`super::cli_core`]（spawn / stdout 泵 / 终态归因 / 取消 / 版本探测） |
//!
//! # 传输形态
//!
//! `-p --output-format stream-json --yolo`：**prompt 走 stdin 的纯文本**
//! （[`PromptTransport::StdinText`]），写完即关 —— `cursor-agent` 的 `-p` 是布尔开关，
//! 读到 EOF 才算 prompt 结束（上游注释 L1021）。prompt 绝不进 argv。
//!
//! 启动骨架（白名单表里的那一行）是 `cursor-agent (stream-json)`：`cli_command()`
//! 由 `launch_header` 的第一个词推出来，因此仍是 `cursor-agent`，与本模块的
//! [`EXECUTABLE`] 一致。
//!
//! # 与上游的差异
//!
//! 1. **`AGENTS.md` / `.cursor/skills/` 由 CLI 自己读**：上游明确不给
//!    `--system-prompt` / `--max-turns`（CLI 不支持，L1047），runtime brief 走
//!    项目内的 `AGENTS.md`。本实现照抄：不加任何 prompt 内联参数。
//! 2. **不做"worker 赖着不退出"的处理**：上游看到 `result` 就 `cancel()` 掉 run
//!    context（新版 CLI 会把 worker 留在后台），本 crate 的 run 循环以 **stdout EOF**
//!    收尾，`result` 只决定成败（`stream.rs` 有详细说明）。
//! 3. **后台工具台账 / 协议漂移计数不做**，见 `stream.rs` 顶部第 2、3 条。

use std::path::Path;

use self::stream::CursorStreamDecoder;
use super::cli_core::args::{filter_extra_args, push_flag_value, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

pub(crate) mod stream;

/// 日志 / 错误串里的名字（也是退出码错误串的前缀，对齐上游的 `cursor-agent`）。
pub(crate) const LABEL: &str = "cursor-agent";

/// 默认可执行文件名。
pub(crate) const EXECUTABLE: &str = "cursor-agent";

/// 守护进程管理、不允许被 `extra_args` 覆盖的参数（逐条对齐 `cursorBlockedArgs`）。
///
/// 覆盖它们等于改写 daemon↔CLI 的通信协议：`-p` 决定 headless、`--output-format`
/// 决定 stdout 是不是逐行 JSON、`--yolo` 决定跑不跑得下去（无人值守时没有审批 UI）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("-p", ArgValueMode::Standalone),
    ("--output-format", ArgValueMode::WithValue),
    ("--yolo", ArgValueMode::Standalone),
];

/// 取值表：只用于"这个 flag 后面那坨是不是它的值"（`--workspace` 由 `cwd` 决定，
/// 但用户也可能在 `extra_args` 里自己写，不能让它的路径被当成位置参数剔掉）。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--workspace", ArgValueMode::WithValue),
    ("--model", ArgValueMode::WithValue),
    ("--resume", ArgValueMode::WithValue),
];

/// `extra_args` 的过滤策略（prompt 走 stdin ⇒ 位置参数必须剔除）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（`buildCursorArgs` 的顺序逐条保留，便于对照上游日志）。
///
/// `cursor-agent -p --output-format stream-json --yolo [--workspace <cwd>]
/// [--model <m>] [--resume <sid>] ++extra`
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--yolo"]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    // `--workspace` 紧跟固定前缀（上游把 cwd 排在 model / resume 之前）。
    push_flag_value(
        &mut args,
        "--workspace",
        request.cwd.as_deref().and_then(Path::to_str),
    );
    push_flag_value(&mut args, "--model", request.model.as_deref());
    push_flag_value(&mut args, "--resume", request.resume_session.as_deref());
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// Cursor adapter。
#[derive(Debug)]
pub struct Cursor {
    config: CliCoreConfig,
}

impl Cursor {
    /// 默认配置：可执行文件 `cursor-agent`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(EXECUTABLE),
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

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Cursor {
    fn kind(&self) -> AgentType {
        AgentType::Cursor
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::StdinText,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::StreamJson,
                streaming: true,
                thinking: true,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            // prompt 没写进去 = 这一 turn 注定跑歪（`-p` 等的是 EOF）。
            prompt_write_is_fatal: true,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(CursorStreamDecoder::new(
            request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
        ))
    }
}

impl TestableAdapter for Cursor {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "cursor-agent 2026.7.20\n".to_owned(),
            expected_version: Some("2026.7.20".to_owned()),
            // 顺序有意义：assistant 先吐正文，`step_finish` 报一份**不该被采纳**的
            // 小用量，`result` 再报权威用量（10+5=15）⇒ 用例同时钉住"result 覆盖
            // step_finish"这条优先级。
            success_stdout: concat!(
                r#"{"type":"system","subtype":"init","session_id":"cursor-sess-1"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"cursor-model","content":[{"type":"output_text","text":"ok"}]}}"#,
                "\n",
                r#"{"type":"step_finish","model":"cursor-model","part":{"tokens":{"input":1,"output":1,"cache":{"read":0}}}}"#,
                "\n",
                r#"{"type":"result","subtype":"success","session_id":"cursor-sess-1","result":"ok","is_error":false,"inputTokens":10,"outputTokens":5,"cacheReadTokens":0,"cacheWriteTokens":0}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: concat!(
                "stdout:这不是 JSON\n",
                r#"{"type":"connection","state":"connected"}"#,
                "\n",
                r#"{"type":"future_event","payload":{"nested":true}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"output_text","text":"ok"}]}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "cursor-agent exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_upstream_build_cursor_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("gpt-5")
            .with_resume_session("sess-9")
            .with_cwd("/work/dir");
        assert_eq!(
            build_args(&request),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--yolo",
                "--workspace",
                "/work/dir",
                "--model",
                "gpt-5",
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
            "-p",
            "--yolo",
            "注入的位置参数",
            "@/etc/passwd",
        ]);
        let args = build_args(&request);
        assert!(!args.contains(&"text".to_owned()));
        assert!(!args.contains(&"注入的位置参数".to_owned()));
        assert!(!args.contains(&"@/etc/passwd".to_owned()));
        assert_eq!(args.iter().filter(|arg| *arg == "-p").count(), 1);
        assert_eq!(args.iter().filter(|arg| *arg == "--yolo").count(), 1);
    }

    #[test]
    fn an_extra_workspace_keeps_its_value() {
        let request = LaunchRequest::new("p").with_extra_args(["--workspace", "/other"]);
        assert_eq!(
            build_args(&request),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--yolo",
                "--workspace",
                "/other",
            ]
        );
    }

    #[test]
    fn prompt_never_lands_in_argv() {
        let request = LaunchRequest::new("SECRET-PROMPT");
        assert!(!build_args(&request)
            .iter()
            .any(|arg| arg.contains("SECRET")));
    }

    crate::adapter_conformance!(Cursor);
}
