#![forbid(unsafe_code)]

//! Feedback redaction state pure helpers — R748.
//!
//! 与 `paperclip/server/src/services/feedback-redaction.ts` 中的 pure helpers 对齐：
//! - `stableStringify` —— 稳定的 JSON 序列化（按 key 字典序排序）
//! - `sha256Digest` —— sha256 hex digest
//! - `createFeedbackRedactionState` —— 空 state 构造
//! - `finalizeFeedbackRedactionSummary` —— 把 state 序列化成 sorted summary
//! - field path helpers —— `build_field_path` / `join_field_path` / `array_index_path`
//!
//! 全部函数零 DB / 零 IO，可独立单测。

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// 把任意 `Value` 序列化为稳定的 JSON 字符串（key 字典序排序）。
///
/// 与 Node `stableStringify` 1:1：
/// - null / primitive → `JSON.stringify`
/// - array → `[<elem1>,<elem2>]`
/// - object → `{<k1>:<v1>,<k2>:<v2>}`（key 按 ASCII 字典序）
pub fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\"")),
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(stable_stringify).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    let v = stable_stringify(&obj[k]);
                    format!("{}:{}", serde_json::to_string(k).unwrap_or_else(|_| String::from("\"\"")), v)
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// 计算 value 的 sha256 hex digest（先 stableStringify 再 sha256）。
///
/// 与 Node `sha256Digest` 1:1：`createHash("sha256").update(stableStringify(value)).digest("hex")`。
pub fn sha256_hex_digest(value: &Value) -> String {
    let canonical = stable_stringify(value);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

/// Redaction state（与 `crate::redaction::free_text_pure::RedactionState` 对齐的纯数据视图）。
///
/// 使用 BTreeMap / BTreeSet 保持字典序，确保 `to_summary` 输出稳定。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionStateLike {
    /// 被 redact 的 field path 集合。
    pub redacted_fields: std::collections::BTreeSet<String>,
    /// 被 truncate 的 field path 集合。
    pub truncated_fields: std::collections::BTreeSet<String>,
    /// 被 omit 的 field path 集合。
    pub omitted_fields: std::collections::BTreeSet<String>,
    /// 注释集合。
    pub notes: std::collections::BTreeSet<String>,
    /// pattern 命中计数（按 pattern kind）。
    pub counts: BTreeMap<String, usize>,
}

impl RedactionStateLike {
    pub fn new() -> Self {
        Self::default()
    }

    /// 把一个 field path 标记为 redact。
    pub fn record_redaction(&mut self, field_path: &str) {
        if !field_path.trim().is_empty() {
            self.redacted_fields.insert(field_path.to_string());
        }
    }

    /// 把一个 field path 标记为 truncate。
    pub fn record_truncation(&mut self, field_path: &str) {
        if !field_path.trim().is_empty() {
            self.truncated_fields.insert(field_path.to_string());
        }
    }

    /// 把一个 field path 标记为 omit。
    pub fn record_omission(&mut self, field_path: &str) {
        if !field_path.trim().is_empty() {
            self.omitted_fields.insert(field_path.to_string());
        }
    }

    /// 累加 pattern 命中计数（count <= 0 跳过）。
    pub fn increment(&mut self, kind: &str, count: usize) {
        if count == 0 {
            return;
        }
        let entry = self.counts.entry(kind.to_string()).or_insert(0);
        *entry += count;
    }

    /// 记录 note。
    pub fn note(&mut self, note: &str) {
        if !note.trim().is_empty() {
            self.notes.insert(note.to_string());
        }
    }

    /// 是否完全空（无任何 record）。
    pub fn is_empty(&self) -> bool {
        self.redacted_fields.is_empty()
            && self.truncated_fields.is_empty()
            && self.omitted_fields.is_empty()
            && self.notes.is_empty()
            && self.counts.is_empty()
    }

    /// 合并另一个 state（self += other）。
    pub fn merge_from(&mut self, other: &RedactionStateLike) {
        for f in &other.redacted_fields {
            self.redacted_fields.insert(f.clone());
        }
        for f in &other.truncated_fields {
            self.truncated_fields.insert(f.clone());
        }
        for f in &other.omitted_fields {
            self.omitted_fields.insert(f.clone());
        }
        for n in &other.notes {
            self.notes.insert(n.clone());
        }
        for (k, v) in &other.counts {
            let entry = self.counts.entry(k.clone()).or_insert(0);
            *entry += v;
        }
    }
}

/// summary 序列化结果。
///
/// 与 Node `finalizeFeedbackRedactionSummary` 对齐：
/// - strategy = "deterministic_feedback_v2"
/// - redactedFields / truncatedFields / omittedFields / notes 按字典序排序
/// - counts 按 key 字典序排序
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactionSummary {
    pub strategy: String,
    pub redacted_fields: Vec<String>,
    pub truncated_fields: Vec<String>,
    pub omitted_fields: Vec<String>,
    pub notes: Vec<String>,
    pub counts: BTreeMap<String, usize>,
}

impl RedactionSummary {
    pub fn from_state(state: &RedactionStateLike) -> Self {
        Self {
            strategy: "deterministic_feedback_v2".to_string(),
            redacted_fields: state.redacted_fields.iter().cloned().collect(),
            truncated_fields: state.truncated_fields.iter().cloned().collect(),
            omitted_fields: state.omitted_fields.iter().cloned().collect(),
            notes: state.notes.iter().cloned().collect(),
            counts: state.counts.clone(),
        }
    }
}

/// 把 state 序列化成 `serde_json::Value` summary。
pub fn finalize_redaction_summary(state: &RedactionStateLike) -> Value {
    serde_json::to_value(RedactionSummary::from_state(state))
        .unwrap_or(Value::Null)
}

// =============================================================================
// Field path helpers
// =============================================================================

/// 给 field path 加子字段路径（用 `.` 连接）。
///
/// 与 Node `${fieldPath}.${key}` 对齐。
pub fn join_field_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{}.{}", parent, child)
    }
}

/// 给 field path 加数组索引路径（用 `[N]` 后缀）。
///
/// 与 Node `${fieldPath}[${index}]` 对齐。
pub fn array_index_path(parent: &str, index: usize) -> String {
    format!("{}[{}]", parent, index)
}

/// 把一段字符串截断到 `max_chars` 字符（按 char 边界）。
///
/// 若 `max_chars == 0` → 返回空字符串。
/// 与 `truncate_string_fields` 内联截断对齐。
pub fn truncate_to_chars(text: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (String::new(), !text.is_empty());
    }
    let count = text.chars().count();
    if count <= max_chars {
        return (text.to_string(), false);
    }
    // 截断到 max_chars 个 char + "..." 末尾标记。
    let truncated: String = text.chars().take(max_chars).collect();
    (format!("{}...", truncated), true)
}

/// 默认的 max chars 上限（与 pc-feedback/redaction/service.rs DEFAULT_MAX_CHARS 对齐）。
pub const DEFAULT_MAX_CHARS: usize = 16 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r748_stable_stringify_null() {
        assert_eq!(stable_stringify(&json!(null)), "null");
    }

    #[test]
    fn r748_stable_stringify_primitives() {
        assert_eq!(stable_stringify(&json!(true)), "true");
        assert_eq!(stable_stringify(&json!(false)), "false");
        assert_eq!(stable_stringify(&json!(42)), "42");
        let v = Value::String("hello".to_string());
        assert_eq!(stable_stringify(&v), "\"hello\"");
    }

    #[test]
    fn r748_stable_stringify_array() {
        assert_eq!(stable_stringify(&json!([1, 2, 3])), "[1,2,3]");
        assert_eq!(stable_stringify(&json!([])), "[]");
    }

    #[test]
    fn r748_stable_stringify_object_sorts_keys() {
        let v = json!({"c": 1, "a": 2, "b": 3});
        assert_eq!(stable_stringify(&v), r#"{"a":2,"b":3,"c":1}"#);
    }

    #[test]
    fn r748_stable_stringify_nested() {
        let v = json!({"b": {"y": 1, "x": 2}, "a": [3, {"q": 4, "p": 5}]});
        let s = stable_stringify(&v);
        assert_eq!(s, r#"{"a":[3,{"p":5,"q":4}],"b":{"x":2,"y":1}}"#);
    }

    #[test]
    fn r748_stable_stringify_string_escapes() {
        // Build string with newline and quote characters.
        let v = Value::String("a\nb\"c".to_string());
        let s = stable_stringify(&v);
        // serde_json escapes newline as 2-char \(backslash + n)
        // and quote as 2-char \(backslash + quote)
        assert!(s.contains("\\n"));
        assert!(s.contains("\\"));
    }

    #[test]
    fn r748_sha256_hex_digest_known_value() {
        let v = json!({"a": 1, "b": 2});
        let d = sha256_hex_digest(&v);
        // 64 hex chars
        assert_eq!(d.len(), 64);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn r748_sha256_hex_digest_stable_across_orderings() {
        let v1 = json!({"a": 1, "b": 2});
        let v2 = json!({"b": 2, "a": 1});
        assert_eq!(sha256_hex_digest(&v1), sha256_hex_digest(&v2));
    }

    #[test]
    fn r748_sha256_hex_digest_changes_with_value() {
        let v1 = json!({"a": 1});
        let v2 = json!({"a": 2});
        assert_ne!(sha256_hex_digest(&v1), sha256_hex_digest(&v2));
    }

    #[test]
    fn r748_state_record_and_increment() {
        let mut s = RedactionStateLike::new();
        s.record_redaction("user.email");
        s.record_redaction("user.password");
        s.increment("pem_block", 1);
        s.increment("jwt", 2);
        s.note("secret value detected");

        assert_eq!(s.redacted_fields.len(), 2);
        assert_eq!(s.counts.get("pem_block"), Some(&1));
        assert_eq!(s.counts.get("jwt"), Some(&2));
        assert!(s.notes.contains("secret value detected"));
    }

    #[test]
    fn r748_state_empty_initial() {
        let s = RedactionStateLike::new();
        assert!(s.is_empty());
    }

    #[test]
    fn r748_state_record_empty_path_skipped() {
        let mut s = RedactionStateLike::new();
        s.record_redaction("");
        s.record_redaction("   ");
        s.record_truncation("");
        assert!(s.is_empty());
    }

    #[test]
    fn r748_state_increment_zero_skipped() {
        let mut s = RedactionStateLike::new();
        s.increment("k", 0);
        assert!(s.counts.is_empty());
    }

    #[test]
    fn r748_state_merge() {
        let mut a = RedactionStateLike::new();
        a.record_redaction("x");
        a.increment("jwt", 1);

        let mut b = RedactionStateLike::new();
        b.record_redaction("y");
        b.increment("pem_block", 1);
        b.increment("jwt", 2);

        a.merge_from(&b);
        assert!(a.redacted_fields.contains("x"));
        assert!(a.redacted_fields.contains("y"));
        assert_eq!(a.counts.get("jwt"), Some(&3));
        assert_eq!(a.counts.get("pem_block"), Some(&1));
    }

    #[test]
    fn r748_summary_sorts_keys() {
        let mut s = RedactionStateLike::new();
        s.record_redaction("z.last");
        s.record_redaction("a.first");
        s.increment("z_pattern", 1);
        s.increment("a_pattern", 1);

        let summary = RedactionSummary::from_state(&s);
        assert_eq!(summary.redacted_fields, vec!["a.first", "z.last"]);
        assert_eq!(summary.strategy, "deterministic_feedback_v2");
    }

    #[test]
    fn r748_finalize_redaction_summary_returns_value() {
        let mut s = RedactionStateLike::new();
        s.record_redaction("a");
        s.increment("jwt", 1);
        let v = finalize_redaction_summary(&s);
        assert_eq!(v["strategy"], "deterministic_feedback_v2");
        assert_eq!(v["redactedFields"], json!(["a"]));
        assert_eq!(v["counts"]["jwt"], json!(1));
    }

    #[test]
    fn r748_join_field_path() {
        assert_eq!(join_field_path("", "user"), "user");
        assert_eq!(join_field_path("user", "email"), "user.email");
        assert_eq!(join_field_path("user.profile", "email"), "user.profile.email");
    }

    #[test]
    fn r748_array_index_path() {
        assert_eq!(array_index_path("items", 0), "items[0]");
        assert_eq!(array_index_path("users[3]", 7), "users[3][7]");
    }

    #[test]
    fn r748_truncate_to_chars_short_text() {
        let (out, truncated) = truncate_to_chars("hello", 10);
        assert_eq!(out, "hello");
        assert!(!truncated);
    }

    #[test]
    fn r748_truncate_to_chars_exact_length() {
        let (out, truncated) = truncate_to_chars("hello", 5);
        assert_eq!(out, "hello");
        assert!(!truncated);
    }

    #[test]
    fn r748_truncate_to_chars_long_text() {
        let (out, truncated) = truncate_to_chars("hello world", 5);
        assert_eq!(out, "hello...");
        assert!(truncated);
    }

    #[test]
    fn r748_truncate_to_chars_zero_max() {
        let (out, truncated) = truncate_to_chars("hello", 0);
        assert_eq!(out, "");
        assert!(truncated);
    }

    #[test]
    fn r748_truncate_to_chars_byte_count() {
        // max=3 chars means take 3 chars + '...'
        let (out, truncated) = truncate_to_chars("abcdef", 3);
        assert_eq!(out, "abc...");
        assert!(truncated);
    }

    #[test]
    fn r748_default_max_chars_constant() {
        assert_eq!(DEFAULT_MAX_CHARS, 16 * 1024);
    }
}
