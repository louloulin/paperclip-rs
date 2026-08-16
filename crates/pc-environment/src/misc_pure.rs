#![forbid(unsafe_code)]

//! Misc environment pure helpers \u2014 1:1 port of paperclip/server/src/services/environments.ts
//!
//! R721: zero-DB helpers extracted for testability.

use serde_json::{Map, Value};

/// Clone a JSON object into a fresh Map. Returns the fallback when value is
/// not a plain object (array / scalar / null).
///
/// Node parity: cloneRecord(value, fallback) \u2014 returns a shallow copy.
pub fn clone_record(value: Option<&Value>, fallback: Option<Map<String, Value>>) -> Option<Map<String, Value>> {
    match value {
        Some(Value::Object(m)) => Some(m.clone()),
        _ => fallback,
    }
}

/// Read an enum value, throwing if the value is not in the allowed set.
///
/// Node parity: readEnum<T>(value, allowed, fieldName) \u2014 null returns None.
pub fn read_enum(value: Option<&str>, allowed: &[&'static str], field_name: &str) -> Result<Option<&'static str>, String> {
    match value {
        None => Ok(None),
        Some(v) => {
            if let Some(hit) = allowed.iter().find(|a| **a == v).copied() {
                Ok(Some(hit))
            } else {
                Err(format!("Unexpected {} value: {}", field_name, v))
            }
        }
    }
}

/// Walk an error and its cause chain looking for a specific PG constraint name.
///
/// Node parity: hasConstraintName(error, constraintName) \u2014 accepts either
///  or .
pub fn has_constraint_name(error: Option<&Value>, constraint_name: &str) -> bool {
    let mut current = error.cloned();
    let mut depth = 0usize;
    const MAX_DEPTH: usize = 32;
    while let Some(err) = current {
        if depth >= MAX_DEPTH { break; }
        depth += 1;
        if let Some(obj) = err.as_object() {
            let constraint = obj.get("constraint").and_then(Value::as_str);
            let constraint_name_alt = obj.get("constraint_name").and_then(Value::as_str);
            if constraint == Some(constraint_name) || constraint_name_alt == Some(constraint_name) {
                return true;
            }
            current = obj.get("cause").cloned();
        } else {
            break;
        }
    }
    false
}

/// Disambiguate a polymorphic filter argument.
pub fn resolve_list_filters_string_or_object(
    company_id_or_filters: Option<&str>,
    maybe_filters: Option<Value>,
) -> Value {
    if company_id_or_filters.is_some() {
        maybe_filters.unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        maybe_filters.unwrap_or(Value::Object(Map::new()))
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clone_record_object_returns_copy() {
        let v = json!({"a": 1});
        let m = clone_record(Some(&v), None).unwrap();
        assert_eq!(m.get("a").unwrap(), &json!(1));
    }

    #[test]
    fn clone_record_non_object_uses_fallback() {
        let v = json!([1, 2]);
        let fb = Map::new();
        let m = clone_record(Some(&v), Some(fb));
        assert!(m.is_some());
        assert!(m.unwrap().is_empty());
    }

    #[test]
    fn clone_record_none_returns_none() {
        assert!(clone_record(None, None).is_none());
    }

    #[test]
    fn read_enum_valid() {
        let out = read_enum(Some("docker"), &["docker", "kubernetes"], "driver").unwrap();
        assert_eq!(out, Some("docker"));
    }

    #[test]
    fn read_enum_null_returns_none() {
        let out = read_enum(None, &["docker"], "driver").unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn read_enum_invalid_errors() {
        assert!(read_enum(Some("alien"), &["docker"], "driver").is_err());
    }

    #[test]
    fn has_constraint_name_direct() {
        let err = json!({"constraint": "abc_idx"});
        assert!(has_constraint_name(Some(&err), "abc_idx"));
    }

    #[test]
    fn has_constraint_name_alt_field() {
        let err = json!({"constraint_name": "abc_idx"});
        assert!(has_constraint_name(Some(&err), "abc_idx"));
    }

    #[test]
    fn has_constraint_name_cause_chain() {
        let err = json!({"cause": {"constraint": "target_idx"}});
        assert!(has_constraint_name(Some(&err), "target_idx"));
    }

    #[test]
    fn has_constraint_name_wrong_constraint() {
        let err = json!({"constraint": "other_idx"});
        assert!(!has_constraint_name(Some(&err), "target_idx"));
    }

    #[test]
    fn has_constraint_name_non_object() {
        assert!(!has_constraint_name(Some(&json!("string")), "any"));
        assert!(!has_constraint_name(None, "any"));
    }
}
