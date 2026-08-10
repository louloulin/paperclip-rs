//! E2E tests for `pc-agent-permissions`.
//!
//! 与 Node `server/src/__tests__/agent-permissions-service.test.ts` 1:1 对齐。

use pc_agent_permissions::{default_permissions_for_role, normalize_agent_permissions};
use serde_json::json;

// ============================================================================
// Role defaults
// ============================================================================

#[test]
fn r669_keeps_agent_creation_authority_least_privileged_by_default() {
    assert!(default_permissions_for_role("ceo").can_create_agents);
    assert!(!default_permissions_for_role("CTO").can_create_agents);
    assert!(!default_permissions_for_role("engineering-manager").can_create_agents);
    assert!(!default_permissions_for_role("engineer").can_create_agents);
}

#[test]
fn r669_enables_skill_creation_for_every_role_by_default() {
    assert!(default_permissions_for_role("ceo").can_create_skills);
    assert!(default_permissions_for_role("CTO").can_create_skills);
    assert!(default_permissions_for_role("engineering-manager").can_create_skills);
    assert!(default_permissions_for_role("engineer").can_create_skills);
}

// ============================================================================
// Override semantics
// ============================================================================

#[test]
fn r669_preserves_explicit_can_create_agents_overrides() {
    assert!(!normalize_agent_permissions(Some(&json!({ "canCreateAgents": false })), "cto").can_create_agents);
    assert!(normalize_agent_permissions(Some(&json!({ "canCreateAgents": true })), "engineer").can_create_agents);
}

#[test]
fn r669_defaults_missing_skill_creation_to_true_and_preserves_explicit_false() {
    assert!(normalize_agent_permissions(Some(&json!({})), "engineer").can_create_skills);
    assert!(!normalize_agent_permissions(Some(&json!({ "canCreateSkills": false })), "ceo").can_create_skills);
    assert!(normalize_agent_permissions(Some(&json!({ "canCreateSkills": true })), "engineer").can_create_skills);
}

// ============================================================================
// Edge cases (robustness beyond Node tests)
// ============================================================================

#[test]
fn r669_returns_defaults_for_none_input() {
    let p = normalize_agent_permissions(None, "engineer");
    let defaults = default_permissions_for_role("engineer");
    assert_eq!(p.can_create_agents, defaults.can_create_agents);
    assert_eq!(p.can_create_skills, defaults.can_create_skills);
}

#[test]
fn r669_returns_defaults_for_non_object_input() {
    let p = normalize_agent_permissions(Some(&json!("string")), "engineer");
    assert!(!p.can_create_agents);
    assert!(p.can_create_skills);

    let p = normalize_agent_permissions(Some(&json!(42)), "engineer");
    assert!(!p.can_create_agents);
    assert!(p.can_create_skills);

    let p = normalize_agent_permissions(Some(&json!([1, 2, 3])), "engineer");
    assert!(!p.can_create_agents);
    assert!(p.can_create_skills);

    let p = normalize_agent_permissions(Some(&json!(null)), "engineer");
    assert!(!p.can_create_agents);
    assert!(p.can_create_skills);
}

#[test]
fn r669_preserves_arbitrary_extra_fields() {
    let p = normalize_agent_permissions(
        Some(&json!({
            "canCreateAgents": false,
            "canAssignTasks": false,
            "canDeploy": true,
        })),
        "engineer",
    );
    assert_eq!(p.extras.get("canAssignTasks"), Some(&json!(false)));
    assert_eq!(p.extras.get("canDeploy"), Some(&json!(true)));
    assert_eq!(p.extras.len(), 2);
    // canCreateAgents/canCreateSkills 不在 extras
    assert!(p.extras.get("canCreateAgents").is_none());
    assert!(p.extras.get("canCreateSkills").is_none());
}

#[test]
fn r669_case_insensitive_ceo_role_match() {
    assert!(default_permissions_for_role("ceo").can_create_agents);
    assert!(default_permissions_for_role("CEO").can_create_agents);
    assert!(default_permissions_for_role("Ceo").can_create_agents);
    assert!(default_permissions_for_role("  ceo  ").can_create_agents);
    // 注意：trim 后再 eq_ignore_ascii_case("ceo")，所以 "  ceo  " 为 true
    assert!(default_permissions_for_role("  ceo  ").can_create_agents);
}

#[test]
fn r669_ignores_non_boolean_can_create_agents_value() {
    let p = normalize_agent_permissions(
        Some(&json!({ "canCreateAgents": "yes" })),
        "engineer",
    );
    // engineer 默认 false
    assert!(!p.can_create_agents);

    let p = normalize_agent_permissions(
        Some(&json!({ "canCreateAgents": 1 })),
        "ceo",
    );
    // ceo 默认 true
    assert!(p.can_create_agents);
}

#[test]
fn r669_ignores_non_boolean_can_create_skills_value() {
    let p = normalize_agent_permissions(
        Some(&json!({ "canCreateSkills": "true" })),
        "engineer",
    );
    // 默认 true（即使显式给了非 boolean 也走默认）
    assert!(p.can_create_skills);
}
