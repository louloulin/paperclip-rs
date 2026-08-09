//! Cursor stream-json 解析和 session 错误判断。

use pc_adapter_api::UsageSummary;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCursorStreamJson {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub summary: String,
    pub usage: UsageSummary,
    pub cost_usd: Option<f64>,
    pub error_message: Option<String>,
    pub result_json: Option<Value>,
}

impl Default for ParsedCursorStreamJson {
    fn default() -> Self {
        Self {
            session_id: None,
            model: None,
            summary: String::new(),
            usage: UsageSummary {
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: Some(0),
            },
            cost_usd: None,
            error_message: None,
            result_json: None,
        }
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
fn number_field(object: &serde_json::Map<String, Value>, key: &str) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn message_text(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(text) = value.as_str() {
        return (!text.trim().is_empty())
            .then_some(text.trim().to_owned())
            .into_iter()
            .collect();
    }
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let direct = string_field(object, "text");
    if !direct.is_empty() {
        lines.push(direct);
    }
    if let Some(Value::Array(content)) = object.get("content") {
        for item in content {
            let Some(item) = item.as_object() else {
                continue;
            };
            if matches!(string_field(item, "type").as_str(), "output_text" | "text") {
                let text = string_field(item, "text");
                if !text.is_empty() {
                    lines.push(text);
                }
            }
        }
    }
    lines
}

fn error_text(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if let Some(text) = value.as_str() {
        return text.trim().to_owned();
    }
    let Some(object) = value.as_object() else {
        return String::new();
    };
    for key in ["message", "error", "code", "detail"] {
        let text = string_field(object, key);
        if !text.is_empty() {
            return text;
        }
    }
    serde_json::to_string(object).unwrap_or_default()
}

pub fn normalize_cursor_stream_line(raw: &str) -> String {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in ["stdout", "stderr"] {
        if lower.starts_with(prefix) {
            let rest = trimmed[prefix.len()..]
                .trim_start_matches([' ', ':', '='])
                .trim();
            if rest.starts_with('{') || rest.starts_with('[') {
                return rest.to_owned();
            }
        }
    }
    trimmed.to_owned()
}

pub fn parse_cursor_stream_json(stdout: &str) -> ParsedCursorStreamJson {
    let mut parsed = ParsedCursorStreamJson::default();
    let mut messages = Vec::new();
    let mut total_cost = 0.0;
    for raw in stdout.lines() {
        let line = normalize_cursor_stream_line(raw);
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(event)) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = ["session_id", "sessionId", "sessionID"]
            .iter()
            .map(|key| string_field(&event, key))
            .find(|value| !value.is_empty())
        {
            parsed.session_id = Some(id);
        }
        let event_type = string_field(&event, "type");
        match event_type.as_str() {
            "system" => {
                let model = string_field(&event, "model");
                if !model.is_empty() {
                    parsed.model = Some(model);
                }
            }
            "assistant" => messages.extend(message_text(event.get("message"))),
            "result" => {
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    parsed.usage.input_tokens +=
                        number_field(usage, "input_tokens").max(number_field(usage, "inputTokens"));
                    parsed.usage.output_tokens += number_field(usage, "output_tokens")
                        .max(number_field(usage, "outputTokens"));
                    if let Some(cached) = parsed.usage.cached_input_tokens.as_mut() {
                        *cached += number_field(usage, "cached_input_tokens")
                            .max(number_field(usage, "cachedInputTokens"))
                            .max(number_field(usage, "cache_read_input_tokens"));
                    }
                }
                total_cost += event
                    .get("total_cost_usd")
                    .or_else(|| event.get("cost_usd"))
                    .or_else(|| event.get("cost"))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let text = string_field(&event, "result");
                let is_error = event
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || string_field(&event, "subtype").to_ascii_lowercase() == "error";
                if is_error {
                    let err = error_text(
                        event
                            .get("error")
                            .or_else(|| event.get("message"))
                            .or_else(|| event.get("result")),
                    );
                    if !err.is_empty() {
                        parsed.error_message = Some(err);
                    }
                    if !text.is_empty() {
                        messages.push(text);
                    }
                } else if !text.is_empty() {
                    if messages.is_empty() {
                        messages.push(text.clone());
                    } else {
                        *messages.last_mut().unwrap() = text;
                    }
                }
                parsed.result_json = Some(Value::Object(event));
                let _ = is_error;
            }
            "error" => {
                let text = error_text(
                    event
                        .get("message")
                        .or_else(|| event.get("error"))
                        .or_else(|| event.get("detail")),
                );
                if !text.is_empty() {
                    parsed.error_message = Some(text);
                }
            }
            "text" => {
                if let Some(part) = event.get("part").and_then(Value::as_object) {
                    let text = string_field(part, "text");
                    if !text.is_empty() {
                        messages.push(text);
                    }
                }
            }
            "step_finish" => {
                if let Some(part) = event.get("part").and_then(Value::as_object) {
                    if let Some(tokens) = part.get("tokens").and_then(Value::as_object) {
                        parsed.usage.input_tokens += number_field(tokens, "input");
                        parsed.usage.output_tokens += number_field(tokens, "output");
                        if let Some(cache) = tokens.get("cache").and_then(Value::as_object) {
                            if let Some(cached) = parsed.usage.cached_input_tokens.as_mut() {
                                *cached += number_field(cache, "read");
                            }
                        }
                    }
                    total_cost += part.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                }
            }
            _ => {}
        }
    }
    parsed.summary = messages.join("\n\n").trim().to_owned();
    parsed.cost_usd = (total_cost > 0.0).then_some(total_cost);
    parsed
}

pub fn is_cursor_unknown_session_error(stdout: &str, stderr: &str) -> bool {
    let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    [
        "unknown session",
        "unknown chat",
        "session ",
        "chat ",
        "resume",
        "could not resume",
    ]
    .iter()
    .any(|needle| text.contains(needle))
        && (text.contains("not found")
            || text.contains("unknown")
            || text.contains("could not resume"))
}
