// SPDX-License-Identifier: MIT
//
// R683 parity tests for validatePluginSandboxProviderConfig secret-binding normalize.

use pc_environment::json_schema_secret_refs::SecretRefBindingVersion;
use pc_environment::plugin_environment_driver_validate_config::{
    as_object_schema, normalize_config_secret_refs, schema_for_collect,
    SecretBindingNormalizeError, SecretBindingNormalizeResult,
};
use serde_json::json;

fn binding(id: &str, version: Option<u64>) -> serde_json::Value {
    let mut v = json!({"type": "secret_ref", "secretId": id});
    if let Some(ver) = version {
        v.as_object_mut().unwrap().insert("version".to_string(), json!(ver));
    }
    v
}

#[test]
fn r683_no_schema_returns_unchanged() {
    let config = json!({"a": 1});
    let r = normalize_config_secret_refs(None, &config, "aws").unwrap();
    assert_eq!(r.normalized_config, config);
    assert!(r.rewritten_paths.is_empty());
    assert!(r.skipped_paths.is_empty());
}

#[test]
fn r683_schema_without_secret_ref_passthrough() {
    let schema = json!({"properties": {"region": {"type": "string"}}});
    let config = json!({"region": "us-east-1"});
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(r.normalized_config, config);
}

#[test]
fn r683_single_secret_ref_latest_rewritten() {
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", None)});
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(r.normalized_config, json!({"apiKey": "01234567-89ab-cdef-0123-456789abcdef"}));
    assert_eq!(r.rewritten_paths, vec!["apiKey".to_string()]);
    assert!(r.skipped_paths.is_empty());
}

#[test]
fn r683_explicit_latest_string_rewritten() {
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", Some(0))});
    // Some(0) -> "latest" string (via mutate)
    let mut v = config["apiKey"].clone();
    v.as_object_mut().unwrap().insert("version".to_string(), json!("latest"));
    let config = json!({"apiKey": v});
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(r.normalized_config, json!({"apiKey": "01234567-89ab-cdef-0123-456789abcdef"}));
}

#[test]
fn r683_pinned_numeric_version_throws_error() {
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", Some(3))});
    let err = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap_err();
    match err {
        SecretBindingNormalizeError::PinnedVersion { path, version, provider } => {
            assert_eq!(path, "apiKey");
            assert_eq!(version, "3");
            assert_eq!(provider, "aws");
        }
    }
}

#[test]
fn r683_pinned_version_does_not_rewrite_partial_config() {
    let schema = json!({"properties": {"a": {"format": "secret-ref"}, "b": {"format": "secret-ref"}}});
    let config = json!({
        "a": binding("01234567-89ab-cdef-0123-456789abcdef", None),
        "b": binding("ffffffff-89ab-cdef-0123-456789abcdef", Some(5)),
    });
    let err = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap_err();
    match err {
        SecretBindingNormalizeError::PinnedVersion { path, version, .. } => {
            // The sorted order may pick a or b; either way pinned version error
            assert!(path == "a" || path == "b");
            assert!(version == "5");
        }
    }
}

#[test]
fn r683_malformed_binding_skipped_silently() {
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let config = json!({"apiKey": "just-a-string"});
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(r.normalized_config, config);
    assert_eq!(r.skipped_paths, vec!["apiKey".to_string()]);
    assert!(r.rewritten_paths.is_empty());
}

#[test]
fn r683_missing_secret_ref_leaf_skipped() {
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let config = json!({}); // no apiKey key
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(r.normalized_config, config);
    assert!(r.rewritten_paths.is_empty());
}

#[test]
fn r683_nested_secret_ref_paths() {
    let schema = json!({
        "properties": {
            "database": {
                "properties": {
                    "password": {"format": "secret-ref"}
                }
            }
        }
    });
    let config = json!({"database": {"password": binding("01234567-89ab-cdef-0123-456789abcdef", None)}});
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(
        r.normalized_config,
        json!({"database": {"password": "01234567-89ab-cdef-0123-456789abcdef"}})
    );
    assert_eq!(r.rewritten_paths, vec!["database.password".to_string()]);
}

#[test]
fn r683_multiple_secret_ref_paths_all_rewritten() {
    let schema = json!({
        "properties": {
            "apiKey": {"format": "secret-ref"},
            "ssh": {"properties": {"privateKey": {"format": "secret-ref"}}}
        }
    });
    let config = json!({
        "apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", None),
        "ssh": {"privateKey": binding("ffffffff-89ab-cdef-0123-456789abcdef", None)},
    });
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(
        r.normalized_config,
        json!({
            "apiKey": "01234567-89ab-cdef-0123-456789abcdef",
            "ssh": {"privateKey": "ffffffff-89ab-cdef-0123-456789abcdef"},
        })
    );
    assert_eq!(r.rewritten_paths.len(), 2);
    assert!(r.rewritten_paths.contains(&"apiKey".to_string()));
    assert!(r.rewritten_paths.contains(&"ssh.privateKey".to_string()));
}

#[test]
fn r683_secret_ref_default_version_is_latest() {
    let v = SecretRefBindingVersion::default();
    assert_eq!(v, SecretRefBindingVersion::Latest);
}

#[test]
fn r683_result_default() {
    let r = SecretBindingNormalizeResult::default();
    assert_eq!(r.normalized_config, serde_json::Value::Null);
    assert!(r.rewritten_paths.is_empty());
    assert!(r.skipped_paths.is_empty());
}

#[test]
fn r683_error_display() {
    let e = SecretBindingNormalizeError::PinnedVersion {
        path: "ssh.privateKey".to_string(),
        version: "3".to_string(),
        provider: "aws".to_string(),
    };
    let s = e.to_string();
    assert!(s.contains("ssh.privateKey"));
    assert!(s.contains("3"));
    assert!(s.contains("aws"));
}

#[test]
fn r683_error_eq_clone() {
    let e1 = SecretBindingNormalizeError::PinnedVersion {
        path: "x".to_string(),
        version: "5".to_string(),
        provider: "p".to_string(),
    };
    let e2 = e1.clone();
    assert_eq!(e1, e2);
}

#[test]
fn r683_as_object_schema_accepts_object() {
    let v = json!({"a": 1});
    let r = as_object_schema(Some(&v));
    assert!(r.is_some());
    assert_eq!(r.unwrap().get("a"), Some(&json!(1)));
}

#[test]
fn r683_as_object_schema_rejects_non_object() {
    assert!(as_object_schema(None).is_none());
    assert!(as_object_schema(Some(&json!("x"))).is_none());
    assert!(as_object_schema(Some(&json!(42))).is_none());
    assert!(as_object_schema(Some(&json!([1, 2]))).is_none());
}

#[test]
fn r683_schema_for_collect_returns_object() {
    let v = json!({"a": 1});
    assert!(schema_for_collect(Some(&v)).is_some());
    assert!(schema_for_collect(Some(&json!("x"))).is_none());
    assert!(schema_for_collect(None).is_none());
}

#[test]
fn r683_normalize_does_not_mutate_input() {
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", None)});
    let _ = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    // Input config should still be the binding form
    assert_eq!(config["apiKey"]["type"], "secret_ref");
}

#[test]
fn r683_schema_allof_branches_merge() {
    let schema = json!({
        "allOf": [
            {"properties": {"x": {"format": "secret-ref"}}},
            {"properties": {"y": {"format": "secret-ref"}}}
        ]
    });
    let config = json!({
        "x": binding("01234567-89ab-cdef-0123-456789abcdef", None),
        "y": binding("ffffffff-89ab-cdef-0123-456789abcdef", None),
    });
    let r = normalize_config_secret_refs(Some(&schema), &config, "aws").unwrap();
    assert_eq!(r.rewritten_paths.len(), 2);
}
