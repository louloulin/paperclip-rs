//! copilot CLI 的 JSONL 解码器（上游 `copilot.go` 的 `handleCopilotEvent`）。
//!
//! # 协议
//!
//! `copilot -p … --output-format json` 逐行吐**点分事件名**的事件流：
//!
//! ```json
//! {"type":"session.start","data":{"sessionId":"…","selectedModel":"…"}}
//! {"type":"assistant.message_delta","data":{"messageId":"…","deltaContent":"o"}}
//! {"type":"assistant.message","data":{"content":"ok","model":"gpt-5","toolRequests":[…]}}
//! {"type":"assistant.usage","data":{"model":"gpt-5","inputTokens":11,"outputTokens":5}}
//! {"type":"tool.execution_complete","data":{"toolCallId":"…","success":true,"result":{"content":"…"}}}
//! {"type":"result","sessionId":"…","exitCode":0}
//! ```
//!
//! 注意 `result` 的 `sessionId` / `exitCode` 在**信封**上（不是 `data` 里），
//! 而其余事件的数据都在 `data` 里 —— 上游的 `copilotEvent` 结构体就是这么定义的。
//!
//! # 与上游的三处有意差异
//!
//! 1. **正文按"增量拼接"计**（本 crate 的 [`RunOutcome::output`] 契约）：上游在
//!    `assistant.message` 到达时会 `output.Reset()` 只留最后一轮，因此多轮对话的
//!    正文口径不同（上游=最后一轮，本 crate=全文）。`assistant.message` 的
//!    `content` 只在**本轮没有交付过增量**时兜底成 `Text`，所以拼接里不会重复。
//! 2. **`session.warning` 不转成 `Error` 事件**：它只是告警，不该让 run 看起来出错
//!    （上游也只是 `MessageLog{level:"warn"}`）。
//! 3. **`assistant.reasoning*` 归到 `Thinking`**：与上游一致，但本 crate 的
//!    `RunOutcome` 不含推理文本，只作为流式事件下发。
//!
//! [`RunOutcome::output`]: crate::adapter::RunOutcome::output

use std::collections::BTreeMap;

use serde_json::Value;

use super::super::cli_core::decoder::{
    field_str, field_u64, json_u64, tokens, CliDecoder, CliSummary, DecoderState,
};
use crate::adapter::{EventDecoder, ModelUsage, RuntimeEvent, TokenUsage};

/// copilot 的用量可能来自三个事件，每个来源分别累加、最后只取一份
/// （上游 `resolveUsage`：三者描述的是同一批 token，绝不相加）。
#[derive(Debug, Default)]
struct UsageSources {
    /// `assistant.usage`：每次模型调用，字段最全。
    call: BTreeMap<String, TokenUsage>,
    /// `assistant.message.outputTokens`：老 CLI 只有输出侧。
    message: BTreeMap<String, TokenUsage>,
    /// `session.shutdown`：会话级总量（续跑时含历史，不能用于续跑）。
    shutdown: BTreeMap<String, TokenUsage>,
}

impl UsageSources {
    /// 择优：shutdown（仅全新会话）> call > message。
    fn resolve(&self, resumed: bool) -> Vec<ModelUsage> {
        let source = if !resumed && has_tokens(&self.shutdown) {
            &self.shutdown
        } else if has_tokens(&self.call) {
            &self.call
        } else if has_tokens(&self.message) {
            &self.message
        } else {
            return Vec::new();
        };
        source
            .iter()
            .map(|(model, usage)| ModelUsage {
                model: model.clone(),
                usage: *usage,
            })
            .collect()
    }
}

/// 任何一段非零才算"这份来源真的有数字"（上游 `hasTokens`：只有模型名、
/// 没有 token 数的空记录不能盖住有数字的来源）。
fn has_tokens(usage: &BTreeMap<String, TokenUsage>) -> bool {
    usage.values().any(|usage| {
        usage.input != 0 || usage.output != 0 || usage.cache_read != 0 || usage.cache_write != 0
    })
}

/// 折一条用量记录（上游 `addUsage`）：copilot 的 `inputTokens` **含**缓存 token，
/// 而 [`TokenUsage::input`] 是单独计价的未缓存部分，所以要减掉，避免重复计价。
fn add_usage(
    dst: &mut BTreeMap<String, TokenUsage>,
    model: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) {
    let uncached_input = input.saturating_sub(cache_read).saturating_sub(cache_write);
    if uncached_input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return;
    }
    *dst.entry(model.to_owned()).or_default() +=
        tokens(uncached_input, output, cache_read, cache_write);
}

/// copilot CLI 的事件解码器。
pub(crate) struct CopilotDecoder {
    state: DecoderState,
    /// 配置里的模型名（`assistant.message` / `assistant.usage` 没带模型时用它）。
    active_model: String,
    /// 续跑的会话不能采信 `session.shutdown` 总量（含历史）。
    resumed: bool,
    sources: UsageSources,
    /// 本轮是否交付过流式增量（决定 `assistant.message.content` 要不要兜底）。
    deltas_this_turn: bool,
}

impl CopilotDecoder {
    pub(crate) fn new(fallback_model: String, resumed: bool) -> Self {
        Self {
            state: DecoderState::default(),
            active_model: fallback_model,
            resumed,
            sources: UsageSources::default(),
            deltas_this_turn: false,
        }
    }

    // 事件开关：上游 `handleEvent` 一个 switch 包完，拆开反而看不出“哪些事件会吐事件”。
    #[allow(clippy::too_many_lines)]
    fn handle(&mut self, event: &Value) -> Vec<RuntimeEvent> {
        let Some(kind) = field_str(event, "type") else {
            return Vec::new();
        };
        let data = event.get("data").cloned().unwrap_or(Value::Null);
        match kind.as_str() {
            "session.start" => {
                if let Some(model) = field_str(&data, "selectedModel") {
                    self.active_model = model;
                }
                // `result` 可能永远不来（超时/取消/崩），会话 id 先从这儿拿。
                if let Some(session) = field_str(&data, "sessionId") {
                    self.state.set_session(&session);
                }
                Vec::new()
            }
            "assistant.message_delta" => {
                let Some(delta) = field_str(&data, "deltaContent") else {
                    return Vec::new();
                };
                self.deltas_this_turn = true;
                self.state.text(&delta)
            }
            "assistant.message" => self.handle_message(&data),
            "assistant.usage" => {
                let model = match field_str(&data, "model") {
                    // CLI 认不出模型时写 "unknown"，保留会话已解析出的名字。
                    Some(model) if model != "unknown" => {
                        model.clone_into(&mut self.active_model);
                        model
                    }
                    _ => self.active_model.clone(),
                };
                add_usage(
                    &mut self.sources.call,
                    &model,
                    field_u64(&data, "inputTokens"),
                    field_u64(&data, "outputTokens"),
                    field_u64(&data, "cacheReadTokens"),
                    field_u64(&data, "cacheWriteTokens"),
                );
                self.refresh_usage()
            }
            "session.shutdown" => {
                let Some(metrics) = data.get("modelMetrics").and_then(Value::as_object) else {
                    return Vec::new();
                };
                for (model, metric) in metrics {
                    let model = if model.is_empty() {
                        self.active_model.clone()
                    } else {
                        model.clone()
                    };
                    let usage = metric.get("usage").cloned().unwrap_or(Value::Null);
                    add_usage(
                        &mut self.sources.shutdown,
                        &model,
                        field_u64(&usage, "inputTokens"),
                        field_u64(&usage, "outputTokens"),
                        field_u64(&usage, "cacheReadTokens"),
                        field_u64(&usage, "cacheWriteTokens"),
                    );
                }
                self.refresh_usage()
            }
            "assistant.reasoning" | "assistant.reasoning_delta" => {
                let text = field_str(&data, "content")
                    .or_else(|| field_str(&data, "deltaContent"))
                    .unwrap_or_default();
                self.state.thinking(&text)
            }
            "tool.execution_complete" => {
                if let Some(model) = field_str(&data, "model") {
                    self.active_model = model;
                }
                let call_id = field_str(&data, "toolCallId").unwrap_or_default();
                let success = data
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let result = data
                    .get("result")
                    .and_then(|result| field_str(result, "content"))
                    .unwrap_or_default();
                let output = if success {
                    result
                } else {
                    match data
                        .get("error")
                        .and_then(|error| field_str(error, "message"))
                    {
                        Some(message) => format!("Error: {message}"),
                        None => result,
                    }
                };
                self.state.tool_result(call_id, output, !success)
            }
            "assistant.turn_start" => self.state.progress("running"),
            "session.error" => {
                let message =
                    field_str(&data, "message").unwrap_or_else(|| "copilot 会话错误".to_owned());
                self.state.note_error(message)
            }
            // `session.warning` 不是错误：告警只是告警（上游 `MessageLog{level:"warn"}`），
            // 不吐任何事件 —— 由下面的兜底臂承担（clippy 的 `match_same_arms` 在这里是对的）。
            "result" => {
                // 信封上的字段：会话 id 以它为准（`session.start` 只是兜底）。
                if let Some(session) = field_str(event, "sessionId") {
                    self.state.set_session(&session);
                }
                if event.get("exitCode").is_some() {
                    let code = json_u64(event.get("exitCode"));
                    if code != 0 {
                        return self
                            .state
                            .note_error(format!("copilot exited with code {code}"));
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_message(&mut self, data: &Value) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        if let Some(model) = field_str(data, "model") {
            self.active_model = model;
        }
        if let Some(reasoning) = field_str(data, "reasoningText") {
            events.extend(self.state.thinking(&reasoning));
        }
        let output_tokens = field_u64(data, "outputTokens");
        if output_tokens > 0 {
            let model = self.active_model.clone();
            add_usage(&mut self.sources.message, &model, 0, output_tokens, 0, 0);
            events.extend(self.refresh_usage());
        }
        if let Some(requests) = data.get("toolRequests").and_then(Value::as_array) {
            for request in requests {
                let tool = field_str(request, "name").unwrap_or_default();
                let call_id = field_str(request, "toolCallId").unwrap_or_default();
                events.extend(self.state.tool_use(call_id, tool, tool_arguments(request)));
            }
        }
        // 增量已经交付过就不重复：`content` 是同一轮的权威全文。
        if !self.deltas_this_turn {
            let content = field_str(data, "content").unwrap_or_default();
            events.extend(self.state.text(&content));
        }
        self.deltas_this_turn = false;
        events
    }

    /// 把新记录的用量同步进累加器（整体覆盖，三个来源不会叠加）。
    fn refresh_usage(&mut self) -> Vec<RuntimeEvent> {
        let resolved = self.sources.resolve(self.resumed);
        self.state.set_usage_map(resolved)
    }
}

/// `toolRequests[].arguments`：正常是对象，容忍被编码成字符串的形态。
fn tool_arguments(request: &Value) -> Value {
    match request.get("arguments") {
        Some(Value::String(raw)) => serde_json::from_str(raw).unwrap_or(Value::Null),
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

impl EventDecoder for CopilotDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        if line.trim().is_empty() {
            return Vec::new();
        }
        match serde_json::from_str::<Value>(line) {
            Ok(event) => self.handle(&event),
            // 非 JSON 行（CLI 的横幅/告警）：容忍。
            Err(_) => Vec::new(),
        }
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        // 终态由进程退出码（或协议内的 `session.error` / `result.exitCode`）决定；
        // copilot 没有"流结束必须带终止事件"的约定。
        Vec::new()
    }
}

impl CliDecoder for CopilotDecoder {
    fn summary(&self) -> CliSummary {
        // 用量表已由 `refresh_usage` 保持一致（择优后的那一份）。
        self.state.summary()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> CopilotDecoder {
        CopilotDecoder::new("gpt-5".to_owned(), false)
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

    fn usage_total(decoder: &CopilotDecoder) -> u64 {
        decoder
            .summary()
            .usage
            .iter()
            .map(|entry| entry.usage.total_tokens)
            .sum()
    }

    #[test]
    fn deltas_and_the_authoritative_message_do_not_duplicate_text() {
        let mut decoder = decoder();
        let mut events = decoder.push_line(
            r#"{"type":"assistant.message_delta","data":{"messageId":"m1","deltaContent":"o"}}"#,
        );
        events.extend(decoder.push_line(
            r#"{"type":"assistant.message_delta","data":{"messageId":"m1","deltaContent":"k"}}"#,
        ));
        events.extend(decoder.push_line(
            r#"{"type":"assistant.message","data":{"messageId":"m1","model":"gpt-5","content":"ok"}}"#,
        ));
        assert_eq!(text_of(&events), "ok");
        assert_eq!(decoder.summary().output, "ok");
        // 下一轮没有增量时，`content` 兜底成正文。
        let events = decoder.push_line(
            r#"{"type":"assistant.message","data":{"messageId":"m2","content":"第二轮"}}"#,
        );
        assert_eq!(text_of(&events), "第二轮");
        assert_eq!(decoder.summary().output, "ok第二轮");
    }

    #[test]
    fn session_and_model_come_from_session_start_and_result() {
        let mut decoder = decoder();
        decoder.push_line(
            r#"{"type":"session.start","data":{"sessionId":"ses-1","selectedModel":"gpt-5.1"}}"#,
        );
        assert_eq!(decoder.summary().session_id.as_deref(), Some("ses-1"));
        // 用量没带模型名时落到会话解析出来的名字上。
        decoder
            .push_line(r#"{"type":"assistant.usage","data":{"inputTokens":3,"outputTokens":2}}"#);
        assert_eq!(decoder.summary().usage[0].model, "gpt-5.1");
        decoder.push_line(r#"{"type":"result","sessionId":"ses-2","exitCode":0}"#);
        assert_eq!(decoder.summary().session_id.as_deref(), Some("ses-2"));
    }

    #[test]
    fn call_usage_wins_over_message_output_tokens() {
        let mut decoder = decoder();
        decoder.push_line(
            r#"{"type":"assistant.usage","data":{"model":"gpt-5","inputTokens":11,"outputTokens":5,"cacheReadTokens":1,"cacheWriteTokens":0}}"#,
        );
        // `assistant.message` 只报输出侧，不能盖住有完整分段的那一份。
        decoder
            .push_line(r#"{"type":"assistant.message","data":{"content":"ok","outputTokens":5}}"#);
        assert_eq!(usage_total(&decoder), 16);
        let usage = &decoder.summary().usage[0].usage;
        // copilot 的 inputTokens 含缓存 ⇒ 未缓存输入 = 11 - 1 = 10。
        assert_eq!((usage.input, usage.output, usage.cache_read), (10, 5, 1));
    }

    #[test]
    fn shutdown_usage_wins_on_a_fresh_session_but_not_on_a_resume() {
        let mut decoder = decoder();
        decoder.push_line(
            r#"{"type":"assistant.usage","data":{"model":"gpt-5","inputTokens":1,"outputTokens":2}}"#,
        );
        decoder.push_line(
            r#"{"type":"session.shutdown","data":{"modelMetrics":{"gpt-5":{"usage":{"inputTokens":20,"outputTokens":10,"cacheReadTokens":2,"cacheWriteTokens":0}}}}}"#,
        );
        assert_eq!(usage_total(&decoder), 30);

        // 续跑：shutdown 含历史 token，退回 per-call 口径。
        let mut resumed = CopilotDecoder::new("gpt-5".to_owned(), true);
        resumed.push_line(
            r#"{"type":"assistant.usage","data":{"model":"gpt-5","inputTokens":1,"outputTokens":2}}"#,
        );
        resumed.push_line(
            r#"{"type":"session.shutdown","data":{"modelMetrics":{"gpt-5":{"usage":{"inputTokens":999,"outputTokens":0}}}}}"#,
        );
        assert_eq!(usage_total(&resumed), 3);
    }

    #[test]
    fn tool_requests_and_execution_complete_become_pairs() {
        let mut decoder = decoder();
        let events = decoder.push_line(
            r#"{"type":"assistant.message","data":{"content":"","toolRequests":[{"toolCallId":"c1","name":"read_file","arguments":{"path":"a.rs"}}]}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [RuntimeEvent::ToolUse { call_id, tool, input }]
                if call_id == "c1" && tool == "read_file" && input["path"] == "a.rs"
        ));
        let events = decoder.push_line(
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"c1","success":true,"result":{"content":"a.rs"}}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [RuntimeEvent::ToolResult { call_id, output, is_error }]
                if call_id == "c1" && output == "a.rs" && !is_error
        ));
        let events = decoder.push_line(
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"c2","success":false,"error":{"message":"nope"}}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [RuntimeEvent::ToolResult { output, is_error, .. }]
                if output == "Error: nope" && *is_error
        ));
    }

    #[test]
    fn reasoning_becomes_thinking_and_warnings_are_not_errors() {
        let mut decoder = decoder();
        let events = decoder
            .push_line(r#"{"type":"assistant.reasoning_delta","data":{"deltaContent":"想想"}}"#);
        assert!(matches!(events.as_slice(), [RuntimeEvent::Thinking { delta }] if delta == "想想"));
        assert!(decoder
            .push_line(r#"{"type":"session.warning","data":{"warningType":"x","message":"注意"}}"#)
            .is_empty());
        assert!(decoder.summary().terminal_error.is_none());
    }

    #[test]
    fn session_error_and_nonzero_result_exit_fail_the_run() {
        let mut error_decoder = decoder();
        let events = error_decoder
            .push_line(r#"{"type":"session.error","data":{"errorType":"x","message":"boom"}}"#);
        assert!(matches!(events.as_slice(), [RuntimeEvent::Error { .. }]));
        assert_eq!(
            error_decoder.summary().terminal_error.as_deref(),
            Some("boom")
        );

        let mut exit_decoder = decoder();
        let events = exit_decoder.push_line(r#"{"type":"result","sessionId":"s","exitCode":7}"#);
        assert!(matches!(events.as_slice(), [RuntimeEvent::Error { .. }]));
        assert_eq!(
            exit_decoder.summary().terminal_error.as_deref(),
            Some("copilot exited with code 7")
        );
    }

    #[test]
    fn junk_and_unknown_events_are_ignored() {
        let mut decoder = decoder();
        assert!(decoder.push_line("Total usage est: 0").is_empty());
        assert!(decoder
            .push_line(r#"{"type":"future.event","data":{"x":1}}"#)
            .is_empty());
        assert!(decoder.push_line(r#"{"data":{"no":"type"}}"#).is_empty());
        assert!(decoder.finish().is_empty());
        assert!(decoder.summary().terminal_error.is_none());
    }
}
