//! `pc-acpx` stable hashing — pure helpers that mirror
//! `stableJson` and `shortHash` from Node `acpx-engine/execute.ts`.
//!
//! `stableJson` produces a canonical JSON string for object keys, so two
//! structurally-similar values with different key order produce identical
//! hash material. `shortHash` produces a fixed-length hex digest of the
//! canonical JSON, ready to be used as a config fingerprint.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Canonicalize a JSON value into a stable string. Objects are sorted by key
/// before serialization so permutations of the same map produce identical
/// output. Nested arrays preserve order; numbers/strings/booleans/null are
/// stringified via `serde_json`.
pub fn stable_json(value: &Value) -> String {
    let canonical = canonicalize(value);
    serde_json::to_string(&canonical).unwrap_or_else(|_| "null".to_string())
}

/// Produce a short hex-encoded SHA-256 digest of the canonical JSON form.
pub fn short_hash(value: &Value) -> String {
    let canonical = stable_json(value);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

// ============================================================================
// Internal canonicalization
// ============================================================================

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut sorted = serde_json::Map::with_capacity(entries.len());
            for (key, value) in entries {
                sorted.insert(key, value);
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_json_is_key_order_invariant() {
        let a = serde_json::json!({ "a": 1, "b": 2, "c": 3 });
        let b = serde_json::json!({ "c": 3, "b": 2, "a": 1 });
        assert_eq!(stable_json(&a), stable_json(&b));
    }

    #[test]
    fn stable_json_preserves_array_order() {
        let a = serde_json::json!([1, 2, 3]);
        let b = serde_json::json!([3, 2, 1]);
        assert_ne!(stable_json(&a), stable_json(&b));
    }

    #[test]
    fn stable_json_canonicalizes_nested_objects() {
        let value = serde_json::json!({
            "outer": { "z": 1, "a": [3, 2, 1] },
            "flag": true,
        });
        let expected = r#"{"flag":true,"outer":{"a":[3,2,1],"z":1}}"#;
        assert_eq!(stable_json(&value), expected);
    }

    #[test]
    fn short_hash_is_stable_and_hex() {
        let a = serde_json::json!({ "a": 1, "b": 2 });
        let b = serde_json::json!({ "b": 2, "a": 1 });
        assert_eq!(short_hash(&a), short_hash(&b));
        assert_eq!(short_hash(&a).len(), 64);
        assert!(short_hash(&a).chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn short_hash_differs_for_distinct_values() {
        let a = serde_json::json!({ "a": 1 });
        let b = serde_json::json!({ "a": 2 });
        assert_ne!(short_hash(&a), short_hash(&b));
    }
}
