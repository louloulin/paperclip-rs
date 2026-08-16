#![forbid(unsafe_code)]

//! Feedback redaction pure helpers.
//! R712: Direct port of feedback-redaction.ts::isPlainRecord + increment + recordField + applyPattern.

use regex::Regex;
use serde_json::Value;

/// Test if a value is a plain JSON object (not null, not array).
/// Node isPlainRecord 1:1 parity.
pub fn is_plain_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// Increment counter in state.counts. Skip if count <= 0.
pub fn increment(counts: &mut std::collections::BTreeMap<String, usize>, kind: &str, count: usize) {
    if count == 0 { return; }
    let entry = counts.entry(kind.to_string()).or_insert(0);
    *entry += count;
}

/// Add field path to state.redactedFields. Skip if empty/whitespace.
pub fn record_field(fields: &mut std::collections::BTreeSet<String>, field_path: &str) {
    if field_path.trim().is_empty() { return; }
    fields.insert(field_path.to_string());
}

/// A redaction pattern (regex + replacement).
#[derive(Debug, Clone)]
pub struct RedactionPattern {
    pub name: String,
    pub regex: Regex,
    pub replacement: String,
}

/// Apply pattern to input, return (output, match_count).
/// Node applyPattern 1:1 parity (regex is reset to lastIndex=0 after use).
pub fn apply_pattern(input: &str, pattern: &RedactionPattern) -> (String, usize) {
    let matches: Vec<_> = pattern.regex.find_iter(input).collect();
    if matches.is_empty() {
        return (input.to_string(), 0);
    }
    let count = matches.len();
    let result = pattern.regex.replace_all(input, pattern.replacement.as_str());
    (result.into_owned(), count)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_plain_record_object() {
        assert!(is_plain_record(&json!({})));
        assert!(is_plain_record(&json!({"k": "v"})));
    }
    #[test]
    fn is_plain_record_array_returns_false() {
        assert!(!is_plain_record(&json!([1, 2, 3])));
    }
    #[test]
    fn is_plain_record_null_returns_false() {
        assert!(!is_plain_record(&json!(null)));
    }
    #[test]
    fn is_plain_record_scalar_returns_false() {
        assert!(!is_plain_record(&json!(42)));
        assert!(!is_plain_record(&json!("str")));
        assert!(!is_plain_record(&json!(true)));
    }

    #[test]
    fn increment_basic() {
        let mut m = std::collections::BTreeMap::new();
        increment(&mut m, "pattern_a", 3);
        increment(&mut m, "pattern_a", 2);
        increment(&mut m, "pattern_b", 1);
        assert_eq!(m.get("pattern_a"), Some(&5));
        assert_eq!(m.get("pattern_b"), Some(&1));
    }
    #[test]
    fn increment_zero_skipped() {
        let mut m = std::collections::BTreeMap::new();
        increment(&mut m, "k", 0);
        assert!(m.is_empty());
    }

    #[test]
    fn record_field_basic() {
        let mut s = std::collections::BTreeSet::new();
        record_field(&mut s, "user.email");
        record_field(&mut s, "user.password");
        assert_eq!(s.len(), 2);
        assert!(s.contains("user.email"));
    }
    #[test]
    fn record_field_empty_skipped() {
        let mut s = std::collections::BTreeSet::new();
        record_field(&mut s, "");
        record_field(&mut s, "   ");
        assert!(s.is_empty());
    }
    #[test]
    fn record_field_dedup() {
        let mut s = std::collections::BTreeSet::new();
        record_field(&mut s, "a");
        record_field(&mut s, "a");
        assert_eq!(s.len(), 1);
    }

    fn make_pattern(re: &str, replacement: &str) -> RedactionPattern {
        RedactionPattern { name: "test".into(), regex: Regex::new(re).unwrap(), replacement: replacement.into() }
    }

    #[test]
    fn apply_pattern_no_match() {
        let p = make_pattern(r"\d+", "N");
        let (out, count) = apply_pattern("abc", &p);
        assert_eq!(out, "abc");
        assert_eq!(count, 0);
    }

    #[test]
    fn apply_pattern_single_match() {
        let p = make_pattern(r"\d+", "N");
        let (out, count) = apply_pattern("abc123xyz", &p);
        assert_eq!(out, "abcNxyz");
        assert_eq!(count, 1);
    }

    #[test]
    fn apply_pattern_multiple_matches() {
        let p = make_pattern(r"\d+", "N");
        let (out, count) = apply_pattern("a1b2c3", &p);
        assert_eq!(out, "aNbNcN");
        assert_eq!(count, 3);
    }

    #[test]
    fn apply_pattern_replacement_with_capture() {
        let p = make_pattern(r"(\w+)@", "[EMAIL]");
        let (out, count) = apply_pattern("Contact alice@ or bob@host", &p);
        // Node parity: regex \w+@ 匹配 alice@ 和 bob@，整体被 [EMAIL] 替换
        assert_eq!(out, "Contact [EMAIL] or [EMAIL]host");
        assert_eq!(count, 2);
    }

    #[test]
    fn apply_pattern_reusable() {
        let p = make_pattern(r"foo", "bar");
        let (out1, _) = apply_pattern("foo and foo", &p);
        let (out2, _) = apply_pattern("foo again", &p);
        assert_eq!(out1, "bar and bar");
        assert_eq!(out2, "bar again");
    }
}
