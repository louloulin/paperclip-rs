use pc_agent::{
    contains_redacted_marker, sanitize_snapshot_value, AgentConfigSnapshot, REDACTED_VALUE,
};
use serde_json::json;

fn snapshot() -> AgentConfigSnapshot {
    AgentConfigSnapshot {
        name: "Researcher".into(),
        role: "general".into(),
        title: None,
        icon: None,
        reports_to: None,
        capabilities: None,
        adapter_type: "codex_local".into(),
        adapter_config: json!({"model": "gpt-5", "apiKey": "secret"}),
        runtime_config: json!({}),
        default_environment_id: None,
        budget_monthly_cents: 0,
        metadata: None,
    }
}

#[test]
fn changed_keys_follow_node_contract_order() {
    let before = snapshot();
    let mut after = before.clone();
    after.name = "Senior Researcher".into();
    after.adapter_type = "claude_local".into();
    after.budget_monthly_cents = 1_000;

    assert_eq!(
        before.changed_keys(&after),
        ["name", "adapterType", "budgetMonthlyCents"]
    );
}

#[test]
fn snapshot_sanitization_redacts_nested_secrets_but_preserves_secret_refs() {
    let value = json!({
        "apiKey": "sk-live",
        "nested": {"access_token": "token-value"},
        "binding": {"type": "secret_ref", "secretId": "00000000-0000-0000-0000-000000000001"},
        "plain": {"type": "plain", "value": "credential-value"}
    });

    assert_eq!(
        sanitize_snapshot_value(&value),
        json!({
            "apiKey": REDACTED_VALUE,
            "nested": {"access_token": REDACTED_VALUE},
            "binding": {"type": "secret_ref", "secretId": "00000000-0000-0000-0000-000000000001"},
            "plain": {"type": "plain", "value": REDACTED_VALUE}
        })
    );
}

#[test]
fn rollback_guard_detects_redacted_values_at_any_depth() {
    assert!(contains_redacted_marker(&json!({
        "adapterConfig": {"token": REDACTED_VALUE}
    })));
    assert!(!contains_redacted_marker(&json!({
        "adapterConfig": {"tokenRef": {"type": "secret_ref", "secretId": "secret-id"}}
    })));
}

#[test]
fn serialized_snapshot_uses_frontend_camel_case_contract() {
    let value = serde_json::to_value(snapshot()).unwrap();
    assert_eq!(value["adapterType"], "codex_local");
    assert_eq!(value["budgetMonthlyCents"], 0);
    assert!(value.get("adapter_type").is_none());
}
