#![forbid(unsafe_code)]

//! Misc tool pure helpers — 1:1 port of paperclip/server/src/services/tool-access.ts
//! and tool-gateway.ts. These are zero-DB helpers extracted for testability.
//!
//! R719: covers schema, normalize, percent/percentile, OAuth actor, connection
//! uid, FK violation detection, and decision-denial reason derivation.

use serde_json::Value;

pub const CONNECTION_KEY_MAX_LEN: usize = 160;
pub const DEFAULT_TOOL_KEY: &str = "tool";

/// Default FK depth limit when walking an error cause chain.
pub const FK_VIOLATION_CAUSE_DEPTH: usize = 4;

/// Detect whether a JSON Schema has any input properties.
///
/// Node parity: `schemaHasInputProperties(schema)` — truthy iff the schema
/// is an object with a non-empty `properties` object.
pub fn schema_has_input_properties(schema: Option<&Value>) -> bool {
    let Some(v) = schema else { return false; };
    let Some(obj) = v.as_object() else { return false; };
    match obj.get("properties") {
        Some(Value::Object(m)) => !m.is_empty(),
        _ => false,
    }
}

/// Parse a numeric value from either a JSON number or a numeric string.
///
/// Node parity: `numberValue(value)` — returns null if not finite.
pub fn number_value(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    let parsed = match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
        _ => return None,
    };
    if parsed.is_finite() { Some(parsed) } else { None }
}

/// Compute a percentage rounded to one decimal place.
///
/// Node parity: `percent(numerator, denominator)` — returns 0 when denominator <= 0.
pub fn percent(numerator: f64, denominator: f64) -> f64 {
    if denominator <= 0.0 { return 0.0; }
    let v = (numerator / denominator) * 1000.0;
    (v.round()) / 10.0
}

/// Compute the p-th percentile of a list of numbers (1-indexed at boundaries).
///
/// Node parity: `percentile(values, p)` — uses ceiling-based indexing.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() { return None; }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let len = sorted.len();
    let raw = ((p / 100.0) * len as f64).ceil() as isize - 1;
    let idx = raw.max(0).min((len - 1) as isize) as usize;
    sorted.get(idx).copied()
}

/// Normalize an arbitrary string into a tool/connection key.
///
/// Node parity: `normalizeKey(input)` — lowercase, keep alphanumerics +
/// `._:` and `-`, trim dashes, cap length, fall back to `DEFAULT_TOOL_KEY`.
pub fn normalize_key(input: &str) -> String {
    let lower = input.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_dash = true;
    for ch in lower.chars() {
        let is_safe = ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == ':';
        if is_safe {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        DEFAULT_TOOL_KEY.to_string()
    } else if trimmed.len() > CONNECTION_KEY_MAX_LEN {
        trimmed[..CONNECTION_KEY_MAX_LEN].to_string()
    } else {
        trimmed
    }
}

/// Build a deterministic connection UID.
///
/// Node parity: `connectionUid(namespace, name, connectionId)`.
pub fn connection_uid(namespace: &str, name: &str, connection_id: &str) -> String {
    let head = &connection_id[..connection_id.len().min(8)];
    format!("{}/{}-{}", normalize_key(namespace), normalize_key(name), head)
}

/// Detect a foreign-key violation on tool_connections.application_id.
///
/// Node parity: walks error.cause chain up to 4 levels deep, looking for
/// code 23503 with constraint or message referencing tool_connections.
pub fn is_tool_connection_foreign_key_violation(error: Option<&Value>) -> bool {
    let mut current = error.cloned();
    for _ in 0..FK_VIOLATION_CAUSE_DEPTH {
        let Some(err) = current else { break; };
        let Some(obj) = err.as_object() else { break; };
        let code = obj.get("code").and_then(Value::as_str);
        let constraint = obj.get("constraint").and_then(Value::as_str)
            .or_else(|| obj.get("constraint_name").and_then(Value::as_str));
        let message = obj.get("message").and_then(Value::as_str).unwrap_or("");
        if code == Some("23503") {
            let constraint_ok = constraint.map(|c| c.contains("tool_connections")).unwrap_or(false);
            let message_ok = message.contains("tool_connections");
            let exact_constraint = constraint == Some("tool_connections_application_id_tool_applications_id_fk");
            if constraint_ok || message_ok || exact_constraint { return true; }
        }
        current = obj.get("cause").cloned();
    }
    false
}

/// Extract OAuth actor type from a string.
///
/// Node parity: `oauthActorType(value)` — returns None for unknown values.
pub fn oauth_actor_type(value: Option<&str>) -> Option<&'static str> {
    match value {
        Some("user") => Some("user"),
        Some("agent") => Some("agent"),
        Some("board") => Some("board"),
        _ => None,
    }
}

/// Build a fallback display name from a user/actor id.
///
/// Node parity: `userFallbackName(userId)` — returns `"user:<prefix>"` when
/// the user record is unavailable.
pub fn user_fallback_name(user_id: &str) -> String {
    let prefix: String = user_id.chars().take(8).collect();
    format!("user:{}", prefix)
}

/// Derive a human-readable denial reason from a decision.
///
/// Node parity: `denialReasonForDecision(decision)` — prefers
/// `decision.denialReason`, falls back to `"denied by review"`.
pub fn denial_reason_for_decision(decision: &Value) -> String {
    if let Some(r) = decision.get("denialReason").and_then(Value::as_str) {
        let t = r.trim();
        if !t.is_empty() { return t.to_string(); }
    }
    "denied by review".to_string()
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_has_input_properties_basic() {
        assert!(schema_has_input_properties(Some(&json!({"properties": {"a": {}}}))));
        assert!(!schema_has_input_properties(Some(&json!({}))));
        assert!(!schema_has_input_properties(Some(&json!({"properties": {}}))));
        assert!(!schema_has_input_properties(Some(&json!("not a schema"))));
        assert!(!schema_has_input_properties(None));
    }

    #[test]
    fn number_value_from_json_and_string() {
        assert_eq!(number_value(Some(&json!(42))).unwrap(), 42.0);
        assert_eq!(number_value(Some(&json!("3.14"))).unwrap(), 3.14);
        assert_eq!(number_value(Some(&json!("-1"))).unwrap(), -1.0);
        assert!(number_value(Some(&json!("abc"))).is_none());
        assert!(number_value(Some(&json!(null))).is_none());
    }

    #[test]
    fn percent_basic_and_zero_denominator() {
        assert!((percent(1.0, 4.0) - 25.0).abs() < 1e-9);
        assert!((percent(0.0, 100.0) - 0.0).abs() < 1e-9);
        assert!((percent(2.0, 3.0) - 66.7).abs() < 1e-9);
        assert_eq!(percent(5.0, 0.0), 0.0);
        assert_eq!(percent(5.0, -1.0), 0.0);
    }

    #[test]
    fn percentile_ceiling_indexing() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        // p=50 of 5 -> ceil(2.5)=3 -> idx=2 -> 3
        assert_eq!(percentile(&v, 50.0).unwrap(), 3.0);
        // p=100 -> idx=4 -> 5
        assert_eq!(percentile(&v, 100.0).unwrap(), 5.0);
        // p=0 -> idx=0 -> 1
        assert_eq!(percentile(&v, 0.0).unwrap(), 1.0);
        assert!(percentile(&[], 50.0).is_none());
    }

    #[test]
    fn normalize_key_basic() {
        assert_eq!(normalize_key("Hello World"), "hello-world");
        assert_eq!(normalize_key("foo_bar.baz:qux"), "foo_bar.baz:qux");
        assert_eq!(normalize_key("  --bad--  "), "bad");
        assert_eq!(normalize_key(""), DEFAULT_TOOL_KEY);
        assert_eq!(normalize_key("###"), DEFAULT_TOOL_KEY);
        let long = "a".repeat(200);
        assert_eq!(normalize_key(&long).len(), CONNECTION_KEY_MAX_LEN);
    }

    #[test]
    fn connection_uid_format() {
        let uid = connection_uid("Acme Tools", "Google Sheets", "abcdef1234567890");
        assert_eq!(uid, "acme-tools/google-sheets-abcdef12");
    }

    #[test]
    fn fk_violation_detection() {
        let err = json!({"code": "23503", "constraint": "tool_connections_application_id_tool_applications_id_fk"});
        assert!(is_tool_connection_foreign_key_violation(Some(&err)));
        let wrapped = json!({"message": "wrapped", "cause": {"code": "23503", "constraint": "tool_connections_xxx"}});
        assert!(is_tool_connection_foreign_key_violation(Some(&wrapped)));
        let wrong = json!({"code": "23505"});
        assert!(!is_tool_connection_foreign_key_violation(Some(&wrong)));
        assert!(!is_tool_connection_foreign_key_violation(None));
    }

    #[test]
    fn oauth_actor_type_variants() {
        assert_eq!(oauth_actor_type(Some("user")), Some("user"));
        assert_eq!(oauth_actor_type(Some("agent")), Some("agent"));
        assert_eq!(oauth_actor_type(Some("board")), Some("board"));
        assert_eq!(oauth_actor_type(Some("alien")), None);
        assert_eq!(oauth_actor_type(None), None);
    }

    #[test]
    fn user_fallback_name_truncates() {
        assert_eq!(user_fallback_name("abcdefghij"), "user:abcdefgh");
        assert_eq!(user_fallback_name("short"), "user:short");
    }

    #[test]
    fn denial_reason_prefers_explicit() {
        let d = json!({"denialReason": "too risky"});
        assert_eq!(denial_reason_for_decision(&d), "too risky");
        let empty = json!({"denialReason": "   "});
        assert_eq!(denial_reason_for_decision(&empty), "denied by review");
        let missing = json!({});
        assert_eq!(denial_reason_for_decision(&missing), "denied by review");
    }
}
