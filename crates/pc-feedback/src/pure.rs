#![forbid(unsafe_code)]

//! Feedback pure helpers — 1:1 port of paperclip/server/src/services/feedback.ts
//! and paperclip/server/src/services/feedback-share-client.ts.
//!
//! R715: zero-DB helpers extracted from the feedback service. Each function is a
//! small, testable building block.

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Default max length for an issue-target excerpt.
pub const MAX_EXCERPT_CHARS: usize = 200;
/// Default max length for any single trace file.
pub const MAX_TRACE_FILE_CHARS: usize = 10_000_000;
/// Max length for a failure-reason message stored in DB.
pub const MAX_FAILURE_REASON_CHARS: usize = 1_000;
/// Length of the truncated sha256 prefix used to build the export id.
pub const EXPORT_ID_HEX_PREFIX_LEN: usize = 24;
/// Default backend URL for the telemetry share client.
pub const DEFAULT_FEEDBACK_EXPORT_BACKEND_URL: &str = "https://telemetry.paperclip.ing";

// =============================================================================
// Type guards (Node asRecord/asString/asNumber/asBoolean parity)
// =============================================================================

pub fn as_record(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(v) if v.is_object() => Some(v.clone()),
        _ => None,
    }
}

pub fn as_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
        }
        _ => None,
    }
}

pub fn as_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    }
}

pub fn as_boolean(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

// =============================================================================
// Arrays / strings (Node parity)
// =============================================================================

pub fn unique_non_empty(values: &[Option<&str>]) -> Vec<String> {
    let mut seen = BTreeSet::<String>::new();
    let mut out = Vec::new();
    for v in values.iter().flatten() {
        let trimmed = v.trim();
        if trimmed.is_empty() { continue; }
        let key = trimmed.to_string();
        if seen.insert(key.clone()) {
            out.push(key);
        }
    }
    out
}

pub fn truncate_excerpt(text: &str, max: usize) -> Option<String> {
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() { return None; }
    if normalized.len() <= max { return Some(normalized); }
    let mut cut = normalized[..max.saturating_sub(1)].to_string();
    cut.push_str("\u{2026}");
    Some(cut)
}

pub fn content_type_for_path(file_path: &str) -> &'static str {
    let lower = file_path.to_lowercase();
    if lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
        "application/x-ndjson"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".md") {
        "text/markdown; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    }
}

// =============================================================================
// Issue paths + target summaries
// =============================================================================

pub fn build_issue_path(identifier: Option<&str>) -> Option<String> {
    let id = identifier?;
    let prefix = id.split('-').next()?.trim();
    if prefix.is_empty() { return None; }
    Some(format!("/{}/issues/{}", prefix, id))
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackTargetSummary {
    pub label: Option<String>,
    pub excerpt: Option<String>,
    pub author_agent_id: Option<String>,
    pub author_user_id: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

pub fn build_target_summary(input: FeedbackTargetSummary) -> FeedbackTargetSummary {
    input
}

// =============================================================================
// Vote reason + skill reference helpers
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackVoteValue { Up, Down }

pub fn parse_feedback_vote(raw: &str) -> Option<FeedbackVoteValue> {
    match raw {
        "up" => Some(FeedbackVoteValue::Up),
        "down" => Some(FeedbackVoteValue::Down),
        _ => None,
    }
}

pub fn normalize_reason(vote: FeedbackVoteValue, reason: Option<&str>) -> Option<String> {
    if vote != FeedbackVoteValue::Down { return None; }
    let r = reason?;
    let trimmed = r.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

pub fn normalize_skill_reference(value: &str) -> String {
    value.trim().to_lowercase()
}

pub fn matches_skill_reference(key: &str, slug: &str, name: &str, reference: &str) -> bool {
    let norm = normalize_skill_reference(reference);
    if norm.is_empty() { return false; }
    if key.to_lowercase() == norm { return true; }
    if slug.to_lowercase() == norm { return true; }
    if name.to_lowercase() == norm { return true; }
    let key_tail = key.split('/').next_back().unwrap_or("").to_lowercase();
    key_tail == norm
}

// =============================================================================
// Export id + share object key (sha256 based)
// =============================================================================

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn build_export_id(feedback_vote_id: &str, shared_at: DateTime<Utc>) -> String {
    let digest = sha256_hex(&format!("{}:{}", feedback_vote_id, shared_at.to_rfc3339()));
    format!("fbexp_{}", &digest[..EXPORT_ID_HEX_PREFIX_LEN])
}

pub fn build_feedback_share_object_key(
    company_id: &str,
    trace_id_or_export_id: &str,
    exported_at: DateTime<Utc>,
) -> String {
    let year = exported_at.year();
    let month = format!("{:02}", exported_at.month());
    let day = format!("{:02}", exported_at.day());
    format!(
        "feedback-traces/{}/{}/{}/{}/{}.json",
        company_id, year, month, day, trace_id_or_export_id
    )
}

// =============================================================================
// Source-run resolution (walks nested bundle payload)
// =============================================================================

pub fn resolve_source_run_id(payload_snapshot: Option<&Value>) -> Option<String> {
    let p = payload_snapshot?;
    let target = p.get("target");
    let created_by = target.and_then(|v| v.get("createdByRunId"));
    if let Some(id) = as_string(created_by) { return Some(id); }
    let bundle = p.get("bundle");
    let agent_context = bundle.and_then(|v| v.get("agentContext"));
    let runtime = agent_context.and_then(|v| v.get("runtime"));
    let source_run = runtime.and_then(|v| v.get("sourceRun"));
    as_string(source_run.and_then(|v| v.get("id")))
}

// =============================================================================
// Bundle file (with sha256 + byte length)
// =============================================================================

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackTraceBundleFile {
    pub path: String,
    pub content_type: String,
    pub encoding: String,
    pub byte_length: usize,
    pub sha256: String,
    pub source: String,
    pub contents: String,
}

pub fn make_bundle_file(
    path: impl Into<String>,
    content_type: impl Into<String>,
    source: impl Into<String>,
    contents: impl Into<String>,
) -> FeedbackTraceBundleFile {
    let contents = contents.into();
    let byte_length = contents.len(); // Node Buffer.byteLength(contents, "utf8") == JS string UTF-8 byte length
    let sha256 = sha256_hex(&contents);
    FeedbackTraceBundleFile {
        path: path.into(),
        content_type: content_type.into(),
        encoding: "utf8".to_string(),
        byte_length,
        sha256,
        source: source.into(),
        contents,
    }
}

pub fn append_note(notes: &mut Vec<String>, note: &str) {
    if note.trim().is_empty() { return; }
    if notes.iter().any(|n| n == note) { return; }
    notes.push(note.to_string());
}

// =============================================================================
// Run log parsing
// =============================================================================

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunLogEntry {
    pub ts: String,
    pub stream: String,
    pub chunk: String,
}

pub fn parse_run_log_entries(log_text: Option<&str>) -> Vec<RunLogEntry> {
    let text = match log_text { Some(t) => t, None => return Vec::new() };
    let mut entries = Vec::new();
    for raw_line in text.split('\n').chain(text.split('\r').filter(|s| !s.is_empty())) {
        let line = raw_line.trim();
        if line.is_empty() { continue; }
        match serde_json::from_str::<Value>(line) {
            Ok(parsed) => {
                let ts = as_string(parsed.get("ts"))
                    .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string());
                let stream = as_string(parsed.get("stream")).unwrap_or_else(|| "stdout".to_string());
                let chunk = parsed.get("chunk")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                entries.push(RunLogEntry { ts, stream, chunk });
            }
            Err(_) => { /* skip malformed line */ }
        }
    }
    entries
}

// =============================================================================
// Trace capture status
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackTraceBundleCaptureStatus { Full, Partial, Unavailable }

pub fn capture_status_from_files(files: &[FeedbackTraceBundleFile]) -> FeedbackTraceBundleCaptureStatus {
    let sources: BTreeSet<&str> = files.iter().map(|f| f.source.as_str()).collect();
    if sources.contains("codex_session")
        || sources.contains("claude_project_session")
        || sources.contains("claude_debug_log")
    {
        return FeedbackTraceBundleCaptureStatus::Full;
    }
    if sources.contains("opencode_session")
        && sources.contains("opencode_message")
        && sources.contains("opencode_message_part")
    {
        return FeedbackTraceBundleCaptureStatus::Full;
    }
    let has_adapter_file = files.iter().any(|f| {
        f.source != "paperclip_run"
            && f.source != "paperclip_run_events"
            && f.source != "paperclip_run_log"
    });
    if has_adapter_file || !files.is_empty() {
        FeedbackTraceBundleCaptureStatus::Partial
    } else {
        FeedbackTraceBundleCaptureStatus::Unavailable
    }
}

// =============================================================================
// Failure reason truncation
// =============================================================================

pub fn truncate_failure_reason(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    let trimmed = message.trim();
    if trimmed.is_empty() { return "Feedback export failed".to_string(); }
    if trimmed.len() <= MAX_FAILURE_REASON_CHARS {
        trimmed.to_string()
    } else {
        trimmed[..MAX_FAILURE_REASON_CHARS].to_string()
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn as_record_only_objects() {
        assert!(as_record(Some(&json!({"a": 1}))).is_some());
        assert!(as_record(Some(&json!({}))).is_some());
        assert!(as_record(Some(&json!([1, 2]))).is_none());
        assert!(as_record(Some(&json!("str"))).is_none());
        assert!(as_record(Some(&Value::Null)).is_none());
        assert!(as_record(None).is_none());
    }

    #[test]
    fn as_string_trims_and_skips_empty() {
        assert_eq!(as_string(Some(&json!("ok"))).as_deref(), Some("ok"));
        assert_eq!(as_string(Some(&json!("  hi  "))).as_deref(), Some("hi"));
        assert_eq!(as_string(Some(&json!(""))), None);
        assert_eq!(as_string(Some(&json!("   "))), None);
        assert_eq!(as_string(Some(&json!(42))), None);
    }

    #[test]
    fn as_number_filters_non_finite() {
        assert_eq!(as_number(Some(&json!(1.5))).unwrap(), 1.5);
        assert_eq!(as_number(Some(&json!(0))).unwrap(), 0.0);
        assert!(as_number(Some(&json!("1.0"))).is_none());
    }

    #[test]
    fn as_boolean_strict() {
        assert_eq!(as_boolean(Some(&json!(true))), Some(true));
        assert_eq!(as_boolean(Some(&json!(false))), Some(false));
        assert_eq!(as_boolean(Some(&json!(1))), None);
        assert_eq!(as_boolean(Some(&json!("true"))), None);
    }

    #[test]
    fn unique_non_empty_dedup() {
        let v = vec![Some("a"), Some("a"), Some("b"), None, Some("  "), Some("c")];
        let out = unique_non_empty(&v);
        assert_eq!(out, vec!["a", "b", "c"]);
    }

    #[test]
    fn truncate_excerpt_collapses_whitespace() {
        assert_eq!(truncate_excerpt("hello   world", 100).as_deref(), Some("hello world"));
        assert_eq!(truncate_excerpt("", 100), None);
        let out = truncate_excerpt("a".repeat(250).as_str(), 10).unwrap();
        assert!(out.ends_with('\u{2026}'));
        assert!(out.chars().count() <= 10);
    }

    #[test]
    fn content_type_known_extensions() {
        assert_eq!(content_type_for_path("foo.jsonl"), "application/x-ndjson");
        assert_eq!(content_type_for_path("FOO.JSON"), "application/json");
        assert_eq!(content_type_for_path("README.md"), "text/markdown; charset=utf-8");
        assert_eq!(content_type_for_path("log.txt"), "text/plain; charset=utf-8");
    }

    #[test]
    fn build_issue_path_basic() {
        assert_eq!(build_issue_path(Some("ACME-123")).as_deref(), Some("/ACME/issues/ACME-123"));
        assert_eq!(build_issue_path(None), None);
        assert_eq!(build_issue_path(Some("")), None);
    }

    #[test]
    fn parse_feedback_vote_values() {
        assert_eq!(parse_feedback_vote("up"), Some(FeedbackVoteValue::Up));
        assert_eq!(parse_feedback_vote("down"), Some(FeedbackVoteValue::Down));
        assert_eq!(parse_feedback_vote("sideways"), None);
    }

    #[test]
    fn normalize_reason_only_for_down() {
        assert_eq!(normalize_reason(FeedbackVoteValue::Up, Some("hi")), None);
        assert_eq!(normalize_reason(FeedbackVoteValue::Down, None), None);
        assert_eq!(normalize_reason(FeedbackVoteValue::Down, Some("  bad  ")).as_deref(), Some("bad"));
    }

    #[test]
    fn matches_skill_reference_variants() {
        assert!(matches_skill_reference("acme/search", "acme-search", "Search", "search"));
        assert!(matches_skill_reference("acme/search", "acme-search", "Search", "acme/search"));
        assert!(matches_skill_reference("acme/search", "acme-search", "Search", "Acme-Search"));
        assert!(!matches_skill_reference("acme/search", "acme-search", "Search", "  "));
        assert!(!matches_skill_reference("acme/search", "acme-search", "Search", "other"));
    }

    #[test]
    fn build_export_id_format() {
        let ts = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let id = build_export_id("vote-123", ts);
        assert!(id.starts_with("fbexp_"));
        assert_eq!(id.len(), "fbexp_".len() + EXPORT_ID_HEX_PREFIX_LEN);
    }

    #[test]
    fn share_object_key_format() {
        let ts = Utc.with_ymd_and_hms(2025, 8, 16, 12, 0, 0).unwrap();
        let key = build_feedback_share_object_key("co-1", "fbexp_abc", ts);
        assert_eq!(key, "feedback-traces/co-1/2025/08/16/fbexp_abc.json");
    }

    #[test]
    fn resolve_source_run_id_target_first() {
        let p = json!({"target": {"createdByRunId": "run-1"}, "bundle": {"agentContext": {"runtime": {"sourceRun": {"id": "run-2"}}}}});
        assert_eq!(resolve_source_run_id(Some(&p)).as_deref(), Some("run-1"));
    }

    #[test]
    fn resolve_source_run_id_fallback_bundle() {
        let p = json!({"bundle": {"agentContext": {"runtime": {"sourceRun": {"id": "run-X"}}}}});
        assert_eq!(resolve_source_run_id(Some(&p)).as_deref(), Some("run-X"));
    }

    #[test]
    fn resolve_source_run_id_none() {
        assert_eq!(resolve_source_run_id(None), None);
        assert_eq!(resolve_source_run_id(Some(&json!({}))), None);
    }

    #[test]
    fn make_bundle_file_hashes() {
        let f = make_bundle_file("a.json", "application/json", "paperclip_run", "hi");
        assert_eq!(f.byte_length, 2);
        assert_eq!(f.encoding, "utf8");
        assert_eq!(f.sha256.len(), 64);
        assert_eq!(f.path, "a.json");
    }

    #[test]
    fn append_note_dedup_and_trim() {
        let mut notes: Vec<String> = vec![];
        append_note(&mut notes, "first");
        append_note(&mut notes, "first");
        append_note(&mut notes, "");
        append_note(&mut notes, "  ");
        append_note(&mut notes, "second");
        assert_eq!(notes, vec!["first", "second"]);
    }

    #[test]
    fn parse_run_log_entries_malformed_skipped() {
        let log = "{\"ts\":\"t1\",\"stream\":\"stdout\",\"chunk\":\"hello\"}\nnot-json\n{\"ts\":\"t2\",\"chunk\":\"x\"}";
        let entries = parse_run_log_entries(Some(log));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].chunk, "hello");
        assert_eq!(entries[1].stream, "stdout"); // default
    }

    #[test]
    fn capture_status_full_for_known_sources() {
        let files = vec![
            make_bundle_file("a", "text/plain", "codex_session", "x"),
        ];
        assert_eq!(capture_status_from_files(&files), FeedbackTraceBundleCaptureStatus::Full);

        let files = vec![
            make_bundle_file("a", "text/plain", "opencode_session", "x"),
            make_bundle_file("b", "text/plain", "opencode_message", "y"),
            make_bundle_file("c", "text/plain", "opencode_message_part", "z"),
        ];
        assert_eq!(capture_status_from_files(&files), FeedbackTraceBundleCaptureStatus::Full);
    }

    #[test]
    fn capture_status_partial_and_unavailable() {
        let files = vec![make_bundle_file("a", "text/plain", "paperclip_run", "x")];
        assert_eq!(capture_status_from_files(&files), FeedbackTraceBundleCaptureStatus::Partial);

        let files = vec![make_bundle_file("a", "text/plain", "custom_adapter", "x")];
        assert_eq!(capture_status_from_files(&files), FeedbackTraceBundleCaptureStatus::Partial);

        let empty: Vec<FeedbackTraceBundleFile> = vec![];
        assert_eq!(capture_status_from_files(&empty), FeedbackTraceBundleCaptureStatus::Unavailable);
    }

    #[test]
    fn truncate_failure_reason_basic() {
        assert_eq!(truncate_failure_reason("  boom  "), "boom");
        assert_eq!(truncate_failure_reason(""), "Feedback export failed");
        let long = "x".repeat(MAX_FAILURE_REASON_CHARS + 50);
        let out = truncate_failure_reason(long);
        assert_eq!(out.len(), MAX_FAILURE_REASON_CHARS);
    }
}


#[cfg(test)]
mod internal_tests_r771 {
    use super::*;

    // ---- Round 771: pc-feedback::pure 边缘测试 ----

    /// as_record: None / 非 object / object 三种。
    #[test]
    fn r771_as_record() {
        assert_eq!(as_record(None), None);
        assert_eq!(as_record(Some(&serde_json::json!("not object"))), None);
        assert_eq!(as_record(Some(&serde_json::json!(1))), None);
        let obj = serde_json::json!({"a": 1});
        assert_eq!(as_record(Some(&obj)).unwrap(), obj);
    }

    /// as_string / as_number / as_boolean: 三种类型转换。
    #[test]
    fn r771_as_primitive_types() {
        assert_eq!(as_string(None), None);
        assert_eq!(as_string(Some(&serde_json::json!("x"))), Some("x".to_string()));
        assert_eq!(as_string(Some(&serde_json::json!(1))), None, "non-string");

        assert_eq!(as_number(None), None);
        assert_eq!(as_number(Some(&serde_json::json!(42))), Some(42.0));
        assert_eq!(as_number(Some(&serde_json::json!("42"))), None, "string");

        assert_eq!(as_boolean(None), None);
        assert_eq!(as_boolean(Some(&serde_json::json!(true))), Some(true));
        assert_eq!(as_boolean(Some(&serde_json::json!(1))), None, "non-bool");
    }

    /// unique_non_empty: 去重 + 过滤 None/空。
    #[test]
    fn r771_unique_non_empty() {
        let v: Vec<Option<&str>> = vec![
            Some("a"),
            Some("a"),
            None,
            Some("b"),
            Some(""),
            Some("b"),
        ];
        let out = unique_non_empty(&v);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }

    /// content_type_for_path: 5 种扩展名 + 未知。
    #[test]
    fn r771_content_type_for_path() {
        assert_eq!(content_type_for_path("file.md"), "text/markdown; charset=utf-8");
        assert_eq!(content_type_for_path("file.json"), "application/json");
        assert_eq!(content_type_for_path("file.txt"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for_path("file.unknown"), "text/plain; charset=utf-8", "unknown → text/plain");
    }

    /// build_issue_path: identifier / None 两种。
    #[test]
    fn r771_build_issue_path() {
        assert_eq!(build_issue_path(Some("PAP-1")), Some("/PAP/issues/PAP-1".to_string()));
        assert_eq!(build_issue_path(None), None);
    }

    /// parse_feedback_vote: 4 种 + 未知。
    #[test]
    fn r771_parse_feedback_vote() {
        assert_eq!(parse_feedback_vote("up"), Some(FeedbackVoteValue::Up));
        assert_eq!(parse_feedback_vote("down"), Some(FeedbackVoteValue::Down));
        assert!(parse_feedback_vote("unknown").is_none());
        assert!(parse_feedback_vote("").is_none());
    }

    /// normalize_reason: 不同 vote 不同必填 / 选填逻辑。
    #[test]
    fn r771_normalize_reason() {
        // up 不需要 reason
        assert_eq!(normalize_reason(FeedbackVoteValue::Up, None), None);
        assert_eq!(normalize_reason(FeedbackVoteValue::Up, Some("good")), None, "up vote → ignore reason");
        // down 通常需要 reason
        assert_eq!(normalize_reason(FeedbackVoteValue::Down, Some("bug")), Some("bug".to_string()));
        assert_eq!(normalize_reason(FeedbackVoteValue::Down, None), None);
    }

    /// append_note: push 到 notes vec。
    #[test]
    fn r771_append_note() {
        let mut notes = Vec::new();
        append_note(&mut notes, "first");
        append_note(&mut notes, "second");
        assert_eq!(notes, vec!["first".to_string(), "second".to_string()]);
    }
}
