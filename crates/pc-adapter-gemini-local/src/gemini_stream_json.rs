//! Gemini CLI stream-json 解析器。

use pc_adapter_api::UsageSummary;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeminiQuestionChoice {
    pub key: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeminiQuestion {
    pub prompt: String,
    pub choices: Vec<GeminiQuestionChoice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedGeminiStreamJson {
    pub session_id: Option<String>,
    pub summary: String,
    pub usage: UsageSummary,
    pub cost_usd: Option<f64>,
    pub error_message: Option<String>,
    pub result_json: Option<Value>,
    pub question: Option<GeminiQuestion>,
}

impl Default for ParsedGeminiStreamJson {
    fn default() -> Self {
        Self {
            session_id: None,
            summary: String::new(),
            usage: UsageSummary { input_tokens: 0, output_tokens: 0, cached_input_tokens: Some(0) },
            cost_usd: None,
            error_message: None,
            result_json: None,
            question: None,
        }
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> String {
    object.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_owned()
}

fn number_field(object: &serde_json::Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter().find_map(|key| object.get(*key).and_then(Value::as_u64)).unwrap_or(0)
}

fn message_text(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else { return Vec::new() };
    if let Some(text) = value.as_str() {
        return (!text.trim().is_empty()).then_some(text.trim().to_owned()).into_iter().collect();
    }
    let Some(object) = value.as_object() else { return Vec::new() };
    let mut result = Vec::new();
    let direct = string_field(object, "text");
    if !direct.is_empty() { result.push(direct); }
    if let Some(Value::Array(content)) = object.get("content") {
        for part in content {
            let Some(part) = part.as_object() else { continue };
            let kind = string_field(part, "type");
            if matches!(kind.as_str(), "output_text" | "text" | "content") {
                let text = string_field(part, "text");
                let text = if text.is_empty() { string_field(part, "content") } else { text };
                if !text.is_empty() { result.push(text); }
            }
        }
    }
    result
}

fn session_id(object: &serde_json::Map<String, Value>) -> Option<String> {
    ["session_id", "sessionId", "sessionID", "checkpoint_id", "thread_id"].iter().map(|key| string_field(object, key)).find(|value| !value.is_empty())
}

fn error_text(value: Option<&Value>) -> String {
    let Some(value) = value else { return String::new() };
    if let Some(text) = value.as_str() { return text.trim().to_owned(); }
    let Some(object) = value.as_object() else { return String::new() };
    for key in ["message", "error", "code", "detail"] {
        let text = string_field(object, key);
        if !text.is_empty() { return text; }
    }
    serde_json::to_string(object).unwrap_or_default()
}

fn accumulate(usage: &mut UsageSummary, raw: Option<&Value>) {
    let Some(Value::Object(object)) = raw else { return };
    let source = object.get("usageMetadata").and_then(Value::as_object).unwrap_or(object);
    usage.input_tokens += number_field(source, &["input_tokens", "inputTokens", "promptTokenCount"]);
    if let Some(cached) = usage.cached_input_tokens.as_mut() { *cached += number_field(source, &["cached_input_tokens", "cachedInputTokens", "cachedContentTokenCount", "cached"]); }
    usage.output_tokens += number_field(source, &["output_tokens", "outputTokens", "candidatesTokenCount"]);
}

pub fn parse_gemini_stream_json(stdout: &str) -> ParsedGeminiStreamJson {
    let mut parsed = ParsedGeminiStreamJson::default();
    let mut messages = Vec::new();
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() { continue; }
        let Ok(Value::Object(event)) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(id) = session_id(&event) { parsed.session_id = Some(id); }
        let event_type = string_field(&event, "type");
        match event_type.as_str() {
            "assistant" => {
                messages.extend(message_text(event.get("message")));
                if let Some(Value::Object(message)) = event.get("message") {
                    if let Some(Value::Array(content)) = message.get("content") {
                        for part in content {
                            let Some(part) = part.as_object() else { continue };
                            if string_field(part, "type") == "question" {
                                let choices = part.get("choices").and_then(Value::as_array).map(|items| items.iter().filter_map(|item| item.as_object()).map(|item| GeminiQuestionChoice { key: string_field(item, "key"), label: string_field(item, "label"), description: (!string_field(item, "description").is_empty()).then(|| string_field(item, "description")) }).collect()).unwrap_or_default();
                                parsed.question = Some(GeminiQuestion { prompt: string_field(part, "prompt"), choices });
                                break;
                            }
                        }
                    }
                }
            }
            "message" if string_field(&event, "role").to_ascii_lowercase() == "assistant" => messages.extend(message_text(event.get("content"))),
            "result" => {
                accumulate(&mut parsed.usage, event.get("usage").or_else(|| event.get("usageMetadata")).or_else(|| event.get("stats")));
                let result_text = ["result", "text", "response"].iter().map(|key| string_field(&event, key)).find(|value| !value.is_empty());
                if messages.is_empty() { if let Some(text) = result_text { messages.push(text); } }
                parsed.cost_usd = ["total_cost_usd", "cost_usd", "cost"].iter().find_map(|key| event.get(*key).and_then(Value::as_f64));
                let status = string_field(&event, "status").to_ascii_lowercase();
                let is_error = event.get("is_error").and_then(Value::as_bool).unwrap_or(false) || string_field(&event, "subtype").to_ascii_lowercase() == "error" || status == "error" || status == "failed";
                if is_error { let text = error_text(event.get("error").or_else(|| event.get("message")).or_else(|| event.get("result"))); if !text.is_empty() { parsed.error_message = Some(text); } }
                parsed.result_json = Some(Value::Object(event));
            }
            "error" => { let text = error_text(event.get("error").or_else(|| event.get("message")).or_else(|| event.get("detail"))); if !text.is_empty() { parsed.error_message = Some(text); } }
            "system" if string_field(&event, "subtype").to_ascii_lowercase() == "error" => { let text = error_text(event.get("error").or_else(|| event.get("message")).or_else(|| event.get("detail"))); if !text.is_empty() { parsed.error_message = Some(text); } }
            "text" => { if let Some(part) = event.get("part").and_then(Value::as_object) { let text = string_field(part, "text"); if !text.is_empty() { messages.push(text); } } }
            "step_finish" => { accumulate(&mut parsed.usage, event.get("usage").or_else(|| event.get("usageMetadata"))); }
            _ if event.get("usage").is_some() || event.get("usageMetadata").is_some() => accumulate(&mut parsed.usage, event.get("usage").or_else(|| event.get("usageMetadata"))),
            _ => {}
        }
    }
    parsed.summary = messages.join("\n\n").trim().to_owned();
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 聚合message和result_usage() {
        let parsed = parse_gemini_stream_json(r#"{"type":"message","role":"assistant","content":"hello"}
{"type":"result","session_id":"s1","stats":{"input_tokens":10,"cached":3,"output_tokens":4},"result":"done"}"#);
        assert_eq!(parsed.summary, "hello");
        assert_eq!(parsed.session_id.as_deref(), Some("s1"));
        assert_eq!(parsed.usage.input_tokens, 10);
        assert_eq!(parsed.usage.cached_input_tokens, Some(3));
    }

    #[test]
    fn question和错误可解析() {
        let parsed = parse_gemini_stream_json(r#"{"type":"assistant","message":{"content":[{"type":"question","prompt":"继续?","choices":[{"key":"y","label":"是","description":"继续执行"}]}]}}
{"type":"result","status":"error","error":{"message":"boom"}}"#);
        assert_eq!(parsed.question.unwrap().choices[0].key, "y");
        assert_eq!(parsed.error_message.as_deref(), Some("boom"));
    }
}

pub fn is_gemini_session_unrecoverable_error(stdout: &str, stderr: &str) -> bool {
    let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    ["unknown session", "session ", "resume", "checkpoint", "cannot resume", "failed to resume", "maximum number of tokens", "input token count exceeds"].iter().any(|needle| text.contains(needle)) && (text.contains("not found") || text.contains("unknown session") || text.contains("cannot resume") || text.contains("failed to resume") || text.contains("exceeds"))
}

pub fn is_gemini_transient_network_error(stdout: &str, stderr: &str) -> bool {
    let text = format!("{stdout}\n{stderr}");
    ["ENOTFOUND oauth2.googleapis.com", "ENOTFOUND sts.googleapis.com", "EAI_AGAIN", "_GaxiosError", "_UserRefreshClient"].iter().any(|needle| text.contains(needle)) && (text.contains("ENOTFOUND") || text.contains("EAI_AGAIN"))
}

pub fn describe_gemini_failure(parsed: &Value) -> Option<String> {
    let object = parsed.as_object()?;
    let status = string_field(object, "status");
    let detail = error_text(object.get("error").or_else(|| object.get("message")));
    if status.is_empty() && detail.is_empty() { return None; }
    let mut result = "Gemini run failed".to_owned();
    if !status.is_empty() { result.push_str(": status="); result.push_str(&status); }
    if !detail.is_empty() { result.push_str(": "); result.push_str(&detail); }
    Some(result)
}

pub fn detect_gemini_auth_required(parsed: Option<&Value>, stdout: &str, stderr: &str) -> bool {
    let parsed_text = parsed.and_then(|value| value.as_object()).map(|object| error_text(object.get("error").or_else(|| object.get("message")))).unwrap_or_default();
    let text = format!("{parsed_text}\n{stdout}\n{stderr}").to_ascii_lowercase();
    ["not authenticated", "please authenticate", "api key required", "api key missing", "api key invalid", "authentication required", "manual authorization is required", "unauthorized", "invalid credentials", "not logged in", "login required", "gemini auth"].iter().any(|needle| text.contains(needle))
}

pub fn detect_gemini_quota_exhausted(parsed: Option<&Value>, stdout: &str, stderr: &str) -> bool {
    let parsed_text = parsed.and_then(|value| value.as_object()).map(|object| error_text(object.get("error").or_else(|| object.get("message")))).unwrap_or_default();
    let text = format!("{parsed_text}\n{stdout}\n{stderr}").to_ascii_lowercase();
    ["resource_exhausted", "quota", "rate limit", "too many requests", "429", "billing details"].iter().any(|needle| text.contains(needle))
}

pub fn is_gemini_turn_limit_result(parsed: Option<&Value>, exit_code: Option<i32>) -> bool {
    if exit_code == Some(53) { return true; }
    let Some(Value::Object(object)) = parsed else { return false };
    ["status", "stopReason", "stop_reason", "errorCode", "error_code"].iter().filter_map(|key| object.get(*key).and_then(Value::as_str)).map(|value| value.trim().to_ascii_lowercase()).any(|value| matches!(value.as_str(), "turn_limit" | "max_turns" | "max_turns_exhausted" | "turn_limit_exhausted"))
}
