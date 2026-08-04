use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::keys::legacy_target_keys;
use super::rules::{
    expand_target_keys, normalize_target_keys, payload_has_displayed_diff, result_consumed,
    row_is_eligible,
};
use super::*;

#[test]
fn target_key_builders_match_node_contract() {
    let id = Uuid::nil();
    assert_eq!(agent_instructions_change_target_key(id), format!("agent:{id}:instructions"));
    assert_eq!(agent_profile_change_target_key(id), format!("agent:{id}:profile"));
    assert_eq!(skill_change_target_key(id), format!("skill:{id}"));
    assert_eq!(skills_scan_projects_change_target_key(), "skills:scan-projects");
}

#[test]
fn profile_patch_detects_only_protected_fields() {
    let mut patch = Map::new();
    patch.insert("adapterConfig".into(), Value::Null);
    assert!(!touches_agent_profile_change_consent_fields(&patch));
    patch.insert("capabilities".into(), json!([]));
    assert!(touches_agent_profile_change_consent_fields(&patch));
}

#[test]
fn target_keys_are_trimmed_deduplicated_and_legacy_expanded() {
    let keys = normalize_target_keys(&[" skill:abc ".into(), "skill:abc".into(), "".into()]);
    assert_eq!(keys, vec!["skill:abc"]);
    let expanded = expand_target_keys(&keys);
    assert!(expanded.contains(&"skill:abc".into()));
    assert!(expanded.contains(&"reflection-coach:company-skill:abc".into()));
    assert_eq!(legacy_target_keys("skill-import:catalog").len(), 2);
}

#[test]
fn displayed_diff_accepts_fence_or_line_prefix() {
    assert!(payload_has_displayed_diff(&json!({"detailsMarkdown":"```diff\n+x\n```"})));
    assert!(payload_has_displayed_diff(&json!({"detailsMarkdown":"Proposal\n-old\n+new"})));
    assert!(!payload_has_displayed_diff(&json!({"detailsMarkdown":"No visible diff"})));
    assert!(!payload_has_displayed_diff(&json!({"detailsMarkdown":"```diffuse\ntext"})));
}

#[test]
fn consumed_result_is_not_reusable() {
    assert!(!result_consumed(&json!({"outcome":"accepted"})));
    assert!(result_consumed(&json!({"outcome":"accepted","consumedAt":"now"})));
}

#[test]
fn eligibility_requires_previous_run_custom_target_diff_and_acceptance() {
    let source = Uuid::from_u128(1);
    let actor = Uuid::from_u128(2);
    let payload = json!({
        "detailsMarkdown":"```diff\n+x\n```",
        "target":{"type":"custom","key":"skill:abc"}
    });
    assert!(row_is_eligible(
        Some(source),
        &payload,
        &json!({"outcome":"accepted"}),
        actor,
        &["skill:abc".into()],
    ));
    assert!(!row_is_eligible(
        Some(actor),
        &payload,
        &json!({"outcome":"accepted"}),
        actor,
        &["skill:abc".into()],
    ));
}
