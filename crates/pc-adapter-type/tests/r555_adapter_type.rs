//! R555 — pc-adapter-type 综合测试。

#![allow(clippy::doc_markdown)]

use pc_adapter_type::{
    is_builtin_adapter_type, normalize_agent_adapter_type, validate_agent_adapter_type,
    validate_optional_agent_adapter_type, DEFAULT_AGENT_ADAPTER_TYPE,
};

#[test]
fn r555_default_is_process() {
    assert_eq!(DEFAULT_AGENT_ADAPTER_TYPE, "process");
}

#[test]
fn r555_normalize_none_returns_default() {
    assert_eq!(
        normalize_agent_adapter_type(None),
        DEFAULT_AGENT_ADAPTER_TYPE
    );
}

#[test]
fn r555_normalize_empty_returns_default() {
    assert_eq!(
        normalize_agent_adapter_type(Some("")),
        DEFAULT_AGENT_ADAPTER_TYPE
    );
}

#[test]
fn r555_normalize_whitespace_returns_default() {
    assert_eq!(
        normalize_agent_adapter_type(Some("   ")),
        DEFAULT_AGENT_ADAPTER_TYPE
    );
}

#[test]
fn r555_normalize_trims_input() {
    assert_eq!(
        normalize_agent_adapter_type(Some("  claude-local  ")),
        "claude_local"
    );
}

#[test]
fn r555_normalize_passes_through_non_empty() {
    // Hyphen input → underscore output (R564 normalization convention)
    assert_eq!(
        normalize_agent_adapter_type(Some("codex-local")),
        "codex_local"
    );
    // Underscore input → unchanged
    assert_eq!(normalize_agent_adapter_type(Some("process")), "process");
    assert_eq!(
        normalize_agent_adapter_type(Some("claude_local")),
        "claude_local"
    );
}

#[test]
fn r555_validate_accepts_non_empty() {
    assert_eq!(
        validate_agent_adapter_type("claude-local"),
        Some("claude-local".into())
    );
    assert_eq!(
        validate_agent_adapter_type("claude-local  "),
        Some("claude-local".into())
    );
}

#[test]
fn r555_validate_rejects_empty() {
    assert!(validate_agent_adapter_type("").is_none());
    assert!(validate_agent_adapter_type("   ").is_none());
    assert!(validate_agent_adapter_type("\t\n").is_none());
}

#[test]
fn r555_validate_optional_accepts_some_non_empty() {
    assert_eq!(
        validate_optional_agent_adapter_type(Some("claude-local")),
        Some("claude-local".into())
    );
}

#[test]
fn r555_validate_optional_accepts_none() {
    assert!(validate_optional_agent_adapter_type(None).is_none());
}

#[test]
fn r555_validate_optional_rejects_empty() {
    assert!(validate_optional_agent_adapter_type(Some("")).is_none());
    assert!(validate_optional_agent_adapter_type(Some("   ")).is_none());
}

#[test]
fn r555_builtin_recognized_set() {
    for t in [
        "process",
        "claude_local",
        "codex_local",
        "cursor_local",
        "cursor_cloud",
        "gemini_local",
        "grok_local",
        "hermes",
        "hermes_gateway",
        "openclaw_gateway",
        "opencode_local",
        "pi_local",
    ] {
        assert!(is_builtin_adapter_type(t), "missing builtin {t}");
    }
}

#[test]
fn r555_custom_adapter_not_builtin() {
    assert!(!is_builtin_adapter_type("my-custom-adapter"));
    assert!(!is_builtin_adapter_type(""));
    assert!(!is_builtin_adapter_type("process ")); // not trimmed
}
