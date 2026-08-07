//! `pc-acpx` transcript helpers — pure functions that mirror
//! `parseAcpxStdoutLine` from Node `acpx-engine/ui.ts`.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ============================================================================
// Public types
// ============================================================================

/// One entry in the agent's transcript. The `kind` tag mirrors the Node
/// `TranscriptEntry` type. All other fields are populated per variant.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntry {
    Init {
        ts: String,
        model: String,
        session_id: Option<String>,
    },
    Thinking {
        ts: String,
        text: String,
        delta: bool,
    },
    Assistant {
        ts: String,
        text: String,
        delta: bool,
    },
    ToolCall {
        ts: String,
        name: String,
        tool_use_id: Option<String>,
        input: Value,
    },
    ToolResult {
        ts: String,
        tool_use_id: String,
        tool_name: Option<String>,
        content: String,
        is_error: bool,
    },
    System {
        ts: String,
        text: String,
    },
    Result {
        ts: String,
        text: String,
        input_tokens: i64,
        output_tokens: i64,
        cached_tokens: i64,
        cost_usd: f64,
        subtype: String,
        is_error: bool,
        errors: Vec<String>,
    },
    Stderr {
        ts: String,
        text: String,
    },
    Stdout {
        ts: String,
        text: String,
    },
}

// ============================================================================
// Main entry
// ============================================================================

/// Parse one acpx-stdout line into zero or more `TranscriptEntry`s. The
/// parser is liberal: lines that fail to parse as JSON fall back to a
/// `stdout` entry containing the raw line.
pub fn parse_acpx_stdout_line(line: &str, ts: &str) -> Vec<TranscriptEntry> {
    let Some(parsed) = parse_json(line) else {
        return vec![TranscriptEntry::Stdout {
            ts: ts.to_string(),
            text: line.to_string(),
        }];
    };

    let type_str = as_string(
        Some(&parsed.get("type").cloned().unwrap_or(Value::Null)),
        "",
    );

    match type_str.as_str() {
        "acpx.session" => vec![init_entry(&parsed, ts)],
        "acpx.text_delta" => text_delta_entries(&parsed, ts),
        "acpx.tool_call" => tool_call_entries(&parsed, ts),
        "acpx.tool_result" => vec![tool_result_entry(&parsed, ts)],
        "acpx.status" => vec![TranscriptEntry::System {
            ts: ts.to_string(),
            text: status_text(&parsed),
        }],
        "acpx.result" => vec![result_entry(&parsed, ts)],
        "acpx.error" => vec![TranscriptEntry::Stderr {
            ts: ts.to_string(),
            text: as_string(parsed.get("message"), line),
        }],
        other if other.starts_with("acpx.") => vec![TranscriptEntry::System {
            ts: ts.to_string(),
            text: as_string(parsed.get("message"), other),
        }],
        _ => vec![TranscriptEntry::Stdout {
            ts: ts.to_string(),
            text: line.to_string(),
        }],
    }
}

// ============================================================================
// Per-variant builders
// ============================================================================

fn init_entry(parsed: &Value, ts: &str) -> TranscriptEntry {
    let agent = as_string(parsed.get("agent"), "acpx");
    let mode = as_string(parsed.get("mode"), "");
    let permission_mode = as_string(parsed.get("permissionMode"), "");
    let tail: Vec<&str> = [mode.as_str(), permission_mode.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    let model = if tail.is_empty() {
        agent
    } else {
        format!("{} ({})", agent, tail.join(" / "))
    };
    let session_id = [
        parsed.get("acpSessionId"),
        parsed.get("sessionId"),
        parsed.get("runtimeSessionName"),
    ]
    .into_iter()
    .find_map(|v| {
        let s = as_string(v, "");
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    });
    TranscriptEntry::Init {
        ts: ts.to_string(),
        model,
        session_id,
    }
}

fn text_delta_entries(parsed: &Value, ts: &str) -> Vec<TranscriptEntry> {
    let text = as_string(parsed.get("text"), "");
    if text.is_empty() {
        return Vec::new();
    }
    let channel = as_string(
        parsed.get("channel"),
        as_string(parsed.get("stream"), "").as_str(),
    );
    if channel == "thought" || channel == "thinking" {
        vec![TranscriptEntry::Thinking {
            ts: ts.to_string(),
            text,
            delta: true,
        }]
    } else {
        vec![TranscriptEntry::Assistant {
            ts: ts.to_string(),
            text,
            delta: true,
        }]
    }
}

fn tool_call_entries(parsed: &Value, ts: &str) -> Vec<TranscriptEntry> {
    let status = as_string(parsed.get("status"), "");
    let text = as_string(parsed.get("text"), "");
    let name = as_string(parsed.get("name"), "acp_tool");
    let tool_use_id = pick_tool_use_id(parsed);
    let input = build_tool_input(parsed, &status, &text);

    let mut entries = vec![TranscriptEntry::ToolCall {
        ts: ts.to_string(),
        name: name.clone(),
        tool_use_id: if tool_use_id.is_empty() {
            None
        } else {
            Some(tool_use_id.clone())
        },
        input,
    }];

    if status == "completed" || status == "failed" || status == "cancelled" {
        let is_error = status != "completed";
        entries.push(TranscriptEntry::ToolResult {
            ts: ts.to_string(),
            tool_use_id: if tool_use_id.is_empty() {
                name.clone()
            } else {
                tool_use_id
            },
            tool_name: Some(name),
            content: if text.is_empty() {
                status.clone()
            } else {
                text
            },
            is_error,
        });
    }

    entries
}

fn tool_result_entry(parsed: &Value, ts: &str) -> TranscriptEntry {
    let tool_use_id = pick_tool_use_id(parsed);
    let fallback_name = as_string(parsed.get("name"), "acp_tool");
    let tool_use_id = if tool_use_id.is_empty() {
        fallback_name
    } else {
        tool_use_id
    };
    let tool_name = parsed
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let content = stringify(
        parsed
            .get("content")
            .or_else(|| parsed.get("output"))
            .or_else(|| parsed.get("error"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    let is_error = parsed
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || parsed.get("error").is_some();
    TranscriptEntry::ToolResult {
        ts: ts.to_string(),
        tool_use_id,
        tool_name,
        content,
        is_error,
    }
}

fn result_entry(parsed: &Value, ts: &str) -> TranscriptEntry {
    let text = as_string(
        parsed.get("summary"),
        as_string(
            parsed.get("stopReason"),
            as_string(parsed.get("text"), "").as_str(),
        )
        .as_str(),
    );
    let subtype = as_string(
        parsed.get("subtype"),
        as_string(parsed.get("stopReason"), "acpx.result").as_str(),
    );
    let errors: Vec<String> = parsed
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| stringify(item.clone()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    TranscriptEntry::Result {
        ts: ts.to_string(),
        text,
        input_tokens: as_number_i64(parsed.get("inputTokens"), 0),
        output_tokens: as_number_i64(parsed.get("outputTokens"), 0),
        cached_tokens: as_number_i64(parsed.get("cachedTokens"), 0),
        cost_usd: as_f64(parsed.get("costUsd")),
        subtype,
        is_error: parsed
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        errors,
    }
}

fn build_tool_input(parsed: &Value, status: &str, text: &str) -> Value {
    let Some(input) = parsed.get("input") else {
        let mut object = serde_json::Map::new();
        if !text.is_empty() {
            object.insert("text".to_string(), Value::String(text.to_string()));
        }
        if !status.is_empty() {
            object.insert("status".to_string(), Value::String(status.to_string()));
        }
        return Value::Object(object);
    };

    let Some(record) = input.as_object() else {
        return input.clone();
    };

    let mut map = record.clone();
    if !status.is_empty() && map.get("status").is_none() {
        map.insert("status".to_string(), Value::String(status.to_string()));
    }
    if !text.is_empty() && map.get("text").is_none() {
        map.insert("text".to_string(), Value::String(text.to_string()));
    }
    Value::Object(map)
}

fn pick_tool_use_id(parsed: &Value) -> String {
    let candidates = ["toolCallId", "toolUseId", "id"];
    for key in candidates {
        let value = as_string(parsed.get(key), "");
        if !value.is_empty() {
            return value;
        }
    }
    String::new()
}

fn status_text(parsed: &Value) -> String {
    let text = as_string(parsed.get("text"), "").trim().to_string();
    let tag = as_string(parsed.get("tag"), "").trim().to_string();
    let used = as_number_i64(parsed.get("used"), -1);
    let size = as_number_i64(parsed.get("size"), -1);
    let mut parts: Vec<String> = Vec::new();
    if !text.is_empty() {
        parts.push(text);
    } else if !tag.is_empty() {
        parts.push(tag.clone());
    }
    if used >= 0 && size > 0 {
        parts.push(format!("({used}/{size} ctx)"));
    }
    if parts.is_empty() {
        if !tag.is_empty() {
            tag
        } else {
            "status".to_string()
        }
    } else {
        parts.join(" ")
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn parse_json(line: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if !value.is_object() {
        return None;
    }
    Some(value)
}

fn as_string(value: Option<&Value>, fallback: &str) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        _ => fallback.to_string(),
    }
}

fn as_number_i64(value: Option<&Value>, fallback: i64) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().map(|v| v as i64).unwrap_or(fallback),
        _ => fallback,
    }
}

fn as_f64(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn stringify(value: Value) -> String {
    match value {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => serde_json::to_string_pretty(&other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Render an `acpx.tool_call` summary suitable for display in UI rows.
pub fn summarize_tool_call(entry: &TranscriptEntry) -> Option<ToolCallSummary> {
    let TranscriptEntry::ToolCall { name, input, .. } = entry else {
        return None;
    };
    let title = input
        .get("title")
        .or_else(|| input.get("command"))
        .or_else(|| input.get("path"))
        .or_else(|| input.get("url"))
        .or_else(|| input.get("query"))
        .or_else(|| input.get("pattern"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(ToolCallSummary {
        name: name.clone(),
        title,
        detail: summarize_input(input),
    })
}

/// A flattened view of a `ToolCall` entry, useful for log lines.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallSummary {
    pub name: String,
    pub title: Option<String>,
    pub detail: BTreeMap<String, String>,
}

fn summarize_input(value: &Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(object) = value.as_object() else {
        return map;
    };
    for (key, value) in object {
        let rendered = stringify(value.clone());
        if rendered.is_empty() {
            continue;
        }
        map.insert(key.clone(), rendered);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_json_lines_fall_back_to_stdout() {
        let entries = parse_acpx_stdout_line("not json", "2026-01-01T00:00:00Z");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::Stdout { ts, text } => {
                assert_eq!(ts, "2026-01-01T00:00:00Z");
                assert_eq!(text, "not json");
            }
            other => panic!("expected stdout, got {other:?}"),
        }
    }

    #[test]
    fn parses_session_event() {
        let line = serde_json::json!({
            "type": "acpx.session",
            "agent": "claude",
            "mode": "persistent",
            "permissionMode": "approve-all",
            "acpSessionId": "acp-1"
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&line, "ts");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::Init {
                model, session_id, ..
            } => {
                assert_eq!(model, "claude (persistent / approve-all)");
                assert_eq!(session_id.as_deref(), Some("acp-1"));
            }
            other => panic!("expected init, got {other:?}"),
        }
    }

    #[test]
    fn parses_text_delta_to_thinking_or_assistant() {
        let thought = serde_json::json!({
            "type": "acpx.text_delta",
            "channel": "thought",
            "text": "let me think"
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&thought, "ts");
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], TranscriptEntry::Thinking { .. }));

        let assistant = serde_json::json!({
            "type": "acpx.text_delta",
            "channel": "assistant",
            "text": "hello"
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&assistant, "ts");
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], TranscriptEntry::Assistant { .. }));
    }

    #[test]
    fn parses_tool_call_and_emits_result_when_status_is_terminal() {
        let line = serde_json::json!({
            "type": "acpx.tool_call",
            "name": "exec",
            "toolCallId": "tc-1",
            "status": "completed",
            "text": "ok",
            "input": { "command": "ls" }
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&line, "ts");
        assert_eq!(entries.len(), 2);
        match &entries[0] {
            TranscriptEntry::ToolCall {
                name,
                tool_use_id,
                input,
                ..
            } => {
                assert_eq!(name, "exec");
                assert_eq!(tool_use_id.as_deref(), Some("tc-1"));
                assert_eq!(input.get("command").and_then(|v| v.as_str()), Some("ls"));
            }
            other => panic!("expected tool_call, got {other:?}"),
        }
        match &entries[1] {
            TranscriptEntry::ToolResult {
                tool_use_id,
                is_error,
                content,
                ..
            } => {
                assert_eq!(tool_use_id, "tc-1");
                assert!(!is_error);
                assert_eq!(content, "ok");
            }
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[test]
    fn parses_result_event() {
        let line = serde_json::json!({
            "type": "acpx.result",
            "summary": "done",
            "inputTokens": 10,
            "outputTokens": 20,
            "cachedTokens": 3,
            "costUsd": 0.01,
            "subtype": "success",
            "isError": false,
            "errors": []
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&line, "ts");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::Result {
                text,
                input_tokens,
                output_tokens,
                cached_tokens,
                cost_usd,
                subtype,
                is_error,
                errors,
                ts: _,
            } => {
                assert_eq!(text, "done");
                assert_eq!(*input_tokens, 10);
                assert_eq!(*output_tokens, 20);
                assert_eq!(*cached_tokens, 3);
                assert!((cost_usd - 0.01).abs() < 1e-9);
                assert_eq!(subtype, "success");
                assert!(!is_error);
                assert!(errors.is_empty());
            }
            other => panic!("expected result, got {other:?}"),
        }
    }

    #[test]
    fn parses_error_event_to_stderr() {
        let line = serde_json::json!({
            "type": "acpx.error",
            "message": "boom"
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&line, "ts");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::Stderr { text, .. } => assert_eq!(text, "boom"),
            other => panic!("expected stderr, got {other:?}"),
        }
    }

    #[test]
    fn parses_status_event() {
        let line = serde_json::json!({
            "type": "acpx.status",
            "text": "heartbeat",
            "used": 5,
            "size": 10
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&line, "ts");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::System { text, .. } => assert_eq!(text, "heartbeat (5/10 ctx)"),
            other => panic!("expected system, got {other:?}"),
        }
    }

    #[test]
    fn parses_unrecognized_acpx_event_to_system() {
        let line = serde_json::json!({
            "type": "acpx.something_else",
            "message": "weird"
        })
        .to_string();
        let entries = parse_acpx_stdout_line(&line, "ts");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::System { text, .. } => assert_eq!(text, "weird"),
            other => panic!("expected system, got {other:?}"),
        }
    }

    #[test]
    fn summarize_tool_call_extracts_known_fields() {
        let entry = TranscriptEntry::ToolCall {
            ts: "ts".into(),
            name: "exec".into(),
            tool_use_id: Some("tc-1".into()),
            input: serde_json::json!({
                "command": "ls",
                "cwd": "/tmp",
                "title": "list repo"
            }),
        };
        let summary = summarize_tool_call(&entry).expect("summary");
        assert_eq!(summary.name, "exec");
        assert_eq!(summary.title.as_deref(), Some("list repo"));
        assert_eq!(
            summary.detail.get("command").map(String::as_str),
            Some("ls")
        );
        assert_eq!(summary.detail.get("cwd").map(String::as_str), Some("/tmp"));
    }
}
