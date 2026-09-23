//! Claude 系 `stream-json` 的**共用**解码器（`claude` / `codebuddy`）。
//!
//! 上游 `claude.go` / `codebuddy.go` 的事件结构体是逐字段对齐的
//! （`claudeSDKMessage` ↔ `codebuddySDKMessage`，连 `handleUser` 的
//! `tool_result` 分支都一样），因此这里只写一份，两个 provider 各按自己的
//! 能力位与封锁参数表接进去。
//!
//! # 事件 → 事件
//!
//! | stream-json 事件 | 本实现 |
//! |---|---|
//! | `system` | 记 `session_id` + `Progress { status: "running" }` |
//! | `assistant`（`text` / `thinking` / `tool_use` 块） | `Text` / `Thinking` / `ToolUse`；`message.usage` 按模型**累加** |
//! | `user`（`tool_result` 块） | `ToolResult` |
//! | `result` | 记 `session_id`；`modelUsage` / `usage` **覆盖**用量（run 级聚合值）；`is_error` → 失败；正文为空时用 `result` 文本兜底 |
//! | `log`（`level == "error"`） | `Error`（**不**改终态） |
//! | `control_request` | **不回答**（缺口，见下） |
//!
//! # 三处与上游的差异（都记在 `docs/33`）
//!
//! 1. **正文口径**：上游 `RunOutcome.output` 在 claude 上是"最后一个 `result`
//!    事件的文本"，本 crate 统一成"所有 `Text` 事件拼接"（`adapter.rs` 对
//!    `RunOutcome::output` 的定义就是拼接，一致性套件也这么断言）。`result`
//!    文本只在**没有** assistant 正文时兜底，两个口径在单 turn 场景下等价，
//!    多 turn 场景下本实现给全文而不是最后一轮。
//! 2. **`control_request` 不回答**：上游会往 stdin 回一个"自动批准"控制帧。
//!    解码器没有 stdin 写通道（只有 codex 那种 `JsonRpc` 传输才有），所以
//!    "需要人工批准"的场景在这里会一直等到 `LaunchRequest::timeout`。
//! 3. **`terminal_reason` 只当诊断**：失败与否只看 `is_error`（上游的
//!    `resultIsError`）；`terminal_reason != "success"` 只发一条 `Error` 事件，
//!    不把 run 判死（避免把 `max_turns` 之类的正常终止误判成失败）。

use serde::Deserialize;
use serde_json::Value;

use super::cli_core::decoder::{field_str, field_u64, tokens};
use super::cli_core::{CliDecoder, CliSummary, DecoderState};
use crate::adapter::{EventDecoder, ModelUsage, RuntimeEvent, TokenUsage};

/// Claude 系 stream-json 的一行。
#[derive(Debug, Deserialize)]
struct ClaudeEvent {
    #[serde(rename = "type")]
    kind: String,
    message: Option<Value>,
    session_id: Option<String>,
    result: Option<String>,
    is_error: Option<bool>,
    terminal_reason: Option<String>,
    usage: Option<Value>,
    /// run 级按模型聚合用量（上游 `claudeSDKMessage.ModelUsage`，JSON 是 camelCase）。
    #[serde(rename = "modelUsage")]
    model_usage: Option<Value>,
    /// `result` 自带的模型名（上游同名字段；缺省时回退到请求里的模型）。
    model: Option<String>,
    log: Option<ClaudeLog>,
}

#[derive(Debug, Deserialize)]
struct ClaudeLog {
    level: Option<String>,
    message: Option<String>,
}

/// 共用解码器。
#[derive(Debug)]
pub(crate) struct ClaudeStreamDecoder {
    state: DecoderState,
    fallback_model: String,
}

impl ClaudeStreamDecoder {
    /// `fallback_model` = 请求里的模型名（`assistant` 消息没带 `model` 时用它）。
    pub(crate) fn new(fallback_model: impl Into<String>) -> Self {
        Self {
            state: DecoderState::default(),
            fallback_model: fallback_model.into(),
        }
    }

    fn handle(&mut self, event: ClaudeEvent) -> Vec<RuntimeEvent> {
        match event.kind.as_str() {
            "system" => {
                self.state.set_session_opt(event.session_id.as_deref());
                self.state.progress("running")
            }
            "assistant" => self.handle_assistant(event.message.as_ref()),
            "user" => self.handle_user(event.message.as_ref()),
            "result" => self.handle_result(&event),
            "log" => {
                let is_error = event
                    .log
                    .as_ref()
                    .and_then(|log| log.level.as_deref())
                    .is_some_and(|level| level.eq_ignore_ascii_case("error"));
                match (is_error, event.log.and_then(|log| log.message)) {
                    (true, Some(message)) => self.state.emit_error(message),
                    _ => Vec::new(),
                }
            }
            // `control_request` 与未知类型：忽略（见模块文档第 2 条）。
            _ => Vec::new(),
        }
    }

    fn handle_assistant(&mut self, message: Option<&Value>) -> Vec<RuntimeEvent> {
        let Some(message) = message else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let model = field_str(message, "model").unwrap_or_else(|| self.fallback_model.clone());
        if let Some(usage) = message.get("usage").filter(|usage| !usage.is_null()) {
            out.extend(self.state.add_usage(&model, usage_from(usage)));
        }
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return out;
        };
        for block in blocks {
            match field_str(block, "type").as_deref() {
                Some("text") => {
                    let text = field_str(block, "text").unwrap_or_default();
                    out.extend(self.state.text(&text));
                }
                Some("thinking") => {
                    let text = field_str(block, "thinking").unwrap_or_default();
                    out.extend(self.state.thinking(&text));
                }
                Some("tool_use") => {
                    let call_id = field_str(block, "id").unwrap_or_default();
                    let tool = field_str(block, "name").unwrap_or_default();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    out.extend(self.state.tool_use(call_id, tool, input));
                }
                _ => {}
            }
        }
        out
    }

    fn handle_user(&mut self, message: Option<&Value>) -> Vec<RuntimeEvent> {
        let Some(blocks) = message
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for block in blocks {
            if field_str(block, "type").as_deref() != Some("tool_result") {
                continue;
            }
            let call_id = field_str(block, "tool_use_id").unwrap_or_default();
            let output = match block.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            out.extend(self.state.tool_result(call_id, output, is_error));
        }
        out
    }

    fn handle_result(&mut self, event: &ClaudeEvent) -> Vec<RuntimeEvent> {
        let mut out = Vec::new();
        self.state.set_session_opt(event.session_id.as_deref());
        // 用量：`modelUsage`（按模型）优先，其次 result 级 `usage`（上游
        // `claudeResultUsage`），两者都是 **run 级聚合值** ⇒ 整张表**覆盖**。
        // 与 assistant 消息上的增量累加会重复计数（上游 `usage = resultUsage`）。
        out.extend(self.state.set_usage_map(self.result_usage(event)));
        if event.is_error == Some(true) {
            let message = event
                .result
                .clone()
                .filter(|text| !text.trim().is_empty())
                .or_else(|| event.terminal_reason.clone())
                .unwrap_or_else(|| "claude 报告 turn 失败".to_owned());
            out.extend(self.state.note_error(message));
        } else {
            if let Some(reason) = event
                .terminal_reason
                .as_deref()
                .filter(|reason| *reason != "success")
            {
                out.extend(self.state.emit_error(format!("terminal_reason: {reason}")));
            }
            if !self.state.has_text() {
                if let Some(text) = event.result.as_deref().filter(|text| !text.is_empty()) {
                    out.extend(self.state.text(text));
                }
            }
        }
        out
    }

    /// `result` 的 run 级用量（上游 `claudeResultUsage`）。
    ///
    /// `modelUsage` 优先；没有可用条目时退到 result 级 `usage`（模型名取
    /// `result.model`，缺省用请求里的模型）；两者都没有可用数值时返回空
    /// ⇒ 保留 assistant 消息累加出来的增量。
    fn result_usage(&self, event: &ClaudeEvent) -> Vec<ModelUsage> {
        let per_model = event
            .model_usage
            .as_ref()
            .map(model_usage_from)
            .unwrap_or_default();
        if !per_model.is_empty() {
            return per_model;
        }
        let Some(usage) = event.usage.as_ref().filter(|usage| !usage.is_null()) else {
            return Vec::new();
        };
        let usage = usage_from(usage);
        if usage.is_empty() {
            return Vec::new();
        }
        let model = event
            .model
            .clone()
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| self.fallback_model.clone());
        vec![ModelUsage { model, usage }]
    }
}

impl EventDecoder for ClaudeStreamDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        match serde_json::from_str::<ClaudeEvent>(line) {
            Ok(event) => self.handle(event),
            Err(_) => Vec::new(),
        }
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        Vec::new()
    }
}

impl CliDecoder for ClaudeStreamDecoder {
    fn summary(&self) -> CliSummary {
        self.state.summary()
    }
}

/// `usage`（`snake_case`，result / assistant 消息上的形态）。
fn usage_from(value: &Value) -> TokenUsage {
    tokens(
        field_u64(value, "input_tokens"),
        field_u64(value, "output_tokens"),
        field_u64(value, "cache_read_input_tokens"),
        field_u64(value, "cache_creation_input_tokens"),
    )
}

/// `modelUsage`（camelCase，按模型的聚合用量）。
///
/// 空名或零 token 的条目被丢掉（上游 `claudeResultUsage` 同款），避免用一堆 0
/// 把已经累加好的增量用量覆盖成空。
fn model_usage_from(value: &Value) -> Vec<ModelUsage> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(model, usage)| {
                    let usage = tokens(
                        field_u64(usage, "inputTokens"),
                        field_u64(usage, "outputTokens"),
                        field_u64(usage, "cacheReadInputTokens"),
                        field_u64(usage, "cacheCreationInputTokens"),
                    );
                    if model.is_empty() || usage.is_empty() {
                        return None;
                    }
                    Some(ModelUsage {
                        model: model.clone(),
                        usage,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> (Vec<RuntimeEvent>, CliSummary) {
        let mut decoder = ClaudeStreamDecoder::new("fallback-model");
        let mut events = Vec::new();
        for line in lines {
            events.extend(decoder.push_line(line));
        }
        events.extend(decoder.finish());
        (events, decoder.summary())
    }

    fn text_of(events: &[RuntimeEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::Text { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn multi_turn_output_is_the_whole_transcript_not_the_last_result() {
        let (events, summary) = feed(&[
            r#"{"type":"system","session_id":"s1"}"#,
            r#"{"type":"assistant","message":{"model":"m","content":[{"type":"text","text":"一"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"二"}]}}"#,
            r#"{"type":"result","session_id":"s1","subtype":"success","result":"二","is_error":false}"#,
        ]);
        assert_eq!(text_of(&events), "一二");
        assert_eq!(summary.output, "一二");
        assert_eq!(summary.session_id.as_deref(), Some("s1"));
        assert_eq!(summary.terminal_error, None);
        assert_eq!(summary.text_events, 2);
    }

    #[test]
    fn result_text_is_only_a_fallback_for_empty_assistant_text() {
        let (events, summary) = feed(&[r#"{"type":"result","result":"只有结果文本"}"#]);
        assert_eq!(text_of(&events), "只有结果文本");
        assert_eq!(summary.output, "只有结果文本");
    }

    #[test]
    fn usage_replaces_instead_of_double_counting() {
        let (_, summary) = feed(&[
            r#"{"type":"assistant","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":2},"content":[]}}"#,
            r#"{"type":"result","usage":{"input_tokens":10,"output_tokens":20},"is_error":false}"#,
        ]);
        assert_eq!(summary.usage.len(), 1);
        assert_eq!(summary.usage[0].usage.total_tokens, 30);
        assert_eq!(summary.usage[0].usage.input, 10);
    }

    #[test]
    fn per_model_usage_wins_and_error_result_fails_the_run() {
        let (_, summary) = feed(&[
            r#"{"type":"result","is_error":true,"result":"额度用尽","modelUsage":{"sonnet":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}}"#,
        ]);
        assert_eq!(summary.terminal_error.as_deref(), Some("额度用尽"));
        assert_eq!(summary.usage.len(), 1);
        assert_eq!(summary.usage[0].model, "sonnet");
        assert_eq!(summary.usage[0].usage.total_tokens, 10);
    }

    #[test]
    fn tool_events_and_error_logs_are_decoded_and_junk_is_ignored() {
        let (events, summary) = feed(&[
            "not json at all",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"path":"a"}},{"type":"thinking","thinking":"想一下"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"log","log":{"level":"error","message":"warning-ish"}}"#,
            r#"{"type":"unknown_future_event"}"#,
        ]);
        assert_eq!(summary.tool_events, 1);
        assert_eq!(summary.terminal_error, None, "log 级 error 不改终态");
        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Thinking { .. })));
        assert!(events.iter().any(|event| matches!(event, RuntimeEvent::ToolResult { call_id, output, is_error } if call_id == "t1" && output == "ok" && !is_error)));
        assert!(events.iter().any(
            |event| matches!(event, RuntimeEvent::Error { message } if message == "warning-ish")
        ));
    }
}
