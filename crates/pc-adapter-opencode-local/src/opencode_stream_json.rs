//! OpenCode stream-json 解析与未知 session 错误判断。

use pc_adapter_api::UsageSummary;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOpenCodeStreamJson {
    pub session_id: Option<String>,
    pub summary: String,
    pub usage: UsageSummary,
    pub cost_usd: Option<f64>,
    pub error_message: Option<String>,
    pub tool_errors: Vec<String>,
}

impl Default for ParsedOpenCodeStreamJson {
    fn default() -> Self {
        Self { session_id: None, summary: String::new(), usage: UsageSummary { input_tokens: 0, output_tokens: 0, cached_input_tokens: Some(0) }, cost_usd: None, error_message: None, tool_errors: Vec::new() }
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> String { object.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_owned() }
fn number_field(object: &serde_json::Map<String, Value>, key: &str) -> u64 { object.get(key).and_then(Value::as_u64).unwrap_or(0) }

fn error_text(value: Option<&Value>) -> String {
    let Some(value) = value else { return String::new() };
    if let Some(text) = value.as_str() { return text.trim().to_owned(); }
    let Some(object) = value.as_object() else { return String::new() };
    let message = string_field(object, "message");
    if !message.is_empty() { return message; }
    if let Some(data) = object.get("data").and_then(Value::as_object) {
        let nested = string_field(data, "message");
        if !nested.is_empty() { return nested; }
    }
    for key in ["name", "code"] { let value = string_field(object, key); if !value.is_empty() { return value; } }
    serde_json::to_string(object).unwrap_or_default()
}

pub fn parse_opencode_stream_json(stdout: &str) -> ParsedOpenCodeStreamJson {
    let mut parsed = ParsedOpenCodeStreamJson::default();
    let mut messages = Vec::new();
    let mut errors = Vec::new();
    let mut cost_total = 0.0;
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() { continue; }
        let Ok(Value::Object(event)) = serde_json::from_str::<Value>(line) else { continue };
        let session_id = string_field(&event, "sessionID");
        if !session_id.is_empty() && parsed.session_id.is_none() { parsed.session_id = Some(session_id); }
        match string_field(&event, "type").as_str() {
            "text" => { if let Some(part) = event.get("part").and_then(Value::as_object) { let text = string_field(part, "text"); if !text.is_empty() { messages.push(text); } } }
            "step_finish" => {
                if let Some(part) = event.get("part").and_then(Value::as_object) {
                    if let Some(tokens) = part.get("tokens").and_then(Value::as_object) {
                        parsed.usage.input_tokens += number_field(tokens, "input");
                        parsed.usage.output_tokens += number_field(tokens, "output") + number_field(tokens, "reasoning");
                        if let Some(cache) = tokens.get("cache").and_then(Value::as_object) {
                            if let Some(cached) = parsed.usage.cached_input_tokens.as_mut() { *cached += number_field(cache, "read"); }
                        }
                    }
                    cost_total += part.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                }
            }
            "tool_use" => {
                if let Some(part) = event.get("part").and_then(Value::as_object) {
                    if let Some(state) = part.get("state").and_then(Value::as_object) {
                        if string_field(state, "status") == "error" { let text = string_field(state, "error"); if !text.is_empty() { parsed.tool_errors.push(text); } }
                    }
                }
            }
            "error" => { let text = error_text(event.get("error").or_else(|| event.get("message"))); if !text.is_empty() { errors.push(text); } }
            _ => {}
        }
    }
    parsed.summary = messages.join("\n\n").trim().to_owned();
    parsed.cost_usd = (cost_total > 0.0).then_some(cost_total);
    parsed.error_message = (!errors.is_empty()).then(|| errors.join("\n"));
    parsed
}

pub fn is_opencode_unknown_session_error(stdout: &str, stderr: &str) -> bool {
    let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    ["unknown session", "session ", "notfounderror", "no session"].iter().any(|needle| text.contains(needle)) && (text.contains("not found") || text.contains("unknown session") || text.contains("notfounderror") || text.contains("no session") || text.contains("resource not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 聚合_text_step_finish_and_error() {
        let parsed = parse_opencode_stream_json(
            r#"{"type":"text","sessionID":"s1","part":{"text":"hello"}}
{"type":"step_finish","sessionID":"s1","part":{"tokens":{"input":10,"output":4,"reasoning":2,"cache":{"read":3}},"cost":0.5}}
{"type":"error","error":{"message":"upstream failed"}}"#,
        );
        assert_eq!(parsed.summary, "hello");
        assert_eq!(parsed.usage.input_tokens, 10);
        assert_eq!(parsed.usage.output_tokens, 6);
        assert_eq!(parsed.usage.cached_input_tokens, Some(3));
        assert_eq!(parsed.cost_usd, Some(0.5));
        assert_eq!(parsed.error_message.as_deref(), Some("upstream failed"));
    }

    #[test]
    fn 工具错误不会破坏主流程() {
        let parsed = parse_opencode_stream_json(
            r#"{"type":"tool_use","sessionID":"s2","part":{"state":{"status":"error","error":"file not found"}}}
{"type":"text","sessionID":"s2","part":{"text":"recovered"}}"#,
        );
        assert!(parsed.error_message.is_none());
        assert_eq!(parsed.tool_errors, vec!["file not found"]);
        assert_eq!(parsed.summary, "recovered");
    }
}
