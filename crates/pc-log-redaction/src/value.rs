//! Recursive redaction over `serde_json::Value` trees.
//!
//! Direct port of `redactCurrentUserValue` from upstream:
//! - string leaves → [`crate::text::redact_current_user_text`]
//! - array leaves → recurse over each element
//! - object leaves → recurse over each value (preserves key order)
//! - other leaves (number / bool / null) → returned unchanged

use serde_json::Value;

use crate::text::redact_current_user_text;
use crate::Options;

/// Recursively redact all string leaves in a JSON `Value`.
///
/// Object keys are NOT redacted (they're typically field names, not PII).
/// Array / object structures are preserved.
#[must_use]
pub fn redact_current_user_value(value: &Value, opts: &Options) -> Value {
    match value {
        Value::String(s) => Value::String(redact_current_user_text(s, opts)),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(redact_current_user_value(item, opts));
            }
            Value::Array(out)
        }
        Value::Object(map) => {
            // serde_json::Map preserves insertion order (BTreeMap / indexmap
            // depending on features); keep deterministic ordering for tests.
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), redact_current_user_value(v, opts));
            }
            Value::Object(out)
        }
        // Number / bool / null → unchanged.
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, StdEnv};
    use serde_json::json;

    fn opts_alice() -> Options {
        Options {
            enabled: true,
            replacement: "*".into(),
            user_names: vec!["alice".into()],
            home_dirs: vec!["/home/alice".into()],
        }
    }

    #[test]
    fn r526_redact_string_leaf() {
        let v = json!("alice's home is /home/alice");
        let out = redact_current_user_value(&v, &opts_alice());
        assert_eq!(out, json!("a****'s home is /home/a****"));
    }

    #[test]
    fn r526_redact_array_of_strings() {
        let v = json!(["alice", "bob", "/home/alice/work"]);
        let out = redact_current_user_value(&v, &opts_alice());
        assert_eq!(out, json!(["a****", "bob", "/home/a****/work"]));
    }

    #[test]
    fn r526_redact_nested_object() {
        let v = json!({
            "user": "alice",
            "home": "/home/alice",
            "metadata": {
                "owner": "alice",
                "safe": 42
            },
            "tags": ["alice", "admin"]
        });
        let out = redact_current_user_value(&v, &opts_alice());
        assert_eq!(out, json!({
            "user": "a****",
            "home": "/home/a****",
            "metadata": {
                "owner": "a****",
                "safe": 42
            },
            "tags": ["a****", "admin"]
        }));
    }

    #[test]
    fn r526_object_keys_not_redacted() {
        // "alice" appears as both key and value — only the value is redacted.
        let v = json!({"alice": "alice"});
        let out = redact_current_user_value(&v, &opts_alice());
        assert_eq!(out, json!({"alice": "a****"}));
    }

    #[test]
    fn r526_passes_through_numbers_and_bools() {
        let v = json!({"n": 42, "b": true, "f": 3.14, "z": null});
        let out = redact_current_user_value(&v, &opts_alice());
        assert_eq!(out, v);
    }

    #[test]
    fn r526_disabled_returns_input_unchanged() {
        let mut opts = opts_alice();
        opts.enabled = false;
        let v = json!("alice is here");
        let out = redact_current_user_value(&v, &opts);
        assert_eq!(out, v);
    }

    #[test]
    fn r526_empty_object_returns_empty_object() {
        let v = json!({});
        let out = redact_current_user_value(&v, &opts_alice());
        assert_eq!(out, v);
    }

    #[test]
    fn r526_deeply_nested_array() {
        let v = json!([[["alice"]]]);
        let out = redact_current_user_value(&v, &opts_alice());
        assert_eq!(out, json!([[["a****"]]]));
    }

    #[test]
    fn r526_with_default_candidates_suppresses_unused() {
        let opts = Options::with_default_candidates(&StdEnv);
        // Just verify it doesn't panic with empty env.
        let v = json!("hello");
        let out = redact_current_user_value(&v, &opts);
        assert_eq!(out, v);
    }
}
