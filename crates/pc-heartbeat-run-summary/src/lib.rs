//! Heartbeat run result summary.
//!
//! 1:1 port of Node `paperclip/server/src/services/heartbeat-run-summary.ts`.
//!
//! Pure logic — extracts a concise "summary" sub-object from a run's
//! result JSON, merges a new summary into existing result JSON, and
//! builds the issue comment body. No DB, no I/O.

#![forbid(unsafe_code)]

use serde_json::{Map, Value};

/// Max length of a free-text summary string.
pub const HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS: usize = 500;
/// Max length of a free-text "result" or "output" field.
pub const HEARTBEAT_RUN_RESULT_OUTPUT_MAX_CHARS: usize = 4_096;
/// Cap on the full result JSON size we will inspect (to avoid
/// pathological large blobs from being summarised).
pub const HEARTBEAT_RUN_SAFE_RESULT_JSON_MAX_BYTES: usize = 64 * 1024;

/// Truncate a free-text field to `max_length`. Returns `None` for
/// non-string values.
pub fn truncate_summary_text(value: Option<&Value>, max_length: usize) -> Option<String> {
    let s = value?.as_str()?;
    let truncated = if s.chars().count() > max_length {
        s.chars().take(max_length).collect::<String>()
    } else {
        s.to_string()
    };
    Some(truncated)
}

/// Read a numeric (or anything) field from a record. Returns
/// `Some(value)` if the key is present (even if null), `None` if
/// missing.
pub fn read_numeric_field<'a>(record: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    if record.contains_key(key) {
        record.get(key)
    } else {
        None
    }
}

/// Read a non-empty trimmed string from a value.
pub fn read_comment_text(value: Option<&Value>) -> Option<String> {
    let s = value?.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Merge a `summary` text into an existing result JSON object. If the
/// result JSON is null/invalid, returns either `{ "summary": s }` or
/// `null` depending on whether `summary` is present.
pub fn merge_heartbeat_run_result_json(
    result_json: Option<&Map<String, Value>>,
    summary: Option<&str>,
) -> Option<Map<String, Value>> {
    let normalized_summary = read_comment_text_str(summary);

    let Some(base) = result_json else {
        return normalized_summary.map(|s| {
            let mut m = Map::new();
            m.insert("summary".to_string(), Value::String(s));
            m
        });
    };

    if normalized_summary.is_none() {
        return Some(base.clone());
    }

    if read_comment_text(base.get("summary")).is_some() {
        return Some(base.clone());
    }

    let mut out = base.clone();
    out.insert("summary".to_string(), Value::String(normalized_summary.unwrap()));
    Some(out)
}

fn read_comment_text_str(s: Option<&str>) -> Option<String> {
    let v = s?;
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Extract a concise summary sub-object from a run's result JSON.
/// Returns `None` if `result_json` is null/invalid or if no recognised
/// field was found.
pub fn summarize_heartbeat_run_result_json(
    result_json: Option<&Map<String, Value>>,
) -> Option<Map<String, Value>> {
    let result_json = result_json?;
    let mut summary: Map<String, Value> = Map::new();

    // Text fields (truncate to 500 chars).
    for key in ["summary", "result", "message", "error"] {
        if let Some(v) = truncate_summary_text(result_json.get(key), HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS) {
            summary.insert(key.to_string(), Value::String(v));
        }
    }

    // Numeric cost field aliases.
    for key in ["total_cost_usd", "cost_usd", "costUsd"] {
        if let Some(v) = read_numeric_field(result_json, key) {
            if !v.is_null() {
                summary.insert(key.to_string(), v.clone());
            }
        }
    }

    // Free-text status fields (no truncation).
    for key in ["stopReason", "timeoutSource"] {
        if let Some(v) = read_comment_text(result_json.get(key)) {
            summary.insert(key.to_string(), Value::String(v));
        }
    }

    // Numeric timeout fields.
    for key in ["effectiveTimeoutSec", "effectiveTimeoutMs"] {
        if let Some(v) = read_numeric_field(result_json, key) {
            if !v.is_null() {
                summary.insert(key.to_string(), v.clone());
            }
        }
    }

    // Boolean timeout fields.
    for key in ["timeoutConfigured", "timeoutFired"] {
        if let Some(v) = result_json.get(key) {
            if let Value::Bool(b) = v {
                summary.insert(key.to_string(), Value::Bool(*b));
            }
        }
    }

    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

/// Build a human-readable comment body for an issue from the result
/// JSON. Returns the first non-empty of `summary` / `result` / `message`.
pub fn build_heartbeat_run_issue_comment(
    result_json: Option<&Map<String, Value>>,
) -> Option<String> {
    let result_json = result_json?;
    read_comment_text(result_json.get("summary"))
        .or_else(|| read_comment_text(result_json.get("result")))
        .or_else(|| read_comment_text(result_json.get("message")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("expected object, got {other:?}"),
        }
    }

    // -------- truncate_summary_text --------

    #[test]
    fn truncate_returns_none_for_non_string() {
        assert!(truncate_summary_text(Some(&json!(42)), 100).is_none());
        assert!(truncate_summary_text(Some(&json!(null)), 100).is_none());
        assert!(truncate_summary_text(Some(&json!([1, 2, 3])), 100).is_none());
    }

    #[test]
    fn truncate_returns_none_for_missing_key() {
        let m = obj(json!({"a": 1}));
        assert!(truncate_summary_text(m.get("missing"), 100).is_none());
    }

    #[test]
    fn truncate_passes_through_short_string() {
        assert_eq!(truncate_summary_text(Some(&json!("hello")), 100).unwrap(), "hello");
    }

    #[test]
    fn truncate_clips_long_string() {
        let long = "a".repeat(1_000);
        let out = truncate_summary_text(Some(&Value::String(long)), 50).unwrap();
        assert_eq!(out.chars().count(), 50);
        assert!(out.chars().all(|c| c == 'a'));
    }

    // -------- read_numeric_field --------

    #[test]
    fn read_numeric_field_returns_none_for_missing_key() {
        let m = obj(json!({"a": 1}));
        assert!(read_numeric_field(&m, "missing").is_none());
    }

    #[test]
    fn read_numeric_field_returns_some_for_existing_key() {
        let m = obj(json!({"a": 1, "b": null}));
        assert_eq!(read_numeric_field(&m, "a").unwrap(), &json!(1));
        // null is "present" per the Node semantics; caller checks null.
        assert_eq!(read_numeric_field(&m, "b").unwrap(), &Value::Null);
    }

    // -------- read_comment_text --------

    #[test]
    fn read_comment_text_skips_non_strings_and_blanks() {
        assert!(read_comment_text(Some(&json!(1))).is_none());
        assert!(read_comment_text(Some(&json!("   "))).is_none());
        assert!(read_comment_text(Some(&json!(""))).is_none());
        assert!(read_comment_text(Some(&json!("  hi  "))).unwrap() == "hi");
    }

    // -------- merge_heartbeat_run_result_json --------

    #[test]
    fn merge_with_null_result_yields_summary_object() {
        let out = merge_heartbeat_run_result_json(None, Some("hello")).unwrap();
        assert_eq!(out.get("summary").unwrap(), "hello");
    }

    #[test]
    fn merge_with_null_result_and_null_summary_yields_none() {
        assert!(merge_heartbeat_run_result_json(None, None).is_none());
    }

    #[test]
    fn merge_with_null_result_and_empty_summary_yields_none() {
        assert!(merge_heartbeat_run_result_json(None, Some("   ")).is_none());
    }

    #[test]
    fn merge_preserves_existing_summary() {
        let base = obj(json!({"summary": "kept", "other": 1}));
        let out = merge_heartbeat_run_result_json(Some(&base), Some("new")).unwrap();
        assert_eq!(out.get("summary").unwrap(), "kept");
        assert_eq!(out.get("other").unwrap(), 1);
    }

    #[test]
    fn merge_inserts_summary_when_missing() {
        let base = obj(json!({"foo": "bar"}));
        let out = merge_heartbeat_run_result_json(Some(&base), Some("added")).unwrap();
        assert_eq!(out.get("summary").unwrap(), "added");
        assert_eq!(out.get("foo").unwrap(), "bar");
    }

    #[test]
    fn merge_trims_summary_whitespace() {
        let base = obj(json!({"foo": "bar"}));
        let out = merge_heartbeat_run_result_json(Some(&base), Some("  hello  ")).unwrap();
        assert_eq!(out.get("summary").unwrap(), "hello");
    }

    #[test]
    fn merge_with_empty_summary_returns_base_unchanged() {
        let base = obj(json!({"foo": "bar"}));
        let out = merge_heartbeat_run_result_json(Some(&base), Some("")).unwrap();
        assert!(out.get("summary").is_none());
        assert_eq!(out.get("foo").unwrap(), "bar");
    }

    // -------- summarize_heartbeat_run_result_json --------

    #[test]
    fn summarize_extracts_text_fields() {
        let v = obj(json!({
            "summary": "Did the thing",
            "result": "All good",
            "message": "FYI",
            "error": "oops"
        }));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        assert_eq!(s["summary"], "Did the thing");
        assert_eq!(s["result"], "All good");
        assert_eq!(s["message"], "FYI");
        assert_eq!(s["error"], "oops");
    }

    #[test]
    fn summarize_truncates_text_fields() {
        let long = "x".repeat(2_000);
        let v = obj(json!({"summary": long}));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        assert_eq!(s["summary"].as_str().unwrap().chars().count(), HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS);
    }

    #[test]
    fn summarize_picks_first_cost_alias_present() {
        let v = obj(json!({"total_cost_usd": 1.23, "cost_usd": 9.99}));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        // Both present, both copied (the function copies each alias).
        assert_eq!(s["total_cost_usd"], json!(1.23));
        assert_eq!(s["cost_usd"], json!(9.99));
    }

    #[test]
    fn summarize_includes_stop_reason_and_timeout_source() {
        let v = obj(json!({"stopReason": "max_turns_exhausted", "timeoutSource": "config"}));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        assert_eq!(s["stopReason"], "max_turns_exhausted");
        assert_eq!(s["timeoutSource"], "config");
    }

    #[test]
    fn summarize_includes_timeout_fields() {
        let v = obj(json!({
            "effectiveTimeoutSec": 60,
            "effectiveTimeoutMs": 60_000,
            "timeoutConfigured": true,
            "timeoutFired": false
        }));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        assert_eq!(s["effectiveTimeoutSec"], json!(60));
        assert_eq!(s["effectiveTimeoutMs"], json!(60_000));
        assert_eq!(s["timeoutConfigured"], json!(true));
        assert_eq!(s["timeoutFired"], json!(false));
    }

    #[test]
    fn summarize_ignores_unrecognised_fields() {
        let v = obj(json!({"unknown_key": "ignored", "summary": "kept"}));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        assert!(s.get("unknown_key").is_none());
        assert_eq!(s["summary"], "kept");
    }

    #[test]
    fn summarize_returns_none_for_empty_object() {
        let v = obj(json!({}));
        assert!(summarize_heartbeat_run_result_json(Some(&v)).is_none());
    }

    #[test]
    fn summarize_returns_none_for_null_input() {
        assert!(summarize_heartbeat_run_result_json(None).is_none());
    }

    #[test]
    fn summarize_does_not_include_null_cost() {
        let v = obj(json!({"total_cost_usd": null, "summary": "x"}));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        assert!(s.get("total_cost_usd").is_none());
        assert_eq!(s["summary"], "x");
    }

    #[test]
    fn summarize_does_not_include_non_bool_timeout_flags() {
        let v = obj(json!({"summary": "x", "timeoutConfigured": "yes"}));
        let s = summarize_heartbeat_run_result_json(Some(&v)).unwrap();
        assert!(s.get("timeoutConfigured").is_none());
    }

    // -------- build_heartbeat_run_issue_comment --------

    #[test]
    fn comment_prefers_summary_then_result_then_message() {
        let v = obj(json!({"summary": "S", "result": "R", "message": "M"}));
        assert_eq!(build_heartbeat_run_issue_comment(Some(&v)).unwrap(), "S");

        let v = obj(json!({"result": "R", "message": "M"}));
        assert_eq!(build_heartbeat_run_issue_comment(Some(&v)).unwrap(), "R");

        let v = obj(json!({"message": "M"}));
        assert_eq!(build_heartbeat_run_issue_comment(Some(&v)).unwrap(), "M");
    }

    #[test]
    fn comment_trims_whitespace() {
        let v = obj(json!({"summary": "  hello  "}));
        assert_eq!(build_heartbeat_run_issue_comment(Some(&v)).unwrap(), "hello");
    }

    #[test]
    fn comment_skips_blank_values() {
        let v = obj(json!({"summary": "   ", "result": "ok"}));
        assert_eq!(build_heartbeat_run_issue_comment(Some(&v)).unwrap(), "ok");
    }

    #[test]
    fn comment_returns_none_for_null_input() {
        assert!(build_heartbeat_run_issue_comment(None).is_none());
    }

    #[test]
    fn comment_returns_none_when_all_text_fields_missing_or_blank() {
        let v = obj(json!({"summary": "", "result": "  "}));
        assert!(build_heartbeat_run_issue_comment(Some(&v)).is_none());
    }
}
