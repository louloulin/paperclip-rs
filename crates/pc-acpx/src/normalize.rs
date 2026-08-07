//! `pc-acpx` normalize helpers — pure functions that mirror
//! `normalizeAgent`, `normalizeMode`, `normalizePermissionMode`,
//! `normalizeNonInteractivePermissions`, and `normalizeRequestedThinkingEffort`
//! from Node `acpx-engine/execute.ts`.

use crate::constants::{
    DEFAULT_ACP_ENGINE_AGENT, DEFAULT_ACP_ENGINE_MODE,
    DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS, DEFAULT_ACP_ENGINE_PERMISSION_MODE,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Agent
// ============================================================================

/// Normalize the `agent` field of an engine config. Defaults to
/// `DEFAULT_ACP_ENGINE_AGENT` when missing or empty.
pub fn normalize_agent(config: &Value) -> String {
    let agent = as_string(config.get("agent"), DEFAULT_ACP_ENGINE_AGENT)
        .trim()
        .to_string();
    if agent.is_empty() {
        DEFAULT_ACP_ENGINE_AGENT.to_string()
    } else {
        agent
    }
}

// ============================================================================
// Mode
// ============================================================================

/// Normalize the session mode. Anything other than `"oneshot"` falls back to
/// the persistent default.
pub fn normalize_mode(config: &Value) -> NormalizedMode {
    match as_string(config.get("mode"), DEFAULT_ACP_ENGINE_MODE).as_str() {
        "oneshot" => NormalizedMode::OneShot,
        _ => NormalizedMode::Persistent,
    }
}

/// Normalized session mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalizedMode {
    Persistent,
    OneShot,
}

impl NormalizedMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            NormalizedMode::Persistent => "persistent",
            NormalizedMode::OneShot => "oneshot",
        }
    }
}

// ============================================================================
// Permission mode
// ============================================================================

/// Normalize the permission mode. Unknown values fall back to the default
/// (`approve-all`).
pub fn normalize_permission_mode(config: &Value) -> NormalizedPermissionMode {
    match as_string(
        config.get("permissionMode"),
        DEFAULT_ACP_ENGINE_PERMISSION_MODE,
    )
    .as_str()
    {
        "approve-reads" => NormalizedPermissionMode::ApproveReads,
        "deny-all" => NormalizedPermissionMode::DenyAll,
        _ => NormalizedPermissionMode::ApproveAll,
    }
}

/// Normalized permission mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalizedPermissionMode {
    ApproveAll,
    ApproveReads,
    DenyAll,
}

impl NormalizedPermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            NormalizedPermissionMode::ApproveAll => "approve-all",
            NormalizedPermissionMode::ApproveReads => "approve-reads",
            NormalizedPermissionMode::DenyAll => "deny-all",
        }
    }
}

// ============================================================================
// Non-interactive permissions
// ============================================================================

/// Normalize the non-interactive permission policy. Anything other than
/// `"fail"` falls back to `"deny"`.
pub fn normalize_non_interactive_permissions(
    config: &Value,
) -> NormalizedNonInteractivePermissions {
    match as_string(
        config.get("nonInteractivePermissions"),
        DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS,
    )
    .as_str()
    {
        "fail" => NormalizedNonInteractivePermissions::Fail,
        _ => NormalizedNonInteractivePermissions::Deny,
    }
}

/// Normalized non-interactive permission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalizedNonInteractivePermissions {
    Deny,
    Fail,
}

impl NormalizedNonInteractivePermissions {
    pub fn as_str(&self) -> &'static str {
        match self {
            NormalizedNonInteractivePermissions::Deny => "deny",
            NormalizedNonInteractivePermissions::Fail => "fail",
        }
    }
}

// ============================================================================
// Thinking effort
// ============================================================================

/// Normalize the requested thinking effort. The Node implementation is a
/// free-form string passthrough (the runtime validates the value), so we
/// preserve the value verbatim and trim it. Empty strings collapse to `None`.
pub fn normalize_requested_thinking_effort(config: &Value) -> Option<String> {
    let value = as_string(config.get("thinkingEffort"), "")
        .trim()
        .to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn as_string(value: Option<&Value>, fallback: &str) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        _ => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_agent_defaults_to_claude() {
        assert_eq!(normalize_agent(&serde_json::json!({})), "claude");
        assert_eq!(
            normalize_agent(&serde_json::json!({ "agent": "" })),
            "claude"
        );
        assert_eq!(
            normalize_agent(&serde_json::json!({ "agent": "codex" })),
            "codex"
        );
    }

    #[test]
    fn normalize_mode_recognizes_oneshot() {
        assert_eq!(
            normalize_mode(&serde_json::json!({ "mode": "oneshot" })),
            NormalizedMode::OneShot
        );
        assert_eq!(
            normalize_mode(&serde_json::json!({ "mode": "persistent" })),
            NormalizedMode::Persistent
        );
        assert_eq!(
            normalize_mode(&serde_json::json!({})),
            NormalizedMode::Persistent
        );
    }

    #[test]
    fn normalize_permission_mode_defaults_to_approve_all() {
        assert_eq!(
            normalize_permission_mode(&serde_json::json!({})),
            NormalizedPermissionMode::ApproveAll
        );
        assert_eq!(
            normalize_permission_mode(&serde_json::json!({ "permissionMode": "approve-reads" })),
            NormalizedPermissionMode::ApproveReads
        );
        assert_eq!(
            normalize_permission_mode(&serde_json::json!({ "permissionMode": "deny-all" })),
            NormalizedPermissionMode::DenyAll
        );
        assert_eq!(
            normalize_permission_mode(&serde_json::json!({ "permissionMode": "weird" })),
            NormalizedPermissionMode::ApproveAll
        );
    }

    #[test]
    fn normalize_non_interactive_permissions_defaults_to_deny() {
        assert_eq!(
            normalize_non_interactive_permissions(&serde_json::json!({})),
            NormalizedNonInteractivePermissions::Deny
        );
        assert_eq!(
            normalize_non_interactive_permissions(&serde_json::json!({
                "nonInteractivePermissions": "fail"
            })),
            NormalizedNonInteractivePermissions::Fail
        );
    }

    #[test]
    fn normalize_requested_thinking_effort_passes_through() {
        assert_eq!(
            normalize_requested_thinking_effort(&serde_json::json!({})),
            None
        );
        assert_eq!(
            normalize_requested_thinking_effort(&serde_json::json!({
                "thinkingEffort": "  high  "
            })),
            Some("high".to_string())
        );
        assert_eq!(
            normalize_requested_thinking_effort(&serde_json::json!({
                "thinkingEffort": "low"
            })),
            Some("low".to_string())
        );
    }
}
