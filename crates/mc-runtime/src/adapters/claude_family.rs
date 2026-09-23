//! Claude 系 `stream-json` 的**共用**解码器（`claude` / `codebuddy` / `qwen`）。
//!
//! 上游 `claude.go` / `codebuddy.go` / `qwen.go` 的事件结构体是逐字段对齐的
//! （`claudeSDKMessage` ↔ `codebuddySDKMessage` ↔ `qwenStreamEvent`，连
//! `handleUser` 的 `tool_result` 分支都一样），因此这里只写一份，各家按自己的
//! 方言（[`ClaudeStreamFlavor`]）、能力位与封锁参数表接进去。
//!
//! # 事件 → 事件
//!
//! | stream-json 事件 | 本实现 |
//! |---|---|
//! | `system` | 记 `session_id` + `Progress { status: "running" }` |
//! | `assistant`（`text` / `thinking` / `tool_use` 块） | `Text` / `Thinking` / `ToolUse`；`message.usage` 按模型**累加** |
//! | `user`（`tool_result` 块） | `ToolResult` |
//! | `result` | 记 `session_id`；`modelUsage` / `usage` **覆盖**用量（run 级聚合值）；`is_error` → 失败；正文为空时用 `result` 文本兜底 |
//! | `error`（**只有 qwen**） | 立刻失败（`note_error`） |
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
    /// 子类型（qwen 用 `error` / `failed` 表达 turn 失败）。
    subtype: Option<String>,
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
    /// qwen 的独立 `error` 事件载荷（对象或字符串）。
    error: Option<Value>,
    log: Option<ClaudeLog>,
}

#[derive(Debug, Deserialize)]
struct ClaudeLog {
    level: Option<String>,
    message: Option<String>,
}

/// Claude 系 stream-json 的**方言**。
///
/// 目前两家：`claude` / `codebuddy`（原味）与 `qwen`（Qwen Code CLI）。qwen 的
/// 事件结构体与 claude 几乎逐字段对齐（`qwenStreamEvent` 就是
/// `claudeSDKMessage` 加了 `subtype` / `error`），但四处**语义**不同：
///
/// 1. `error` 事件类型 → 立刻判失败（fail-closed）；
/// 2. `assistant.message.usage` 只在 `message.model` 非空时才累加（qwen 的门）；
/// 3. qwen 把缓存读**折进了** `input_tokens` ⇒ 累加前先减掉 `cache_read_input_tokens`
///    （饱和减法），且 qwen 没有 cache-creation 概念；
/// 4. 恢复会话时 `result.usage` 是**整个会话**的累计值，直接覆盖会把本轮
///    用量算成历史总量 ⇒ 恢复场景整条跳过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeStreamFlavor {
    /// claude / codebuddy。
    Claude,
    /// Qwen Code CLI。
    Qwen,
}

/// 共用解码器。
#[derive(Debug)]
pub(crate) struct ClaudeStreamDecoder {
    state: DecoderState,
    fallback_model: String,
    flavor: ClaudeStreamFlavor,
    /// 本次是不是恢复会话（qwen 的结果用量要按它决定跳不跳）。
    resumed: bool,
    /// qwen：见过的模型名（顶层 `model` 或 assistant 的 `message.model`），
    /// 用来给 `result.usage` 归因。
    seen_model: Option<String>,
}

impl ClaudeStreamDecoder {
    /// `fallback_model` = 请求里的模型名（`assistant` 消息没带 `model` 时用它）。
    pub(crate) fn new(fallback_model: impl Into<String>) -> Self {
        Self::for_flavor(ClaudeStreamFlavor::Claude, fallback_model)
    }

    /// 指定方言（`qwen` 用 [`ClaudeStreamFlavor::Qwen`]）。
    pub(crate) fn for_flavor(
        flavor: ClaudeStreamFlavor,
        fallback_model: impl Into<String>,
    ) -> Self {
        Self {
            state: DecoderState::default(),
            fallback_model: fallback_model.into(),
            flavor,
            resumed: false,
            seen_model: None,
        }
    }

    /// 标记本次为恢复会话（qwen 跳过 `result.usage`）。
    pub(crate) fn with_resume(mut self, resumed: bool) -> Self {
        self.resumed = resumed;
        self
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
            // qwen 的独立错误事件：立刻判死（上游 `handleQwenEvent` 的 `error` 分支把
            // `sawResult` / `resultIsError` 都置位）。
            "error" if self.flavor == ClaudeStreamFlavor::Qwen => {
                self.state.set_session_opt(event.session_id.as_deref());
                self.state.note_error(qwen_error_text(&event))
            }
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
        let raw_model = field_str(message, "model").filter(|model| !model.is_empty());
        let model = raw_model
            .clone()
            .unwrap_or_else(|| self.fallback_model.clone());
        // qwen：`message.model` 为空就不算用量（上游 `message.Usage != nil &&
        // message.Model != ""`）；claude 没有这道门。
        let usage_gate = match self.flavor {
            ClaudeStreamFlavor::Claude => true,
            ClaudeStreamFlavor::Qwen => raw_model.is_some(),
        };
        if usage_gate {
            if let Some(usage) = message.get("usage").filter(|usage| !usage.is_null()) {
                if self.flavor == ClaudeStreamFlavor::Qwen {
                    self.seen_model = raw_model;
                    out.extend(self.state.add_usage(&model, qwen_usage_from(usage)));
                } else {
                    out.extend(self.state.add_usage(&model, usage_from(usage)));
                }
            }
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
        match self.flavor {
            ClaudeStreamFlavor::Claude => self.handle_claude_result(event),
            ClaudeStreamFlavor::Qwen => self.handle_qwen_result(event),
        }
    }

    /// qwen 的 `result`：失败判定看 `is_error` **或** `subtype`，恢复会话时跳过用量。
    fn handle_qwen_result(&mut self, event: &ClaudeEvent) -> Vec<RuntimeEvent> {
        let mut out = Vec::new();
        self.state.set_session_opt(event.session_id.as_deref());
        if let Some(model) = event.model.as_deref().filter(|model| !model.is_empty()) {
            self.seen_model = Some(model.to_owned());
        }
        let failed = event.is_error == Some(true)
            || matches!(event.subtype.as_deref(), Some("error" | "failed"));
        if failed {
            out.extend(self.state.note_error(qwen_error_text(event)));
            return out;
        }
        // 恢复会话时 `result.usage` 是会话累计值：覆盖会把本轮算成历史总量 ⇒ 跳过，
        // 保留 assistant 消息累加出来的增量。没有可用数值时同样保留（不是清零）。
        if !self.resumed {
            let usage = self.qwen_result_usage(event);
            if !usage.is_empty() {
                out.extend(self.state.set_usage_map(usage));
            }
        }
        if !self.state.has_text() {
            if let Some(text) = event.result.as_deref().filter(|text| !text.is_empty()) {
                out.extend(self.state.text(text));
            }
        }
        out
    }

    /// qwen `result` 的用量：`modelUsage`（若对端也给）优先，其次 result 级 `usage`。
    fn qwen_result_usage(&self, event: &ClaudeEvent) -> Vec<ModelUsage> {
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
            .or_else(|| self.seen_model.clone())
            .unwrap_or_else(|| self.fallback_model.clone());
        vec![ModelUsage { model, usage }]
    }

    fn handle_claude_result(&mut self, event: &ClaudeEvent) -> Vec<RuntimeEvent> {
        let mut out = Vec::new();
        self.state.set_session_opt(event.session_id.as_deref());
        // 用量：`modelUsage`（按模型）优先，其次 result 级 `usage`（上游
        // `claudeResultUsage`），两者都是 **run 级聚合值** ⇒ 整张表**覆盖**。
        // 与 assistant 消息上的增量累加会重复计数（上游 `usage = resultUsage`）。
        // 没有任何可用数值时**保留**原表：`result_usage` 返回空是"这份 result 没给
        // 用量"，不是"用量为 0"（见 [`Self::result_usage`] 的文档）。
        let usage = self.result_usage(event);
        if !usage.is_empty() {
            out.extend(self.state.set_usage_map(usage));
        }
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

/// qwen 的 `usage`：缓存读**已经折在** `input_tokens` 里 ⇒ 先饱和减法再统计，
/// 且没有 cache-creation。（上游 `qwen.go` 同款口径。）
fn qwen_usage_from(value: &Value) -> TokenUsage {
    let input = field_u64(value, "input_tokens");
    let cache_read = field_u64(value, "cache_read_input_tokens");
    tokens(
        input.saturating_sub(cache_read),
        field_u64(value, "output_tokens"),
        cache_read,
        0,
    )
}

/// qwen 错误文本：`result` → `error.message` → `error` 的 JSON 串 → 兜底句子。
///
/// （上游 `qwenErrorText`，逐档对齐。）
fn qwen_error_text(event: &ClaudeEvent) -> String {
    if let Some(text) = event
        .result
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        return text.to_owned();
    }
    if let Some(message) = event
        .error
        .as_ref()
        .and_then(|error| field_str(error, "message"))
        .filter(|message| !message.is_empty())
    {
        return message;
    }
    if let Some(error) = event.error.as_ref().filter(|error| !error.is_null()) {
        return error.to_string();
    }
    "qwen returned an error event without details".to_owned()
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

    #[test]
    fn a_result_without_any_usage_keeps_the_assistant_increments() {
        let mut decoder =
            ClaudeStreamDecoder::for_flavor(ClaudeStreamFlavor::Claude, "claude-sonnet-4");
        decoder.push_line(
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4","usage":{"input_tokens":10,"output_tokens":5},"content":[{"type":"text","text":"ok"}]}}"#,
        );
        assert_eq!(decoder.summary().usage[0].usage.total_tokens, 15);
        // `result` 不带用量：`result_usage` 返回空 ⇒ 保留 assistant 的增量。
        decoder.push_line(
            r#"{"type":"result","subtype":"success","session_id":"s","result":"ok","is_error":false}"#,
        );
        assert_eq!(
            decoder.summary().usage[0].usage.total_tokens,
            15,
            "result 没给用量时不该把 assistant 的增量抹掉"
        );
    }

    #[test]
    fn qwen_error_event_fails_the_run_and_uses_the_payload_text() {
        let mut decoder = ClaudeStreamDecoder::for_flavor(ClaudeStreamFlavor::Qwen, "qwen3");
        let events = decoder.push_line(r#"{"type":"error","error":{"message":"额度用尽"}}"#);
        assert!(events.iter().any(
            |event| matches!(event, RuntimeEvent::Error { message } if message == "额度用尽")
        ));
        assert_eq!(
            decoder.summary().terminal_error.as_deref(),
            Some("额度用尽")
        );
    }

    #[test]
    fn qwen_folds_cache_reads_into_input_and_skips_result_usage_when_resumed() {
        let mut decoder =
            ClaudeStreamDecoder::for_flavor(ClaudeStreamFlavor::Qwen, "qwen3").with_resume(true);
        decoder.push_line(
            r#"{"type":"assistant","message":{"model":"qwen3","usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":4},"content":[{"type":"text","text":"ok"}]}}"#,
        );
        assert_eq!(decoder.summary().usage[0].usage.input, 6);
        assert_eq!(decoder.summary().usage[0].usage.cache_read, 4);
        decoder.push_line(
            r#"{"type":"result","subtype":"success","session_id":"s","result":"ok","usage":{"input_tokens":900,"output_tokens":900}}"#,
        );
        assert_eq!(
            decoder.summary().usage[0].usage.total_tokens,
            12,
            "恢复会话时 result.usage 是会话累计值，必须跳过"
        );
        assert_eq!(decoder.summary().session_id.as_deref(), Some("s"));
    }

    #[test]
    fn qwen_failed_subtype_fails_and_missing_model_skips_assistant_usage() {
        let mut decoder = ClaudeStreamDecoder::for_flavor(ClaudeStreamFlavor::Qwen, "qwen3");
        decoder.push_line(
            r#"{"type":"assistant","message":{"usage":{"input_tokens":10},"content":[]}}"#,
        );
        assert!(decoder.summary().usage.is_empty(), "model 为空时不累加用量");
        decoder.push_line(r#"{"type":"result","subtype":"failed","result":"炸了"}"#);
        assert_eq!(decoder.summary().terminal_error.as_deref(), Some("炸了"));
    }
}
