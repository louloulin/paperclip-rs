//! Grok streaming-json 输出解析器。
//!
//! 该模块只负责协议解析，不负责进程启动或 Paperclip 生命周期，便于
//! 与 Node `server/parse.ts` 做逐事件对照测试。

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedGrokJsonl {
    pub session_id: Option<String>,
    pub summary: String,
    pub thought: String,
    pub error_message: Option<String>,
    pub stop_reason: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Debug, Default)]
struct TurnBoundaryState {
    last_chunk: String,
    backtick_parity: bool,
}

fn count_backticks(text: &str) -> usize {
    text.chars().filter(|character| *character == '`').count()
}

fn ends_with_sentence_close(character: char) -> bool {
    matches!(character, '.' | '?' | '!' | ':' | ';')
}

fn apply_turn_boundary(state: &mut TurnBoundaryState, incoming: &str) -> String {
    if incoming.is_empty() {
        return String::new();
    }
    let mut output = incoming.to_owned();
    let previous = state.last_chunk.as_str();
    let starts_uppercase = incoming.chars().next().is_some_and(char::is_uppercase);
    let ends_whitespace = previous.chars().last().is_some_and(char::is_whitespace);
    let starts_whitespace = incoming.chars().next().is_some_and(char::is_whitespace);
    if !previous.is_empty()
        && !ends_whitespace
        && !starts_whitespace
        && starts_uppercase
        && incoming.chars().count() >= 2
    {
        let last_character = previous.chars().last().unwrap_or_default();
        let closing_lone_backtick = previous == "`" && !state.backtick_parity;
        if ends_with_sentence_close(last_character) || closing_lone_backtick {
            output.insert(0, '\n');
        }
    }
    state.last_chunk = incoming.to_owned();
    if count_backticks(incoming) % 2 == 1 {
        state.backtick_parity = !state.backtick_parity;
    }
    output
}

fn string_value(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn error_text(value: Option<&Value>) -> String {
    let Some(value) = value else { return String::new() };
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    let Some(object) = value.as_object() else {
        return String::new();
    };
    for key in ["message", "error", "detail", "code"] {
        if let Some(text) = object.get(key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_owned();
            }
        }
    }
    serde_json::to_string(object).unwrap_or_default()
}

pub fn parse_grok_jsonl(stdout: &str) -> ParsedGrokJsonl {
    let mut parsed = ParsedGrokJsonl::default();
    let mut thought_parts = Vec::new();
    let mut text_parts = Vec::new();
    let mut boundary = TurnBoundaryState::default();

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(event)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match string_value(event.get("type")).trim() {
            "thought" => {
                let text = string_value(event.get("data"));
                if !text.is_empty() {
                    thought_parts.push(apply_turn_boundary(&mut boundary, &text));
                }
            }
            "text" => {
                let text = string_value(event.get("data"));
                if !text.is_empty() {
                    text_parts.push(text);
                }
            }
            "end" => {
                let session_id = string_value(event.get("sessionId"));
                if !session_id.trim().is_empty() {
                    parsed.session_id = Some(session_id.trim().to_owned());
                }
                let stop_reason = string_value(event.get("stopReason"));
                if !stop_reason.trim().is_empty() {
                    parsed.stop_reason = Some(stop_reason.trim().to_owned());
                }
                let request_id = string_value(event.get("requestId"));
                if !request_id.trim().is_empty() {
                    parsed.request_id = Some(request_id.trim().to_owned());
                }
            }
            "error" => {
                let value = event
                    .get("error")
                    .or_else(|| event.get("message"))
                    .or_else(|| event.get("detail"))
                    .or_else(|| event.get("data"));
                let text = error_text(value);
                if !text.trim().is_empty() {
                    parsed.error_message = Some(text.trim().to_owned());
                }
            }
            _ => {}
        }
    }
    parsed.summary = text_parts.join("").trim().to_owned();
    parsed.thought = thought_parts.join("").trim().to_owned();
    parsed
}

pub fn is_grok_unknown_session_error(stdout: &str, stderr: &str) -> bool {
    let haystack = format!("{stdout}\n{stderr}");
    let lower = haystack.to_ascii_lowercase();
    lower.lines().filter(|line| !line.trim().is_empty()).any(|line| {
        let line = line.trim();
        line.contains("unknown session")
            || line.contains("session") && line.contains("not found")
            || line.contains("resume") && line.contains("not found")
            || line.contains("invalid session")
    })
}

#[allow(dead_code)]
fn _keep_map_import(_: Map<String, Value>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 聚合文本和结束元数据() {
        let parsed = parse_grok_jsonl(
            r#"{"type":"text","data":"hel"}
{"type":"text","data":"lo"}
{"type":"end","stopReason":"EndTurn","sessionId":"sess-1","requestId":"req-1"}"#,
        );
        assert_eq!(parsed.summary, "hello");
        assert_eq!(parsed.session_id.as_deref(), Some("sess-1"));
        assert_eq!(parsed.stop_reason.as_deref(), Some("EndTurn"));
        assert_eq!(parsed.request_id.as_deref(), Some("req-1"));
    }

    #[test]
    fn 结构化错误优先读取_message() {
        let parsed = parse_grok_jsonl(r#"{"type":"error","error":{"message":"Authentication required"}}"#);
        assert_eq!(parsed.error_message.as_deref(), Some("Authentication required"));
    }

    #[test]
    fn 推理跨回合插入换行() {
        let parsed = parse_grok_jsonl(
            r#"{"type":"thought","data":"Done."}
{"type":"thought","data":"Next"}"#,
        );
        assert_eq!(parsed.thought, "Done.\nNext");
    }

    #[test]
    fn 非法行不会污染结果() {
        let parsed = parse_grok_jsonl("noise\n{}\n\nnot json");
        assert_eq!(parsed, ParsedGrokJsonl::default());
    }

    #[test]
    fn 空输出返回默认结果() {
        assert_eq!(parse_grok_jsonl(""), ParsedGrokJsonl::default());
    }

    #[test]
    fn 未知会话错误识别大小写和常见措辞() {
        assert!(is_grok_unknown_session_error("", "Session not found"));
        assert!(is_grok_unknown_session_error("", "invalid session id"));
        assert!(!is_grok_unknown_session_error("", "everything fine"));
    }
}
