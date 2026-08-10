//! Pure-Rust tests for normalize / is_responsible_user_denial_code re-exports.

use pc_responsible_user_denial_run_outcomes::{
    is_responsible_user_denial_code, normalize_responsible_user_denial_code,
};
use serde_json::json;

#[test]
fn normalize_returns_some_for_known_code() {
    let v = json!("rate_limited");
    let c = normalize_responsible_user_denial_code(&v);
    assert!(c.is_some());
    assert_eq!(c.unwrap().as_str(), "rate_limited");
}

#[test]
fn normalize_returns_none_for_unknown_string() {
    let v = json!("not_a_real_code");
    assert!(normalize_responsible_user_denial_code(&v).is_none());
}

#[test]
fn normalize_returns_none_for_non_string() {
    assert!(normalize_responsible_user_denial_code(&json!(42)).is_none());
    assert!(normalize_responsible_user_denial_code(&json!(null)).is_none());
    assert!(normalize_responsible_user_denial_code(&json!({})).is_none());
    assert!(normalize_responsible_user_denial_code(&json!([1, 2])).is_none());
}

#[test]
fn is_code_recognizes_known_codes() {
    assert!(is_responsible_user_denial_code("rate_limited"));
    assert!(is_responsible_user_denial_code("unsupported_channel"));
    assert!(is_responsible_user_denial_code("quota_exceeded"));
    assert!(is_responsible_user_denial_code("not_entitled"));
    assert!(is_responsible_user_denial_code("other"));
}

#[test]
fn is_code_rejects_unknown() {
    assert!(!is_responsible_user_denial_code("nope"));
    assert!(!is_responsible_user_denial_code(""));
    assert!(!is_responsible_user_denial_code("RANDOM"));
}
