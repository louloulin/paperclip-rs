// SPDX-License-Identifier: MIT
//
// R682 parity tests for `json-schema-secret-refs.ts` pure helpers.

use pc_environment::json_schema_secret_refs::{
    collect_secret_ref_paths, is_uuid_secret_ref, parse_secret_ref_binding_object,
    read_config_value_at_path, write_config_value_at_path, SecretRefBindingObject,
    SecretRefBindingVersion,
};
use serde_json::{json, Value};
use std::collections::HashSet;

// =============================================================================
// isUuidSecretRef
// =============================================================================

#[test]
fn r682_is_uuid_secret_ref_valid_lower() {
    assert!(is_uuid_secret_ref("01234567-89ab-cdef-0123-456789abcdef"));
}

#[test]
fn r682_is_uuid_secret_ref_valid_upper() {
    assert!(is_uuid_secret_ref("01234567-89AB-CDEF-0123-456789ABCDEF"));
}

#[test]
fn r682_is_uuid_secret_ref_valid_mixed_case() {
    assert!(is_uuid_secret_ref("01234567-89Ab-cDeF-0123-456789AbCdEf"));
}

#[test]
fn r682_is_uuid_secret_ref_rejects_empty() {
    assert!(!is_uuid_secret_ref(""));
}

#[test]
fn r682_is_uuid_secret_ref_rejects_too_short() {
    assert!(!is_uuid_secret_ref("01234567-89ab-cdef-0123-456789abcde"));
}

#[test]
fn r682_is_uuid_secret_ref_rejects_non_hex() {
    assert!(!is_uuid_secret_ref("01234567-89ab-cdef-0123-456789abcdeg"));
    assert!(!is_uuid_secret_ref("zzzzzzzz-89ab-cdef-0123-456789abcdef"));
}

#[test]
fn r682_is_uuid_secret_ref_rejects_missing_dashes() {
    assert!(!is_uuid_secret_ref("0123456789abcdef0123456789abcdef"));
}

#[test]
fn r682_is_uuid_secret_ref_rejects_extra_chars() {
    assert!(!is_uuid_secret_ref("01234567-89ab-cdef-0123-456789abcdef0"));
}

// =============================================================================
// parseSecretRefBindingObject
// =============================================================================

#[test]
fn r682_parse_secret_ref_basic_latest() {
    let v = json!({"type": "secret_ref", "secretId": "01234567-89ab-cdef-0123-456789abcdef"});
    let r = parse_secret_ref_binding_object(&v).unwrap();
    assert_eq!(r.secret_id, "01234567-89ab-cdef-0123-456789abcdef");
    assert_eq!(r.version, SecretRefBindingVersion::Latest);
}

#[test]
fn r682_parse_secret_ref_explicit_latest_string() {
    let v = json!({
        "type": "secret_ref",
        "secretId": "01234567-89ab-cdef-0123-456789abcdef",
        "version": "latest",
    });
    let r = parse_secret_ref_binding_object(&v).unwrap();
    assert_eq!(r.version, SecretRefBindingVersion::Latest);
}

#[test]
fn r682_parse_secret_ref_null_version() {
    let v = json!({
        "type": "secret_ref",
        "secretId": "01234567-89ab-cdef-0123-456789abcdef",
        "version": null,
    });
    let r = parse_secret_ref_binding_object(&v).unwrap();
    assert_eq!(r.version, SecretRefBindingVersion::Latest);
}

#[test]
fn r682_parse_secret_ref_numeric_version() {
    let v = json!({
        "type": "secret_ref",
        "secretId": "01234567-89ab-cdef-0123-456789abcdef",
        "version": 5,
    });
    let r = parse_secret_ref_binding_object(&v).unwrap();
    assert_eq!(r.version, SecretRefBindingVersion::Number(5));
}

#[test]
fn r682_parse_secret_ref_rejects_wrong_type() {
    let v = json!({"type": "plain", "secretId": "01234567-89ab-cdef-0123-456789abcdef"});
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_missing_type() {
    let v = json!({"secretId": "01234567-89ab-cdef-0123-456789abcdef"});
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_bad_uuid() {
    let v = json!({"type": "secret_ref", "secretId": "not-a-uuid"});
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_non_string_secret_id() {
    let v = json!({"type": "secret_ref", "secretId": 12345});
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_zero_version() {
    let v = json!({
        "type": "secret_ref",
        "secretId": "01234567-89ab-cdef-0123-456789abcdef",
        "version": 0,
    });
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_negative_version() {
    let v = json!({
        "type": "secret_ref",
        "secretId": "01234567-89ab-cdef-0123-456789abcdef",
        "version": -1,
    });
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_non_integer_version() {
    let v = json!({
        "type": "secret_ref",
        "secretId": "01234567-89ab-cdef-0123-456789abcdef",
        "version": 1.5,
    });
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_string_version_other_than_latest() {
    let v = json!({
        "type": "secret_ref",
        "secretId": "01234567-89ab-cdef-0123-456789abcdef",
        "version": "v1",
    });
    assert!(parse_secret_ref_binding_object(&v).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_null_input() {
    assert!(parse_secret_ref_binding_object(&Value::Null).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_string_input() {
    assert!(parse_secret_ref_binding_object(&json!("raw")).is_none());
}

#[test]
fn r682_parse_secret_ref_rejects_array_input() {
    assert!(parse_secret_ref_binding_object(&json!([])).is_none());
}

#[test]
fn r682_parse_secret_ref_accepts_uuid_with_whitespace() {
    // Node uses record.secretId.trim() before UUID check
    let v = json!({"type": "secret_ref", "secretId": "  01234567-89ab-cdef-0123-456789abcdef  "});
    let r = parse_secret_ref_binding_object(&v).unwrap();
    assert_eq!(r.secret_id, "01234567-89ab-cdef-0123-456789abcdef");
}

// =============================================================================
// collectSecretRefPaths
// =============================================================================

#[test]
fn r682_collect_secret_ref_paths_null_input() {
    let p = collect_secret_ref_paths(None);
    assert!(p.is_empty());
}

#[test]
fn r682_collect_secret_ref_paths_empty_object() {
    let p = collect_secret_ref_paths(Some(&json!({})));
    assert!(p.is_empty());
}

#[test]
fn r682_collect_secret_ref_paths_no_properties() {
    let p = collect_secret_ref_paths(Some(&json!({"type": "object"})));
    assert!(p.is_empty());
}

#[test]
fn r682_collect_secret_ref_paths_single_top_level() {
    let s = json!({
        "type": "object",
        "properties": {
            "apiKey": {"type": "string", "format": "secret-ref"}
        }
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(p, HashSet::from(["apiKey".to_string()]));
}

#[test]
fn r682_collect_secret_ref_paths_nested() {
    let s = json!({
        "type": "object",
        "properties": {
            "database": {
                "type": "object",
                "properties": {
                    "password": {"type": "string", "format": "secret-ref"}
                }
            }
        }
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(p, HashSet::from(["database.password".to_string()]));
}

#[test]
fn r682_collect_secret_ref_paths_deeply_nested() {
    let s = json!({
        "properties": {
            "a": {
                "properties": {
                    "b": {
                        "properties": {
                            "c": {"format": "secret-ref"}
                        }
                    }
                }
            }
        }
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(p, HashSet::from(["a.b.c".to_string()]));
}

#[test]
fn r682_collect_secret_ref_paths_multiple_at_same_level() {
    let s = json!({
        "properties": {
            "a": {"format": "secret-ref"},
            "b": {"format": "secret-ref"},
            "c": {"type": "string"}
        }
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(
        p,
        HashSet::from(["a".to_string(), "b".to_string()])
    );
}

#[test]
fn r682_collect_secret_ref_paths_allof_merges() {
    let s = json!({
        "allOf": [
            {"properties": {"x": {"format": "secret-ref"}}},
            {"properties": {"y": {"format": "secret-ref"}}}
        ]
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(
        p,
        HashSet::from(["x".to_string(), "y".to_string()])
    );
}

#[test]
fn r682_collect_secret_ref_paths_anyof_merges() {
    let s = json!({
        "anyOf": [
            {"properties": {"p": {"format": "secret-ref"}}},
            {"properties": {"q": {"format": "secret-ref"}}}
        ]
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(
        p,
        HashSet::from(["p".to_string(), "q".to_string()])
    );
}

#[test]
fn r682_collect_secret_ref_paths_oneof_merges() {
    let s = json!({
        "oneOf": [
            {"properties": {"a": {"format": "secret-ref"}}},
            {"properties": {"b": {"format": "secret-ref"}}}
        ]
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(
        p,
        HashSet::from(["a".to_string(), "b".to_string()])
    );
}

#[test]
fn r682_collect_secret_ref_paths_format_other_than_secret_ref_ignored() {
    let s = json!({
        "properties": {
            "email": {"format": "email"},
            "uri": {"format": "uri"},
            "password": {"format": "secret-ref"},
        }
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(p, HashSet::from(["password".to_string()]));
}

#[test]
fn r682_collect_secret_ref_paths_non_object_input() {
    let p = collect_secret_ref_paths(Some(&json!("string")));
    assert!(p.is_empty());
    let p = collect_secret_ref_paths(Some(&json!(42)));
    assert!(p.is_empty());
    let p = collect_secret_ref_paths(Some(&json!([])));
    assert!(p.is_empty());
}

#[test]
fn r682_collect_secret_ref_paths_property_value_non_object() {
    let s = json!({
        "properties": {
            "ok": "not-an-object",
            "real": {"format": "secret-ref"}
        }
    });
    let p = collect_secret_ref_paths(Some(&s));
    assert_eq!(p, HashSet::from(["real".to_string()]));
}

// =============================================================================
// readConfigValueAtPath
// =============================================================================

#[test]
fn r682_read_config_value_top_level() {
    let c = json!({"a": 1});
    assert_eq!(read_config_value_at_path(&c, "a"), Some(&json!(1)));
}

#[test]
fn r682_read_config_value_nested() {
    let c = json!({"a": {"b": {"c": "deep"}}});
    assert_eq!(read_config_value_at_path(&c, "a.b.c"), Some(&json!("deep")));
}

#[test]
fn r682_read_config_value_missing_key_returns_none() {
    let c = json!({"a": 1});
    assert_eq!(read_config_value_at_path(&c, "missing"), None);
}

#[test]
fn r682_read_config_value_missing_nested_returns_none() {
    let c = json!({"a": {"b": 1}});
    assert_eq!(read_config_value_at_path(&c, "a.missing"), None);
}

#[test]
fn r682_read_config_value_traverses_array_returns_none() {
    let c = json!({"a": [1, 2, 3]});
    // Node: Array.isArray branch → returns undefined (None).
    assert_eq!(read_config_value_at_path(&c, "a.0"), None);
}

#[test]
fn r682_read_config_value_traverses_string_returns_none() {
    let c = json!({"a": "hello"});
    assert_eq!(read_config_value_at_path(&c, "a.b"), None);
}

#[test]
fn r682_read_config_value_null_returns_none() {
    let c = json!({"a": null});
    assert_eq!(read_config_value_at_path(&c, "a"), Some(&Value::Null));
    assert_eq!(read_config_value_at_path(&c, "a.b"), None);
}

#[test]
fn r682_read_config_value_root() {
    let c = json!({"a": 1});
    // Empty path is not supported in Node — splitting yields [""], fails.
    assert_eq!(read_config_value_at_path(&c, ""), None);
}

#[test]
fn r682_read_config_value_boolean_and_number() {
    let c = json!({"b": true, "n": 42});
    assert_eq!(read_config_value_at_path(&c, "b"), Some(&json!(true)));
    assert_eq!(read_config_value_at_path(&c, "n"), Some(&json!(42)));
}

// =============================================================================
// writeConfigValueAtPath
// =============================================================================

#[test]
fn r682_write_config_value_top_level() {
    let c = json!({"a": 1});
    let r = write_config_value_at_path(&c, "b", Some(&json!(2)));
    assert_eq!(r, json!({"a": 1, "b": 2}));
}

#[test]
fn r682_write_config_value_nested_new() {
    let c = json!({});
    let r = write_config_value_at_path(&c, "a.b.c", Some(&json!("v")));
    assert_eq!(r, json!({"a": {"b": {"c": "v"}}}));
}

#[test]
fn r682_write_config_value_nested_existing() {
    let c = json!({"a": {"b": 1}});
    let r = write_config_value_at_path(&c, "a.b", Some(&json!(2)));
    assert_eq!(r, json!({"a": {"b": 2}}));
}

#[test]
fn r682_write_config_value_does_not_mutate_input() {
    let c = json!({"a": {"b": 1}});
    let _r = write_config_value_at_path(&c, "a.b", Some(&json!(2)));
    assert_eq!(c, json!({"a": {"b": 1}})); // original unchanged
}

#[test]
fn r682_write_config_value_none_deletes_leaf() {
    let c = json!({"a": {"b": 1, "c": 2}});
    let r = write_config_value_at_path(&c, "a.b", None);
    assert_eq!(r, json!({"a": {"c": 2}}));
}

#[test]
fn r682_write_config_value_intermediate_non_object_replaced() {
    let c = json!({"a": "scalar"});
    let r = write_config_value_at_path(&c, "a.b.c", Some(&json!(42)));
    assert_eq!(r, json!({"a": {"b": {"c": 42}}}));
}

#[test]
fn r682_write_config_value_intermediate_array_replaced() {
    let c = json!({"a": [1, 2, 3]});
    let r = write_config_value_at_path(&c, "a.b", Some(&json!("x")));
    assert_eq!(r, json!({"a": {"b": "x"}}));
}

#[test]
fn r682_write_config_value_complex_roundtrip_with_read() {
    let c = json!({});
    let r1 = write_config_value_at_path(&c, "auth.token", Some(&json!("secret-value")));
    let read_back = read_config_value_at_path(&r1, "auth.token");
    assert_eq!(read_back, Some(&json!("secret-value")));
}

#[test]
fn r682_write_config_value_value_object_passthrough() {
    let c = json!({});
    let new_value = json!({"nested": {"x": 1}});
    let r = write_config_value_at_path(&c, "deep.path", Some(&new_value));
    assert_eq!(r, json!({"deep": {"path": {"nested": {"x": 1}}}}));
}

#[test]

// =============================================================================
// Integration: parseSecretRefBindingObject + readConfigValueAtPath
// =============================================================================

#[test]
fn r682_integration_parse_then_read() {
    let config = json!({
        "database": {
            "password": {
                "type": "secret_ref",
                "secretId": "01234567-89ab-cdef-0123-456789abcdef",
                "version": 3
            }
        }
    });
    let value = read_config_value_at_path(&config, "database.password").unwrap();
    let binding = parse_secret_ref_binding_object(value).unwrap();
    assert_eq!(binding.secret_id, "01234567-89ab-cdef-0123-456789abcdef");
    assert_eq!(binding.version, SecretRefBindingVersion::Number(3));
}

#[test]
fn r682_integration_collect_then_validate_all() {
    let schema = json!({
        "properties": {
            "apiKey": {"format": "secret-ref"},
            "ssh": {
                "properties": {
                    "privateKey": {"format": "secret-ref"}
                }
            }
        }
    });
    let config = json!({
        "apiKey": {
            "type": "secret_ref",
            "secretId": "01234567-89ab-cdef-0123-456789abcdef"
        },
        "ssh": {
            "privateKey": {
                "type": "secret_ref",
                "secretId": "ffffffff-89ab-cdef-0123-456789abcdef",
                "version": "latest"
            }
        }
    });
    let paths = collect_secret_ref_paths(Some(&schema));
    for path in &paths {
        let value = read_config_value_at_path(&config, path).unwrap();
        let binding = parse_secret_ref_binding_object(value);
        assert!(
            binding.is_some(),
            "path {} should yield a valid binding",
            path
        );
    }
    assert_eq!(paths.len(), 2);
}

#[test]
fn r682_secret_ref_binding_object_default_version() {
    let b = SecretRefBindingObject {
        secret_id: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
        version: SecretRefBindingVersion::default(),
    };
    assert_eq!(b.version, SecretRefBindingVersion::Latest);
}

#[test]
fn r682_write_config_value_empty_path_writes_empty_key() {
    let c = json!({"a": 1});
    let r = write_config_value_at_path(&c, "", Some(&json!(99)));
    // Node: "".split(".") = [""], leaf = "", writes cursor[""] = 99.
    assert_eq!(r, json!({"": 99, "a": 1}));
}
