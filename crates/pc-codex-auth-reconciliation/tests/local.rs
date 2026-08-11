//! Unit tests for env-binding parsing + classification (no DB needed).

use pc_codex_auth_reconciliation::{
    classify_api_key_binding, parse_adapter_env, read_plain_env_value, ApiKeyBinding,
};
use serde_json::json;

#[test]
fn read_plain_env_value_returns_bare_string_trimmed() {
    let v = json!("  sk-abc  ");
    assert_eq!(read_plain_env_value(Some(&v)), Some("sk-abc".to_string()));
}

#[test]
fn read_plain_env_value_returns_none_for_empty_string() {
    let v = json!("   ");
    assert_eq!(read_plain_env_value(Some(&v)), None);
}

#[test]
fn read_plain_env_value_unwraps_nested_plain_object() {
    let v = json!({ "type": "plain", "value": "sk-xyz" });
    assert_eq!(read_plain_env_value(Some(&v)), Some("sk-xyz".to_string()));
}

#[test]
fn read_plain_env_value_returns_none_for_secret_ref() {
    let v = json!({ "type": "secret_ref", "ref": "abc" });
    assert_eq!(read_plain_env_value(Some(&v)), None);
}

#[test]
fn read_plain_env_value_returns_none_for_null() {
    assert_eq!(read_plain_env_value(None), None);
}

#[test]
fn read_plain_env_value_returns_none_for_non_string_non_object() {
    let v = json!(123);
    assert_eq!(read_plain_env_value(Some(&v)), None);
}

#[test]
fn classify_plain_string() {
    let v = json!("sk-plain");
    assert_eq!(
        classify_api_key_binding(Some(&v)),
        ApiKeyBinding::Plain {
            value: "sk-plain".to_string()
        }
    );
}

#[test]
fn classify_plain_nested_object() {
    let v = json!({ "type": "plain", "value": "sk-plain" });
    assert_eq!(
        classify_api_key_binding(Some(&v)),
        ApiKeyBinding::Plain {
            value: "sk-plain".to_string()
        }
    );
}

#[test]
fn classify_secret_ref() {
    let v = json!({ "type": "secret_ref", "ref": "secret-1" });
    assert_eq!(classify_api_key_binding(Some(&v)), ApiKeyBinding::Secret);
}

#[test]
fn classify_legacy_secret_shape() {
    let v = json!({ "type": "secret", "id": "abc" });
    assert_eq!(classify_api_key_binding(Some(&v)), ApiKeyBinding::Secret);
}

#[test]
fn classify_none_for_missing() {
    assert_eq!(classify_api_key_binding(None), ApiKeyBinding::None);
}

#[test]
fn classify_none_for_null_value() {
    let v = json!(null);
    assert_eq!(classify_api_key_binding(Some(&v)), ApiKeyBinding::None);
}

#[test]
fn classify_none_for_empty_object() {
    let v = json!({});
    assert_eq!(classify_api_key_binding(Some(&v)), ApiKeyBinding::None);
}

#[test]
fn parse_adapter_env_extracts_env_subobject() {
    let txt = json!({
        "env": {
            "CODEX_HOME": "/tmp/codex-home",
            "OPENAI_API_KEY": "sk-abc"
        }
    })
    .to_string();
    let env = parse_adapter_env(&txt).expect("env");
    assert_eq!(
        env.get("CODEX_HOME").unwrap().as_str(),
        Some("/tmp/codex-home")
    );
}

#[test]
fn parse_adapter_env_returns_none_for_non_object_root() {
    let txt = "[]".to_string();
    assert!(parse_adapter_env(&txt).is_none());
}

#[test]
fn parse_adapter_env_returns_none_for_missing_env() {
    let txt = json!({ "foo": "bar" }).to_string();
    assert!(parse_adapter_env(&txt).is_none());
}

#[test]
fn parse_adapter_env_returns_none_for_invalid_json() {
    let txt = "not json";
    assert!(parse_adapter_env(txt).is_none());
}
