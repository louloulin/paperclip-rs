//! Claude Code `stream-json` 协议解析。
//! 只处理 JSONL 协议，不耦合进程执行和 adapter 生命周期。

use pc_adapter_api::UsageSummary;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedClaudeStreamJson {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub usage: Option<UsageSummary>,
    pub usage_basis_per_run: bool,
    pub summary: String,
    pub result_json: Option<Value>,
    pub error_message: Option<String>,
    pub stop_reason: Option<String>,
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn number_field(object: &serde_json::Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_u64))
        .unwrap_or(0)
}

pub fn claude_model_usage_totals(model_usage: Option<&Value>) -> Option<UsageSummary> {
    let Some(Value::Object(models)) = model_usage else { return None };
    let mut totals = UsageSummary {
        input_tokens: 0,
        output_tokens: 0,
        cached_input_tokens: Some(0),
    };
    let mut saw_entry = false;
    for value in models.values() {
        let Value::Object(entry) = value else { continue };
        if entry.is_empty() { continue; }
        saw_entry = true;
        totals.input_tokens += number_field(entry, &["inputTokens"]) + number_field(entry, &["cacheCreationInputTokens"]);
        totals.output_tokens += number_field(entry, &["outputTokens"]);
        if let Some(cached) = totals.cached_input_tokens.as_mut() {
            *cached += number_field(entry, &["cacheReadInputTokens"]);
        }
    }
    saw_entry.then_some(totals)
}

pub fn parse_claude_stream_json(stdout: &str) -> ParsedClaudeStreamJson {
    let mut parsed = ParsedClaudeStreamJson::default();
    let mut assistant_texts = Vec::new();
    let mut final_result = None;

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() { continue; }
        let Ok(Value::Object(event)) = serde_json::from_str::<Value>(line) else { continue };
        match string_field(&event, "type").as_deref() {
            Some("system") if string_field(&event, "subtype").as_deref() == Some("init") => {
                if let Some(session) = string_field(&event, "session_id") { parsed.session_id = Some(session); }
                if let Some(model) = string_field(&event, "model") { parsed.model = Some(model); }
            }
            Some("assistant") => {
                if let Some(session) = string_field(&event, "session_id") { parsed.session_id = Some(session); }
                if let Some(Value::Object(message)) = event.get("message") {
                    if let Some(Value::Array(content)) = message.get("content") {
                        for block in content {
                            let Value::Object(block) = block else { continue };
                            if string_field(block, "type").as_deref() == Some("text") {
                                if let Some(text) = string_field(block, "text") { assistant_texts.push(text); }
                            }
                        }
                    }
                }
            }
            Some("result") => {
                if let Some(session) = string_field(&event, "session_id") { parsed.session_id = Some(session); }
                if let Some(model) = string_field(&event, "model") { parsed.model = Some(model); }
                parsed.stop_reason = string_field(&event, "stop_reason");
                if event.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                    parsed.error_message = string_field(&event, "result").or_else(|| string_field(&event, "subtype").map(|s| format!("error_subtype={s}")));
                }
                final_result = Some(Value::Object(event));
            }
            _ => {}
        }
    }

    let Some(result) = final_result else {
        parsed.summary = assistant_texts.join("\n\n").trim().to_owned();
        return parsed;
    };
    let object = result.as_object().expect("result object");
    parsed.usage = claude_model_usage_totals(object.get("modelUsage")).or_else(|| {
        object.get("usage").and_then(Value::as_object).map(|usage| UsageSummary {
            input_tokens: number_field(usage, &["input_tokens"]),
            output_tokens: number_field(usage, &["output_tokens"]),
            cached_input_tokens: Some(number_field(usage, &["cache_read_input_tokens", "cached_input_tokens"])),
        })
    });
    parsed.usage_basis_per_run = parsed.usage.is_some();
    parsed.cost_usd = object.get("total_cost_usd").and_then(Value::as_f64);
    parsed.summary = string_field(object, "result").unwrap_or_else(|| assistant_texts.join("\n\n")).trim().to_owned();
    parsed.result_json = Some(result);
    parsed
}

fn messages_from_result(parsed: &Value) -> Vec<String> {
    let Some(object) = parsed.as_object() else { return Vec::new() };
    let mut messages = Vec::new();
    if let Some(text) = string_field(object, "result") { messages.push(text); }
    if let Some(Value::Array(errors)) = object.get("errors") {
        for error in errors {
            match error {
                Value::String(text) if !text.trim().is_empty() => messages.push(text.trim().to_owned()),
                Value::Object(error) => {
                    if let Some(text) = string_field(error, "message").or_else(|| string_field(error, "error")).or_else(|| string_field(error, "code")) { messages.push(text); }
                    else if let Ok(text) = serde_json::to_string(error) { messages.push(text); }
                }
                _ => {}
            }
        }
    }
    messages
}

pub fn extract_claude_login_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|candidate| candidate.trim_matches(|c: char| "])}.!?,;:'\"".contains(c)))
        .find(|candidate| candidate.starts_with("http://") || candidate.starts_with("https://"))
        .map(ToOwned::to_owned)
}

pub fn detect_claude_login_required(parsed: Option<&Value>, stdout: &str, stderr: &str) -> bool {
    let mut haystack = String::new();
    if let Some(parsed) = parsed { for message in messages_from_result(parsed) { haystack.push_str(&message); haystack.push('\n'); } }
    haystack.push_str(stdout); haystack.push('\n'); haystack.push_str(stderr);
    let lower = haystack.to_ascii_lowercase();
    ["not logged in", "please log in", "please run claude login", "please run /login", "login required", "requires login", "unauthorized", "authentication required", "invalid api key"]
        .iter().any(|needle| lower.contains(needle))
}

pub fn is_claude_unknown_session_error(parsed: &Value) -> bool {
    messages_from_result(parsed).iter().any(|message| {
        let lower = message.to_ascii_lowercase();
        lower.contains("no conversation found with session id") || lower.contains("unknown session") || lower.contains("session ") && lower.contains("not found") || lower.contains("not a valid uuid") || lower.contains("--resume requires a valid session") || lower.contains("does not match any session title")
    })
}

pub fn is_claude_image_processing_error(parsed: &Value) -> bool {
    messages_from_result(parsed).iter().any(|message| message.to_ascii_lowercase().contains("could not process image"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析最终结果和assistant文本() {
        let parsed = parse_claude_stream_json(
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"opus"}
{"type":"assistant","session_id":"s1","message":{"content":[{"type":"text","text":"hello"}]}}
{"type":"result","session_id":"s1","result":"done","total_cost_usd":1.2,"usage":{"input_tokens":10,"output_tokens":4,"cache_read_input_tokens":2}}"#,
        );
        assert_eq!(parsed.summary, "done");
        assert_eq!(parsed.model.as_deref(), Some("opus"));
        assert_eq!(parsed.usage.unwrap().output_tokens, 4);
        assert_eq!(parsed.cost_usd, Some(1.2));
    }

    #[test]
    fn model_usage优先且缓存创建计入输入() {
        let parsed = parse_claude_stream_json(r#"{"type":"result","result":"ok","modelUsage":{"opus":{"inputTokens":10,"cacheCreationInputTokens":3,"cacheReadInputTokens":20,"outputTokens":4}}}"#);
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.input_tokens, 13);
        assert_eq!(usage.cached_input_tokens, Some(20));
    }

    #[test]
    fn 登录和未知会话识别() {
        assert!(detect_claude_login_required(None, "", "Authentication required"));
        assert!(is_claude_unknown_session_error(&serde_json::json!({"result":"No conversation found with session id x"})));
        assert!(is_claude_image_processing_error(&serde_json::json!({"result":"400 Could not process image"})));
    }
}
