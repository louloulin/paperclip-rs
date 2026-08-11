//! OpenClaw Gateway stdout parser — 把 stdout/stderr 行解析为 Paperclip
//! TranscriptEntry 形状。

#![allow(dead_code)]

use std::sync::OnceLock;

use regex_lite::Regex;
use serde_json::{json, Value};

const EVENT_LINE_TAG: &str = "[openclaw-gateway:event]";
const SYS_LINE_TAG: &str = "[openclaw-gateway]";

const EVENT_LINE_RE_STR: &str =
    r"^\[openclaw-gateway:event\]\s+run=(\S+)\s+stream=(\S+)\s+data=(.*)$";
const SYS_LINE_PREFIX_STR: &str = r"^\[openclaw-gateway\]\s*";

fn event_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(EVENT_LINE_RE_STR).expect("valid event regex"))
}

fn sys_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(SYS_LINE_PREFIX_STR).expect("valid sys regex"))
}

/// Stream 来源（用于 onLog 判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSource {
    Stdout,
    Stderr,
}

/// TranscriptEntry kind（与 Paperclip UI 兼容）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    Assistant,
    Stderr,
    System,
    Stdout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub kind: EntryKind,
    pub text: String,
    /// `delta=true` 表示追加文本而非替换。
    pub delta: bool,
}

/// 把原始行归一化（按来源剥离 `[stderr]` 前缀）。
pub fn normalize_stream_line(line: &str) -> (StreamSource, String) {
    if let Some(rest) = line.strip_prefix("[stderr]") {
        (StreamSource::Stderr, rest.to_owned())
    } else {
        (StreamSource::Stdout, line.to_owned())
    }
}

/// 解析 `[openclaw-gateway:event]` 行 → TranscriptEntry 列表。
pub fn parse_event_line(line: &str) -> Vec<TranscriptEntry> {
    let line = line.trim();
    if !line.starts_with(EVENT_LINE_TAG) {
        return vec![TranscriptEntry {
            kind: EntryKind::Stdout,
            text: line.to_owned(),
            delta: false,
        }];
    }
    let captures = match event_re().captures(line) {
        Some(c) => c,
        None => {
            return vec![TranscriptEntry {
                kind: EntryKind::Stdout,
                text: line.to_owned(),
                delta: false,
            }]
        }
    };
    let stream = captures
        .get(2)
        .map(|m| m.as_str().to_lowercase())
        .unwrap_or_default();
    let data_str = captures.get(3).map(|m| m.as_str()).unwrap_or("").trim();
    let data: Value = serde_json::from_str::<Value>(data_str)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));

    match stream.as_str() {
        "assistant" => {
            let delta = data.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if !delta.is_empty() {
                return vec![TranscriptEntry {
                    kind: EntryKind::Assistant,
                    text: delta.to_owned(),
                    delta: true,
                }];
            }
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !text.is_empty() {
                return vec![TranscriptEntry {
                    kind: EntryKind::Assistant,
                    text: text.to_owned(),
                    delta: false,
                }];
            }
            Vec::new()
        }
        "error" => {
            let message = data
                .get("error")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("message").and_then(|v| v.as_str()))
                .unwrap_or("");
            if !message.is_empty() {
                vec![TranscriptEntry {
                    kind: EntryKind::Stderr,
                    text: message.to_owned(),
                    delta: false,
                }]
            } else {
                Vec::new()
            }
        }
        "lifecycle" => {
            let phase = data
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let message = data
                .get("error")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("message").and_then(|v| v.as_str()))
                .unwrap_or("");
            if matches!(phase.as_str(), "error" | "failed" | "cancelled") && !message.is_empty() {
                vec![TranscriptEntry {
                    kind: EntryKind::Stderr,
                    text: message.to_owned(),
                    delta: false,
                }]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// 公开顶层入口：解析一行原始 stdout（含可能的 `[stderr]` 前缀）。
pub fn parse_stdout_line(line: &str) -> Vec<TranscriptEntry> {
    let (source, stripped) = normalize_stream_line(line);
    if source == StreamSource::Stderr {
        return vec![TranscriptEntry {
            kind: EntryKind::Stderr,
            text: stripped,
            delta: false,
        }];
    }

    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if trimmed.starts_with(EVENT_LINE_TAG) {
        return parse_event_line(trimmed);
    }

    if trimmed.starts_with(SYS_LINE_TAG) {
        let text = sys_prefix_re().replace(trimmed, "").to_string();
        return vec![TranscriptEntry {
            kind: EntryKind::System,
            text,
            delta: false,
        }];
    }

    vec![TranscriptEntry {
        kind: EntryKind::Stdout,
        text: stripped,
        delta: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_stderr_prefix() {
        let (s, body) = normalize_stream_line("[stderr]hello");
        assert_eq!(s, StreamSource::Stderr);
        assert_eq!(body, "hello");
    }

    #[test]
    fn normalize_keeps_stdout_intact() {
        let (s, body) = normalize_stream_line("plain stdout line");
        assert_eq!(s, StreamSource::Stdout);
        assert_eq!(body, "plain stdout line");
    }

    #[test]
    fn parse_event_line_assistant_delta() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=assistant data={"delta":"hello"}"#;
        let entries = parse_event_line(line);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, EntryKind::Assistant);
        assert_eq!(entries[0].text, "hello");
        assert!(entries[0].delta);
    }

    #[test]
    fn parse_event_line_assistant_text() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=assistant data={"text":"hi"}"#;
        let entries = parse_event_line(line);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, EntryKind::Assistant);
        assert_eq!(entries[0].text, "hi");
        assert!(!entries[0].delta);
    }

    #[test]
    fn parse_event_line_error_includes_message() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=error data={"error":"boom"}"#;
        let entries = parse_event_line(line);
        assert_eq!(entries[0].kind, EntryKind::Stderr);
        assert_eq!(entries[0].text, "boom");
    }

    #[test]
    fn parse_event_line_error_fallback_to_message_key() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=error data={"message":"oh no"}"#;
        let entries = parse_event_line(line);
        assert_eq!(entries[0].kind, EntryKind::Stderr);
        assert_eq!(entries[0].text, "oh no");
    }

    #[test]
    fn parse_event_line_lifecycle_error_phase() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=lifecycle data={"phase":"error","message":"timeout"}"#;
        let entries = parse_event_line(line);
        assert_eq!(entries[0].kind, EntryKind::Stderr);
        assert_eq!(entries[0].text, "timeout");
    }

    #[test]
    fn parse_event_line_lifecycle_succeeded_phase_ignored() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=lifecycle data={"phase":"completed","message":"ok"}"#;
        let entries = parse_event_line(line);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_event_line_unknown_stream_returns_empty() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=other data={"x":"y"}"#;
        let entries = parse_event_line(line);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_stdout_line_routes_stderr_to_stderr_kind() {
        let entries = parse_stdout_line("[stderr]something failed");
        assert_eq!(entries[0].kind, EntryKind::Stderr);
        assert_eq!(entries[0].text, "something failed");
    }

    #[test]
    fn parse_stdout_line_routes_sys_message() {
        let entries = parse_stdout_line("[openclaw-gateway] starting up");
        assert_eq!(entries[0].kind, EntryKind::System);
        assert_eq!(entries[0].text, "starting up");
    }

    #[test]
    fn parse_stdout_line_event_routed() {
        let entries = parse_stdout_line(
            r#"[openclaw-gateway:event] run=r-1 stream=assistant data={"text":"hi"}"#,
        );
        assert_eq!(entries[0].kind, EntryKind::Assistant);
        assert_eq!(entries[0].text, "hi");
    }

    #[test]
    fn parse_stdout_line_unrecognized_routed_to_stdout() {
        let entries = parse_stdout_line("random free-form text");
        assert_eq!(entries[0].kind, EntryKind::Stdout);
        assert_eq!(entries[0].text, "random free-form text");
    }

    #[test]
    fn parse_stdout_line_empty_input_returns_empty() {
        assert!(parse_stdout_line("").is_empty());
        assert!(parse_stdout_line("   ").is_empty());
    }

    #[test]
    fn event_re_captures_run_stream_data() {
        let line = r#"[openclaw-gateway:event] run=r-9 stream=assistant data={"text":"yo"}"#;
        let caps = event_re().captures(line).unwrap();
        assert_eq!(caps.get(1).unwrap().as_str(), "r-9");
        assert_eq!(caps.get(2).unwrap().as_str(), "assistant");
        assert_eq!(caps.get(3).unwrap().as_str(), r#"{"text":"yo"}"#);
    }

    #[test]
    fn parse_event_line_handles_malformed_json_gracefully() {
        let line = r#"[openclaw-gateway:event] run=r-1 stream=assistant data=not-json"#;
        // Malformed JSON → empty data object → no entries
        let entries = parse_event_line(line);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_event_line_no_match_falls_back_to_stdout() {
        let line = "[openclaw-gateway:event] garbage";
        let entries = parse_event_line(line);
        assert_eq!(entries[0].kind, EntryKind::Stdout);
    }
}
