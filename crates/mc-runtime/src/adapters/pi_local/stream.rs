//! pi `--mode json` 事件流的解码器（`EventDecoder` 的 pi 实现）。
//!
//! 逐分支对齐上游 `server/pkg/agent/pi.go`（L530-640 的 `switch evt.Type`）：
//!
//! | pi 事件 | 本实现 |
//! |---|---|
//! | `agent_start` | `Progress { status: "running" }` |
//! | `turn_start` | **清空** output 与文本缓冲（上游 `output.Reset()`） |
//! | `message_update` + `text_delta` | `Text`（经 [`TextDrain`] 消毒） |
//! | `message_update` + `thinking_delta` | `Thinking` |
//! | `tool_execution_start` | `ToolUse` |
//! | `tool_execution_end` | `ToolResult` |
//! | `turn_end` | 累加用量（按模型）+ 记录/清除 turn 级错误 |
//! | `error` | `Error`（**不必然**终止 run） |
//! | `auto_retry_end` | `success` → 清除 turn 错误；失败 → 记协议错误 |
//!
//! 两处**刻意**的取舍（写在这里，因为改的人会问）：
//!
//! 1. `turn_start` 清空 output 是上游行为，照抄 —— 终态 `RunOutcome.output` 是
//!    **最后一个 turn** 的正文，不是全文。想拿全文要靠事件流（`Text` 事件不丢）。
//! 2. 上游还有一个"turn 级错误 10 分钟宽限计时器"（`piTurnErrorGuard`），用于
//!    "pi 报了错但既不退出也不重试"的场景。本片不做定时器：终态由退出码 /
//!    cancel / timeout 驱动（`piTurnErrorGuard` 的 10 分钟窗口在这里由
//!    `LaunchRequest::timeout` 覆盖，二者不会同时触发）。M3-8 做批量 adapter 时
//!    再统一补"无退出错误"的宽限策略。

use std::collections::BTreeMap;

use serde::Deserialize;

use super::sanitize::TextDrain;
use crate::adapter::{EventDecoder, RuntimeEvent, TokenUsage};

/// provider label（日志/错误串里的名字）。
pub(crate) const LABEL: &str = "pi";

/// `{"type": "...", ...}`（字段名对齐 pi 的 JSON 命名）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    assistant_message_event: Option<PiAssistantMessageEvent>,
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    args: Option<serde_json::Value>,
    result: Option<serde_json::Value>,
    is_error: Option<bool>,
    /// `error` 事件里是字符串，`turn_end` 里是对象 —— 与上游 `json.RawMessage` 同款处理。
    message: Option<serde_json::Value>,
    success: Option<bool>,
    final_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PiAssistantMessageEvent {
    #[serde(rename = "type")]
    kind: String,
    delta: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiTurnMessage {
    model: Option<String>,
    usage: Option<PiUsage>,
    stop_reason: Option<String>,
    error_message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiUsage {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cache_read: u64,
    #[serde(default)]
    cache_write: u64,
    #[serde(default)]
    total_tokens: u64,
}

impl From<PiUsage> for TokenUsage {
    fn from(raw: PiUsage) -> Self {
        Self {
            input: raw.input,
            output: raw.output,
            cache_read: raw.cache_read,
            cache_write: raw.cache_write,
            total_tokens: raw.total_tokens,
        }
    }
}

/// `turn_end` 的用量按模型累加后的可读快照。
pub(crate) struct PiStreamSummary {
    /// 最后一个 turn 的正文（见模块文档第 1 条）。
    pub output: String,
    /// 模型名 → 用量（`BTreeMap` 保证稳定顺序）。
    pub usage: BTreeMap<String, TokenUsage>,
    /// 未恢复的 turn 级 provider 错误。
    pub turn_error: Option<String>,
    /// 协议级错误（`error` / `auto_retry_end` 失败）。
    pub protocol_error: Option<String>,
    /// 事件计数（一致性套件按 `capabilities()` 断言用）。
    pub text_events: usize,
    pub thinking_events: usize,
    pub tool_events: usize,
}

/// pi 事件流解码器。
pub struct PiDecoder {
    drain: TextDrain,
    output: String,
    usage: BTreeMap<String, TokenUsage>,
    turn_error: Option<String>,
    protocol_error: Option<String>,
    /// `turn_end` 没给模型名时的兜底：请求里指定的模型。
    fallback_model: Option<String>,
    text_events: usize,
    thinking_events: usize,
    tool_events: usize,
}

impl PiDecoder {
    /// 新建（`decoder()` 缝用；模型名兜底为空，最终落到 `"unknown"`）。
    pub fn new() -> Self {
        Self {
            drain: TextDrain::new(),
            output: String::new(),
            usage: BTreeMap::new(),
            turn_error: None,
            protocol_error: None,
            fallback_model: None,
            text_events: 0,
            thinking_events: 0,
            tool_events: 0,
        }
    }

    /// 带模型名兜底的新实例（launch 路径用 `request.model`）。
    pub fn with_fallback_model(model: Option<String>) -> Self {
        Self {
            fallback_model: model,
            ..Self::new()
        }
    }

    /// 解码结束后的快照（`finish()` 之后取）。
    pub(crate) fn summary(&self) -> PiStreamSummary {
        PiStreamSummary {
            output: self.output.clone(),
            usage: self.usage.clone(),
            turn_error: self.turn_error.clone(),
            protocol_error: self.protocol_error.clone(),
            text_events: self.text_events,
            thinking_events: self.thinking_events,
            tool_events: self.tool_events,
        }
    }

    /// 把一条事件转成 0..n 个 `RuntimeEvent`。
    fn handle(&mut self, line: &str) -> Vec<RuntimeEvent> {
        let Ok(event) = serde_json::from_str::<PiStreamEvent>(line) else {
            // 未知/坏行不中断 run：pi 的 stdout 上混日志行是常态。
            return Vec::new();
        };
        let mut out = Vec::new();
        match event.kind.as_str() {
            "agent_start" => out.push(RuntimeEvent::Progress {
                status: "running".into(),
            }),
            "turn_start" => {
                // 上游 `output.Reset(); textBuffer.Reset()`：终态只保留最后一个 turn。
                self.output.clear();
                self.drain.reset();
            }
            "message_update" => {
                let Some(update) = event.assistant_message_event else {
                    return out;
                };
                let delta = update.delta.unwrap_or_default();
                if delta.is_empty() {
                    return out;
                }
                match update.kind.as_str() {
                    "text_delta" => {
                        let text = self.drain.push(&delta);
                        if !text.is_empty() {
                            self.output.push_str(&text);
                            self.text_events += 1;
                            out.push(RuntimeEvent::Text { delta: text });
                        }
                    }
                    "thinking_delta" => {
                        self.thinking_events += 1;
                        out.push(RuntimeEvent::Thinking { delta });
                    }
                    _ => {}
                }
            }
            "tool_execution_start" => {
                self.tool_events += 1;
                out.push(RuntimeEvent::ToolUse {
                    call_id: event.tool_call_id.unwrap_or_default(),
                    tool: event.tool_name.unwrap_or_default(),
                    input: event.args.unwrap_or(serde_json::Value::Null),
                });
            }
            "tool_execution_end" => {
                self.tool_events += 1;
                out.push(RuntimeEvent::ToolResult {
                    call_id: event.tool_call_id.unwrap_or_default(),
                    output: decode_result(event.result.as_ref()),
                    is_error: event.is_error.unwrap_or(false),
                });
            }
            "turn_end" => self.handle_turn_end(event.message.as_ref(), &mut out),
            "error" => {
                let message = decode_string(event.message.as_ref());
                if self.protocol_error.is_none() {
                    self.protocol_error = Some(message.clone());
                }
                out.push(RuntimeEvent::Error { message });
            }
            "auto_retry_end" => {
                if event.success.unwrap_or(false) {
                    self.turn_error = None;
                } else if self.protocol_error.is_none() {
                    let message = event
                        .final_error
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("{LABEL} exhausted automatic retries"));
                    self.protocol_error = Some(message);
                }
            }
            _ => {}
        }
        out
    }

    fn handle_turn_end(
        &mut self,
        message: Option<&serde_json::Value>,
        out: &mut Vec<RuntimeEvent>,
    ) {
        let Some(message) = message else {
            return;
        };
        let Ok(turn) = serde_json::from_value::<PiTurnMessage>(message.clone()) else {
            return;
        };
        if let Some(usage) = turn.usage {
            let model = turn
                .model
                .filter(|m| !m.is_empty())
                .or_else(|| self.fallback_model.clone())
                .unwrap_or_else(|| "unknown".to_owned());
            let delta: TokenUsage = usage.into();
            *self.usage.entry(model.clone()).or_default() += delta;
            out.push(RuntimeEvent::Usage {
                model,
                usage: delta,
            });
        }
        // 与上游同款：同一 turn 的 stopReason=error 会先于自动重试出现，
        // 所以它只是"待定"；`turn_start` 或一次成功的 `turn_end` 都能清掉它。
        if turn.stop_reason.as_deref() == Some("error") {
            let text = turn
                .error_message
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("{LABEL} ended the turn with an error"));
            self.turn_error = Some(text);
        } else {
            self.turn_error = None;
        }
    }
}

impl Default for PiDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl EventDecoder for PiDecoder {
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        self.handle(trimmed)
    }

    fn finish(&mut self) -> Vec<RuntimeEvent> {
        let text = self.drain.flush();
        if text.is_empty() {
            return Vec::new();
        }
        self.output.push_str(&text);
        self.text_events += 1;
        vec![RuntimeEvent::Text { delta: text }]
    }
}

/// `tool_execution_end` 的 `result`：JSON 字符串取原值，否则给原始 JSON 文本。
fn decode_result(raw: Option<&serde_json::Value>) -> String {
    match raw {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// `error` 事件的 `message`（上游 `decodePiString`）：字符串取原值，否则原文去引号。
fn decode_string(raw: Option<&serde_json::Value>) -> String {
    match raw {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string().trim_matches('"').to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AgentType;

    /// 一段真实的 pi 事件流形状：起手 → 两个 turn（含工具调用）→ 用量。
    const SUCCESS_TRANSCRIPT: &str = r#"{"type":"agent_start"}
{"type":"turn_start"}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"he"}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"llo"}}
{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"hmm"}}
{"type":"tool_execution_start","toolCallId":"c1","toolName":"read","args":{"path":"a.txt"}}
{"type":"tool_execution_end","toolCallId":"c1","result":"file body","isError":false}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":" done"}}
{"type":"turn_end","message":{"role":"assistant","model":"gpt-x","usage":{"input":10,"output":5,"cacheRead":1,"cacheWrite":2,"totalTokens":18}}}"#;

    fn decode_all(decoder: &mut PiDecoder, transcript: &str) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        for line in transcript.lines() {
            events.extend(decoder.push_line(line));
        }
        events.extend(decoder.finish());
        events
    }

    #[test]
    fn success_transcript_decodes_to_events_and_usage() {
        let mut decoder = PiDecoder::with_fallback_model(Some("requested".into()));
        let events = decode_all(&mut decoder, SUCCESS_TRANSCRIPT);
        let kinds: Vec<&str> = events.iter().map(RuntimeEvent::kind).collect();
        assert_eq!(
            kinds,
            vec![
                "progress",
                "text",
                "text",
                "thinking",
                "tool_use",
                "tool_result",
                "text",
                "usage"
            ]
        );
        let RuntimeEvent::Text { delta } = &events[1] else {
            panic!("second event is text")
        };
        assert_eq!(delta, "he");
        let RuntimeEvent::ToolUse {
            call_id,
            tool,
            input,
        } = &events[4]
        else {
            panic!("fifth event is tool_use")
        };
        assert_eq!(call_id, "c1");
        assert_eq!(tool, "read");
        assert_eq!(input["path"], "a.txt");
        let summary = decoder.summary();
        assert_eq!(summary.output, "hello done");
        assert_eq!(summary.text_events, 3);
        assert_eq!(summary.tool_events, 2);
        assert_eq!(summary.thinking_events, 1);
        assert_eq!(summary.turn_error, None);
        assert_eq!(summary.protocol_error, None);
        assert_eq!(
            summary.usage.get("gpt-x"),
            Some(&TokenUsage {
                input: 10,
                output: 5,
                cache_read: 1,
                cache_write: 2,
                total_tokens: 18
            })
        );
    }

    #[test]
    fn turn_start_resets_output_like_upstream() {
        let mut decoder = PiDecoder::new();
        decode_all(
            &mut decoder,
            r#"{"type":"turn_start"}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"first"}}
{"type":"turn_start"}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"second"}}"#,
        );
        assert_eq!(decoder.summary().output, "second");
    }

    #[test]
    fn turn_error_is_recorded_and_cleared_by_success() {
        let mut decoder = PiDecoder::new();
        decode_all(
            &mut decoder,
            r#"{"type":"turn_end","message":{"model":"m","stopReason":"error","errorMessage":"connection error."}}"#,
        );
        assert_eq!(
            decoder.summary().turn_error.as_deref(),
            Some("connection error.")
        );

        // 同样的 turn_end 之后再一个成功 turn → 清掉（自动重试成功的样子）。
        let mut decoder = PiDecoder::new();
        decode_all(
            &mut decoder,
            r#"{"type":"turn_end","message":{"model":"m","stopReason":"error","errorMessage":"boom"}}
{"type":"turn_start"}
{"type":"turn_end","message":{"model":"m","stopReason":"endTurn"}}"#,
        );
        assert_eq!(decoder.summary().turn_error, None);
    }

    #[test]
    fn turn_error_falls_back_to_label_when_message_missing() {
        let mut decoder = PiDecoder::new();
        decode_all(
            &mut decoder,
            r#"{"type":"turn_end","message":{"model":"m","stopReason":"error"}}"#,
        );
        assert_eq!(
            decoder.summary().turn_error.as_deref(),
            Some("pi ended the turn with an error")
        );
        assert_eq!(LABEL, AgentType::Pi.as_str(), "label 必须与白名单取值一致");
    }

    #[test]
    fn usage_model_falls_back_to_request_then_unknown() {
        let mut decoder = PiDecoder::with_fallback_model(Some("req".into()));
        decode_all(
            &mut decoder,
            r#"{"type":"turn_end","message":{"usage":{"input":1,"output":1}}}"#,
        );
        assert!(decoder.summary().usage.contains_key("req"));

        let mut decoder = PiDecoder::new();
        decode_all(
            &mut decoder,
            r#"{"type":"turn_end","message":{"usage":{"input":1,"output":1}}}"#,
        );
        assert!(decoder.summary().usage.contains_key("unknown"));
    }

    #[test]
    fn protocol_errors_are_reported_but_not_fatal_here() {
        let mut decoder = PiDecoder::new();
        let events = decode_all(
            &mut decoder,
            r#"{"type":"error","message":"provider said no"}
{"type":"auto_retry_end","success":false,"finalError":"exhausted retries"}
{"type":"auto_retry_end","success":false}"#,
        );
        assert_eq!(events.len(), 1, "只有 error 事件落成 Error");
        assert_eq!(events[0].kind(), "error");
        let summary = decoder.summary();
        // 第一条协议错误胜出（后续失败不覆盖）。
        assert_eq!(summary.protocol_error.as_deref(), Some("provider said no"));
    }

    #[test]
    fn auto_retry_success_clears_turn_error() {
        let mut decoder = PiDecoder::new();
        decode_all(
            &mut decoder,
            r#"{"type":"turn_end","message":{"model":"m","stopReason":"error","errorMessage":"boom"}}
{"type":"auto_retry_end","success":true}"#,
        );
        assert_eq!(decoder.summary().turn_error, None);
    }

    #[test]
    fn junk_and_unknown_lines_are_ignored() {
        let mut decoder = PiDecoder::new();
        let events = decode_all(
            &mut decoder,
            "not json at all\n\n{\"type\":\"\"}\n{\"type\":\"future_event\",\"x\":1}\n{",
        );
        assert!(events.is_empty());
        assert_eq!(decoder.summary().output, "");
    }

    #[test]
    fn text_delta_split_inside_control_token_is_joined() {
        // 半个控制 token 跨两条增量来，最终不能出现在正文里（尾部名字字符按上游 RE 贪婪吞掉）。
        let mut decoder = PiDecoder::new();
        let events = decode_all(
            &mut decoder,
            "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"ok <|turn\"}}\n{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\">done!\"}}",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(decoder.summary().output, "ok !");
    }
}
