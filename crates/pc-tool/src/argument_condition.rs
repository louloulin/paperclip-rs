#![forbid(unsafe_code)]

//! Tool argument condition matching.
//! R707: Direct port of tool-access-policy.ts::readPath + argumentFiltersMatch.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Walk a nested Value by dot-separated path.
pub fn read_path(value: &Value, path: &str) -> Option<Value> {
    if path.is_empty() { return None; }
    let mut current = value.clone();
    for segment in path.split('.') {
        match &current {
            Value::Object(map) => {
                match map.get(segment) {
                    Some(v) => current = v.clone(),
                    None => return None,
                }
            }
            Value::Array(arr) => {
                match segment.parse::<usize>() {
                    Ok(idx) => match arr.get(idx) {
                        Some(v) => current = v.clone(),
                        None => return None,
                    },
                    Err(_) => return None,
                }
            }
            _ => return None,
        }
    }
    Some(current)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgumentFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_any: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_hashes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_equals: Option<std::collections::BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_not_equals: Option<std::collections::BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_in: Option<std::collections::BTreeMap<String, Vec<Value>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_matches: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_exists: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_absent: Option<Vec<String>>,
}

fn stable_stringify(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

pub fn argument_filters_match(
    filters: &ArgumentFilters,
    arguments_hash: &str,
    arguments: &Value,
) -> bool {
    if filters.allow_any == Some(true) { return true; }
    if let Some(ref h) = filters.exact_hash {
        if h != arguments_hash { return false; }
    }
    if let Some(ref allowed) = filters.allowed_hashes {
        if !allowed.is_empty() && !allowed.iter().any(|h| h == arguments_hash) { return false; }
    }
    if let Some(ref f) = filters.field_equals {
        for (path, expected) in f {
            let actual = read_path(arguments, path).unwrap_or(Value::Null);
            if stable_stringify(&actual) != stable_stringify(expected) { return false; }
        }
    }
    if let Some(ref f) = filters.field_not_equals {
        for (path, expected) in f {
            let actual = read_path(arguments, path).unwrap_or(Value::Null);
            if stable_stringify(&actual) == stable_stringify(expected) { return false; }
        }
    }
    if let Some(ref f) = filters.field_in {
        for (path, allowed_values) in f {
            let actual = read_path(arguments, path).unwrap_or(Value::Null);
            let actual_str = stable_stringify(&actual);
            if !allowed_values.iter().any(|v| stable_stringify(v) == actual_str) { return false; }
        }
    }
    if let Some(ref f) = filters.field_matches {
        for (path, pattern) in f {
            let actual = read_path(arguments, path);
            match actual {
                Some(Value::String(s)) => {
                    match Regex::new(pattern) {
                        Ok(re) => if !re.is_match(&s) { return false; },
                        Err(_) => return false,
                    }
                }
                _ => return false,
            }
        }
    }
    if let Some(ref paths) = filters.field_exists {
        for path in paths {
            if read_path(arguments, path).is_none() { return false; }
        }
    }
    if let Some(ref paths) = filters.field_absent {
        for path in paths {
            if read_path(arguments, path).is_some() { return false; }
        }
    }
    filters.exact_hash.is_some()
        || filters.allowed_hashes.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
        || filters.field_equals.is_some()
        || filters.field_not_equals.is_some()
        || filters.field_in.is_some()
        || filters.field_matches.is_some()
        || filters.field_exists.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
        || filters.field_absent.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;
    fn args() -> Value { json!({"name": "alice", "age": 30, "tags": ["admin", "user"]}) }
    #[test]
    fn read_path_simple() {
        let a = args();
        assert_eq!(read_path(&a, "name").unwrap(), json!("alice"));
        assert_eq!(read_path(&a, "age").unwrap(), json!(30));
    }
    #[test]
    fn read_path_array_index() {
        let a = args();
        assert_eq!(read_path(&a, "tags.0").unwrap(), json!("admin"));
    }
    #[test]
    fn read_path_missing() {
        let a = args();
        assert!(read_path(&a, "missing").is_none());
        assert!(read_path(&a, "name.foo").is_none());
        assert!(read_path(&a, "tags.99").is_none());
        assert!(read_path(&a, "tags.bad").is_none());
    }
    #[test]
    fn read_path_empty_returns_none() {
        let a = args();
        assert!(read_path(&a, "").is_none());
    }
    #[test]
    fn allow_any_short_circuits() {
        let f = ArgumentFilters { allow_any: Some(true), ..Default::default() };
        assert!(argument_filters_match(&f, "anyhash", &args()));
    }
    #[test]
    fn exact_hash_match() {
        let mut f = ArgumentFilters::default();
        f.exact_hash = Some("abc123".into());
        assert!(argument_filters_match(&f, "abc123", &args()));
        assert!(!argument_filters_match(&f, "xyz", &args()));
    }
    #[test]
    fn allowed_hashes_includes() {
        let mut f = ArgumentFilters::default();
        f.allowed_hashes = Some(vec!["a".into(), "b".into()]);
        assert!(argument_filters_match(&f, "a", &args()));
        assert!(argument_filters_match(&f, "b", &args()));
        assert!(!argument_filters_match(&f, "c", &args()));
    }
    #[test]
    fn field_equals_match() {
        let mut f = ArgumentFilters::default();
        let mut m = std::collections::BTreeMap::new();
        m.insert("name".into(), json!("alice"));
        f.field_equals = Some(m);
        assert!(argument_filters_match(&f, "any", &args()));
    }
    #[test]
    fn field_not_equals() {
        let mut f = ArgumentFilters::default();
        let mut m = std::collections::BTreeMap::new();
        m.insert("name".into(), json!("bob"));
        f.field_not_equals = Some(m);
        assert!(argument_filters_match(&f, "any", &args()));
    }
    #[test]
    fn field_in_match() {
        let mut f = ArgumentFilters::default();
        let mut m = std::collections::BTreeMap::new();
        m.insert("name".into(), vec![json!("alice"), json!("bob")]);
        f.field_in = Some(m);
        assert!(argument_filters_match(&f, "any", &args()));
    }
    #[test]
    fn field_matches_regex() {
        let mut f = ArgumentFilters::default();
        let mut m = std::collections::BTreeMap::new();
        m.insert("name".into(), "^alic.*".into());
        f.field_matches = Some(m);
        assert!(argument_filters_match(&f, "any", &args()));
    }
    #[test]
    fn field_matches_invalid_regex_fails() {
        let mut f = ArgumentFilters::default();
        let mut m = std::collections::BTreeMap::new();
        m.insert("name".into(), "[invalid(".into());
        f.field_matches = Some(m);
        assert!(!argument_filters_match(&f, "any", &args()));
    }
    #[test]
    fn field_exists() {
        let mut f = ArgumentFilters::default();
        f.field_exists = Some(vec!["name".into(), "tags".into()]);
        assert!(argument_filters_match(&f, "any", &args()));
        let mut f2 = ArgumentFilters::default();
        f2.field_exists = Some(vec!["missing_field".into()]);
        assert!(!argument_filters_match(&f2, "any", &args()));
    }
    #[test]
    fn field_absent() {
        let mut f = ArgumentFilters::default();
        f.field_absent = Some(vec!["missing".into()]);
        assert!(argument_filters_match(&f, "any", &args()));
        let mut f2 = ArgumentFilters::default();
        f2.field_absent = Some(vec!["name".into()]);
        assert!(!argument_filters_match(&f2, "any", &args()));
    }
    #[test]
    fn no_filters_returns_false() {
        let f = ArgumentFilters::default();
        assert!(!argument_filters_match(&f, "any", &args()));
    }
    #[test]
    fn multiple_filters_all_must_match() {
        let mut f = ArgumentFilters::default();
        let mut m_eq = std::collections::BTreeMap::new();
        m_eq.insert("name".into(), json!("alice"));
        f.field_equals = Some(m_eq);
        f.field_exists = Some(vec!["tags".into()]);
        assert!(argument_filters_match(&f, "any", &args()));
    }
    #[test]
    fn stable_stringify_handles_null() {
        assert_eq!(stable_stringify(&Value::Null), "null");
    }
}
