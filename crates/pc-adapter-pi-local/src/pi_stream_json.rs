//! Pi-local stream-json 解析与未知 session 错误判断。
//!
//! 完整复刻 Node `packages/adapters/pi-local/src/server/parse.ts`
//! 的 `parsePiJsonl` 与 `isPiUnknownSessionError` 行为。

use pc_adapter_api::UsageSummary;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
/// 单条 tool call 的完整记录。
pub struct PiToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args: Option<Value>,
    pub result: Option<String>,
    pub is_error: bool,
}

/// Pi usage 累加（Pi 原生格式 + generic 格式兼容）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// `parse_pi_jsonl` 的完整结果。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedPiOutput {
    pub session_id: Option<String>,
    pub messages: Vec<String>,
    pub errors: Vec<String>,
    pub usage: PiUsage,
    pub final_message: Option<String>,
    pub tool_calls: Vec<PiToolCall>,
}

impl Default for ParsedPiOutput {
    fn default() -> Self {
        Self {
            session_id: None,
            messages: Vec::new(),
            errors: Vec::new(),
            usage: PiUsage {
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: Some(0),
                cost_usd: Some(0.0),
            },
            final_message: None,
            tool_calls: Vec::new(),
        }
    }
}

/// 转换为 `pc_adapter_api::UsageSummary`。
///
/// 与 Node 的 `usage.costUsd` 是单独字段不同，PC API 把 cost 拆出来，
/// 此处保留到 `PiUsage.cost_usd`，调用方决定是否合并。
#[allow(dead_code)]
pub fn to_usage_summary(parsed: &ParsedPiOutput) -> UsageSummary {
    UsageSummary {
        input_tokens: parsed.usage.input_tokens,
        output_tokens: parsed.usage.output_tokens,
        cached_input_tokens: parsed.usage.cached_input_tokens,
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// 取字符串原始值（不 trim），用于保留流式文本 delta 的尾随空格。
fn raw_string_field(object: &serde_json::Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn number_field(object: &serde_json::Map<String, Value>, key: &str) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn float_field(object: &serde_json::Map<String, Value>, key: &str) -> f64 {
    object.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn as_record(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

/// 从 Pi message.content 提取纯文本（兼容 string / `[{type,text}]` 数组）。
///
/// 与 Node `extractTextContent` 行为一致：string 直接返回；
/// 数组按 `text` 段过滤后用空串拼接（保留原文空白）。
fn extract_text_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        if string_field(obj, "type") == "text" {
            if let Some(text) = obj.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    parts.push(text.to_owned());
                }
            }
        }
    }
    parts.join("")
}

fn accumulate_usage(target: &mut PiUsage, usage_obj: &serde_json::Map<String, Value>) {
    target.input_tokens += number_field(usage_obj, "inputTokens")
        + number_field(usage_obj, "input");
    target.output_tokens += number_field(usage_obj, "outputTokens")
        + number_field(usage_obj, "output");
    let cached = target.cached_input_tokens.unwrap_or(0)
        + number_field(usage_obj, "cachedInputTokens")
        + number_field(usage_obj, "cacheRead");
    target.cached_input_tokens = Some(cached);

    let cost_obj = usage_obj.get("cost").and_then(Value::as_object);
    if let Some(cost) = cost_obj {
        let total = float_field(cost, "total");
        if total > 0.0 {
            let current = target.cost_usd.unwrap_or(0.0);
            target.cost_usd = Some(current + total);
        }
    } else {
        let direct = float_field(usage_obj, "costUsd");
        if direct > 0.0 {
            let current = target.cost_usd.unwrap_or(0.0);
            target.cost_usd = Some(current + direct);
        }
    }
}

/// 解析 Pi CLI `--output-format stream-json` 输出。
///
/// 与 Node `parsePiJsonl` 行为对齐：
/// - 跳过 RPC 内部事件（response / extension_* / agent_start / turn_start）。
/// - `agent_end` 取最后一条 assistant content 作为 `finalMessage`。
/// - `auto_retry_end` 失败时把 `finalError` 加入 errors。
/// - `turn_end` 累加 usage/cost，并把 `toolResults[]` 写回对应 toolCall。
/// - `message_update.text_delta` 追加到 `messages` 最后一条。
/// - `tool_execution_start`/`end` 维护 `toolCalls`。
/// - 顶层 `usage` 或 `event.usage` 兼容 Pi 格式与 generic 格式。
pub fn parse_pi_jsonl(stdout: &str) -> ParsedPiOutput {
    let mut parsed = ParsedPiOutput::default();

    for raw_line in stdout.split('\n') {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(event)) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        // 会话 ID 顶层兼容多种命名（Node 仅用 sessionId，但宽松兼容）。
        for key in ["sessionId", "session_id", "sessionID"] {
            let value = string_field(&event, key);
            if !value.is_empty() && parsed.session_id.is_none() {
                parsed.session_id = Some(value);
                break;
            }
        }

        let event_type = string_field(&event, "type");

        // RPC 内部协议 - 全部跳过。
        if matches!(
            event_type.as_str(),
            "response"
                | "extension_ui_request"
                | "extension_ui_response"
                | "extension_error"
                | "agent_start"
                | "turn_start"
        ) {
            continue;
        }

        if event_type == "agent_end" {
            if let Some(messages) = event.get("messages").and_then(Value::as_array) {
                if let Some(last) = messages.last() {
                    if let Some(last_obj) = last.as_object() {
                        if string_field(last_obj, "role") == "assistant" {
                            let content = last.get("content").unwrap_or(&Value::Null);
                            let text = extract_text_content(content);
                            if !text.is_empty() {
                                parsed.final_message = Some(text);
                            }
                        }
                    }
                }
            }
            continue;
        }

        if event_type == "auto_retry_end" {
            let succeeded = event
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !succeeded {
                let final_error = string_field(&event, "finalError");
                if final_error.is_empty() {
                    parsed.errors.push(
                        "Pi exhausted automatic retries without producing a response."
                            .to_owned(),
                    );
                } else {
                    parsed.errors.push(final_error);
                }
            }
            continue;
        }

        if event_type == "turn_end" {
            if let Some(message) = event.get("message").and_then(as_record) {
                let content = message.get("content").unwrap_or(&Value::Null);
                let text = extract_text_content(content);
                if !text.is_empty() {
                    parsed.final_message = Some(text.clone());
                    parsed.messages.push(text);
                }
                if let Some(usage) = message.get("usage").and_then(as_record) {
                    accumulate_usage(&mut parsed.usage, usage);
                }
            }

            if let Some(tool_results) = event.get("toolResults").and_then(Value::as_array) {
                for tr in tool_results {
                    let Some(tr_obj) = tr.as_object() else {
                        continue;
                    };
                    let tool_call_id = string_field(tr_obj, "toolCallId");
                    let content = tr.get("content");
                    let is_error = tr_obj
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                    if let Some(existing) = parsed
                        .tool_calls
                        .iter_mut()
                        .find(|tc| tc.tool_call_id == tool_call_id && !tool_call_id.is_empty())
                    {
                        let result_text = match content {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => serde_json::to_string(v).unwrap_or_default(),
                            None => String::new(),
                        };
                        existing.result = Some(result_text);
                        existing.is_error = is_error;
                    }
                }
            }
            continue;
        }

        if event_type == "message_update" {
            if let Some(assistant_event) =
                event.get("assistantMessageEvent").and_then(as_record)
            {
                let msg_type = string_field(assistant_event, "type");
                if msg_type == "text_delta" {
                    let delta = raw_string_field(assistant_event, "delta");
                    if !delta.is_empty() {
                        if parsed.messages.is_empty() {
                            parsed.messages.push(delta);
                        } else {
                            let last_idx = parsed.messages.len() - 1;
                            parsed.messages[last_idx].push_str(&delta);
                        }
                    }
                }
            }
            continue;
        }

        if event_type == "error" {
            let message = string_field(&event, "message");
            if !message.is_empty() {
                parsed.errors.push(message);
            }
            continue;
        }

        if event_type == "tool_execution_start" {
            let tool_call_id = string_field(&event, "toolCallId");
            let tool_name = string_field(&event, "toolName");
            let args = event.get("args").cloned();
            parsed.tool_calls.push(PiToolCall {
                tool_call_id,
                tool_name,
                args,
                result: None,
                is_error: false,
            });
            continue;
        }

        if event_type == "tool_execution_end" {
            let tool_call_id = string_field(&event, "toolCallId");
            let tool_name = string_field(&event, "toolName");
            let tool_result = event.get("result");
            let is_error = event
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let result_text = match tool_result {
                Some(Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_default(),
                None => String::new(),
            };

            let mut found = false;
            for tc in parsed.tool_calls.iter_mut() {
                if !tool_call_id.is_empty() && tc.tool_call_id == tool_call_id {
                    tc.result = Some(result_text.clone());
                    tc.is_error = is_error;
                    found = true;
                    break;
                }
            }
            // 兼容某些实现里只发送 tool_execution_end 而无 start。
            if !found && !tool_name.is_empty() {
                parsed.tool_calls.push(PiToolCall {
                    tool_call_id,
                    tool_name,
                    args: None,
                    result: Some(result_text),
                    is_error,
                });
            }
            continue;
        }

        // 顶层 usage 事件或事件本身带 usage 字段（兜底累加）。
        if event_type == "usage" || event.get("usage").is_some() {
            if let Some(usage_obj) = event.get("usage").and_then(as_record) {
                accumulate_usage(&mut parsed.usage, usage_obj);
            }
        }
    }

    parsed
}

/// 识别 Pi 报错的"未知 session"提示，用于上层判断是否需要清除 session。
pub fn is_pi_unknown_session_error(stdout: &str, stderr: &str) -> bool {
    let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    if text.is_empty() {
        return false;
    }
    if text.contains("unknown session") {
        return true;
    }
    if text.contains("session not found") {
        return true;
    }
    if text.contains("no session") {
        return true;
    }
    // session <...> not found：两个 "session" 和 "not found" 都在，且 "not found" 在 "session" 之后。
    if let Some(session_pos) = text.find("session") {
        if text[session_pos..].contains("not found") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析_turn_end_完整链路() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"turn_end","message":{"role":"assistant","content":"final answer","usage":{"input":120,"output":40,"cacheRead":20,"cost":{"total":0.0025}},"toolResults":[{"toolCallId":"call_1","content":"OK","isError":false}]}}"#,
        );
        assert_eq!(parsed.final_message.as_deref(), Some("final answer"));
        assert_eq!(parsed.messages, vec!["final answer".to_string()]);
        assert_eq!(parsed.usage.input_tokens, 120);
        assert_eq!(parsed.usage.output_tokens, 40);
        assert_eq!(parsed.usage.cached_input_tokens, Some(20));
        assert_eq!(parsed.usage.cost_usd, Some(0.0025));
    }

    #[test]
    fn 解析_agent_end取最后_assistant() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"agent_end","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"reply here"}]}"#,
        );
        assert_eq!(parsed.final_message.as_deref(), Some("reply here"));
    }

    #[test]
    fn 解析_content数组提取_text() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"hello "},{"type":"text","text":"world"}]}]}"#,
        );
        assert_eq!(parsed.final_message.as_deref(), Some("hello world"));
    }

    #[test]
    fn 解析_auto_retry_end失败记录错误() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"auto_retry_end","success":false,"finalError":"rate limit"}"#,
        );
        assert_eq!(parsed.errors, vec!["rate limit".to_string()]);
    }

    #[test]
    fn 解析_auto_retry_end成功无错误() {
        let parsed = parse_pi_jsonl(r#"{"type":"auto_retry_end","success":true}"#);
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn 解析_tool_execution_start_end_匹配() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"tool_execution_start","toolCallId":"call_1","toolName":"read","args":{"path":"a.txt"}}
{"type":"tool_execution_end","toolCallId":"call_1","toolName":"read","result":"contents","isError":false}"#,
        );
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].tool_call_id, "call_1");
        assert_eq!(parsed.tool_calls[0].tool_name, "read");
        assert_eq!(parsed.tool_calls[0].result.as_deref(), Some("contents"));
        assert!(!parsed.tool_calls[0].is_error);
    }

    #[test]
    fn 解析_tool_execution_end_兜底创建() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"tool_execution_end","toolCallId":"call_orphan","toolName":"bash","result":"oops","isError":true}"#,
        );
        assert_eq!(parsed.tool_calls.len(), 1);
        assert!(parsed.tool_calls[0].is_error);
        assert_eq!(parsed.tool_calls[0].result.as_deref(), Some("oops"));
    }

    #[test]
    fn 解析_message_update_text_delta_追加() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hel"}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"lo"}}"#,
        );
        assert_eq!(parsed.messages, vec!["Hello".to_string()]);
    }

    #[test]
    fn 解析_standalone_usage兼容generic格式() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"usage","usage":{"inputTokens":50,"outputTokens":10,"cachedInputTokens":5,"costUsd":0.001}}"#,
        );
        assert_eq!(parsed.usage.input_tokens, 50);
        assert_eq!(parsed.usage.output_tokens, 10);
        assert_eq!(parsed.usage.cached_input_tokens, Some(5));
        assert_eq!(parsed.usage.cost_usd, Some(0.001));
    }

    #[test]
    fn 解析_error事件记录消息() {
        let parsed = parse_pi_jsonl(r#"{"type":"error","message":"upstream timeout"}"#);
        assert_eq!(parsed.errors, vec!["upstream timeout".to_string()]);
    }

    #[test]
    fn 解析_error空消息跳过() {
        let parsed = parse_pi_jsonl(r#"{"type":"error","message":""}"#);
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn 解析_rpc事件全部跳过() {
        let parsed = parse_pi_jsonl(
            r#"{"type":"response","id":"1"}
{"type":"extension_ui_request","id":"2"}
{"type":"extension_ui_response","id":"3"}
{"type":"extension_error","id":"4"}
{"type":"agent_start"}
{"type":"turn_start"}"#,
        );
        assert!(parsed.messages.is_empty());
        assert!(parsed.errors.is_empty());
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn 未知_session_识别() {
        assert!(is_pi_unknown_session_error("Session not found: abc123", ""));
        assert!(is_pi_unknown_session_error("", "unknown session id: s1"));
        assert!(is_pi_unknown_session_error("", "no session available"));
        assert!(is_pi_unknown_session_error("there is a session X not found", ""));
        assert!(!is_pi_unknown_session_error("all good", ""));
        assert!(!is_pi_unknown_session_error("", ""));
    }

    #[test]
    fn 非_json行安全忽略() {
        let parsed = parse_pi_jsonl(
            "not-json-line\n{\"type\":\"turn_end\",\"message\":{\"role\":\"assistant\",\"content\":\"ok\"}}",
        );
        assert_eq!(parsed.final_message.as_deref(), Some("ok"));
    }

    #[test]
    fn to_usage_summary_映射字段() {
        let parsed = ParsedPiOutput {
            usage: PiUsage {
                input_tokens: 7,
                output_tokens: 3,
                cached_input_tokens: Some(2),
                cost_usd: Some(0.5),
            },
            ..ParsedPiOutput::default()
        };
        let usage = to_usage_summary(&parsed);
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cached_input_tokens, Some(2));
    }
    

}
