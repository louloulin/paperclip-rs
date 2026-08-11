#![allow(clippy::doc_markdown)]
//! R544 — pc-agent-eligibility 综合测试集。
//!
//! 覆盖：
//! 1. AgentStatus 枚举 + from_db / as_str
//! 2. is_agent_status_assignable_to_work / is_agent_status_invokable 状态矩阵
//! 3. get_agent_org_chain_health — 健康链 / 终止祖先 / missing manager / cycle / 跨公司
//! 4. get_agent_work_eligibility — assignable / invokable 原因组合
//! 5. is_agent_assignable_to_work / is_agent_invokable convenience wrapper
//! 6. repair_guidance 内容（按 reason 不同）
//! 7. 强类型枚举 serde JSON 互操作

use pc_agent_eligibility::{
    get_agent_org_chain_health, get_agent_work_eligibility, is_agent_assignable_to_work,
    is_agent_invokable, is_agent_status_assignable_to_work, is_agent_status_invokable,
    AgentEligibilityAgent, AgentEligibilityLifecycleReason, AgentOrgChainHealthStatus,
    AgentOrgChainInvalidReason, AgentStatus, EligibilityInput,
};

// ============================================================================
// Helpers
// ============================================================================

fn agent(id: &str, status: AgentStatus, reports_to: Option<&str>) -> AgentEligibilityAgent {
    AgentEligibilityAgent {
        id: id.to_string(),
        company_id: "co-1".to_string(),
        name: format!("agent-{id}"),
        status,
        reports_to: reports_to.map(str::to_string),
    }
}

fn input<'a>(
    target: &'a AgentEligibilityAgent,
    roster: &'a [AgentEligibilityAgent],
) -> EligibilityInput<'a> {
    EligibilityInput {
        agent: target,
        agents: roster,
    }
}

fn active(id: &str) -> AgentEligibilityAgent {
    agent(id, AgentStatus::Active, None)
}

// ============================================================================
// AgentStatus enum / from_db / as_str
// ============================================================================

#[test]
fn r544_agent_status_from_db_round_trips_all_known_values() {
    for raw in [
        "active",
        "idle",
        "running",
        "paused",
        "error",
        "terminated",
        "pending_approval",
    ] {
        let parsed = AgentStatus::from_db(raw);
        assert_eq!(parsed.as_str(), raw, "round-trip {raw}");
    }
}

#[test]
fn r544_agent_status_from_db_captures_unknown_values() {
    let parsed = AgentStatus::from_db("future_status_v2");
    assert_eq!(parsed, AgentStatus::Other("future_status_v2".to_string()));
    assert_eq!(parsed.as_str(), "future_status_v2");
}

// ============================================================================
// Status predicates
// ============================================================================

#[test]
fn r544_is_agent_status_assignable_to_work_matrix() {
    assert!(is_agent_status_assignable_to_work(&AgentStatus::Active));
    assert!(is_agent_status_assignable_to_work(&AgentStatus::Idle));
    assert!(is_agent_status_assignable_to_work(&AgentStatus::Running));
    assert!(is_agent_status_assignable_to_work(&AgentStatus::Paused));
    assert!(is_agent_status_assignable_to_work(&AgentStatus::Error));
    assert!(!is_agent_status_assignable_to_work(
        &AgentStatus::Terminated
    ));
    assert!(!is_agent_status_assignable_to_work(
        &AgentStatus::PendingApproval
    ));
    // Unknown status → not assignable
    assert!(!is_agent_status_assignable_to_work(&AgentStatus::Other(
        "frozen".into()
    )));
}

#[test]
fn r544_is_agent_status_invokable_matrix() {
    assert!(is_agent_status_invokable(&AgentStatus::Active));
    assert!(is_agent_status_invokable(&AgentStatus::Idle));
    assert!(is_agent_status_invokable(&AgentStatus::Running));
    assert!(is_agent_status_invokable(&AgentStatus::Error));
    // Paused: not invokable (only non-invokable besides terminated / pending)
    assert!(!is_agent_status_invokable(&AgentStatus::Paused));
    assert!(!is_agent_status_invokable(&AgentStatus::Terminated));
    assert!(!is_agent_status_invokable(&AgentStatus::PendingApproval));
    assert!(!is_agent_status_invokable(&AgentStatus::Other("x".into())));
}

// ============================================================================
// get_agent_org_chain_health — healthy cases
// ============================================================================

#[test]
fn r544_org_chain_healthy_for_root_agent() {
    let a = active("a");
    let roster = vec![a.clone()];
    let h = get_agent_org_chain_health(&input(&a, &roster));
    assert_eq!(h.status, AgentOrgChainHealthStatus::Healthy);
    assert_eq!(h.reason, AgentOrgChainInvalidReason::Healthy);
    assert!(h.first_invalid_ancestor.is_none());
    assert!(h.invalid_ancestors.is_empty());
    assert_eq!(h.full_chain.len(), 1);
    assert_eq!(h.full_chain[0].depth, 0);
    assert!(h.repair_guidance.is_none());
}

#[test]
fn r544_org_chain_healthy_for_three_level_active_chain() {
    let root = active("root");
    let mid = agent("mid", AgentStatus::Active, Some("root"));
    let leaf = agent("leaf", AgentStatus::Active, Some("mid"));
    let roster = vec![root.clone(), mid.clone(), leaf.clone()];
    let h = get_agent_org_chain_health(&input(&leaf, &roster));
    assert_eq!(h.status, AgentOrgChainHealthStatus::Healthy);
    assert_eq!(h.full_chain.len(), 3);
    assert_eq!(h.full_chain[0].id, "leaf");
    assert_eq!(h.full_chain[1].id, "mid");
    assert_eq!(h.full_chain[2].id, "root");
    assert_eq!(h.full_chain[2].depth, 2);
    assert!(h.repair_guidance.is_none());
}

// ============================================================================
// get_agent_org_chain_health — invalid cases
// ============================================================================

#[test]
fn r544_org_chain_flags_terminated_ancestor() {
    let root = agent("root", AgentStatus::Terminated, None);
    let mid = agent("mid", AgentStatus::Active, Some("root"));
    let leaf = agent("leaf", AgentStatus::Active, Some("mid"));
    let roster = vec![root.clone(), mid.clone(), leaf.clone()];
    let h = get_agent_org_chain_health(&input(&leaf, &roster));
    assert_eq!(h.status, AgentOrgChainHealthStatus::InvalidOrgChain);
    assert_eq!(h.reason, AgentOrgChainInvalidReason::TerminatedAncestor);
    assert_eq!(h.first_invalid_ancestor.as_ref().unwrap().id, "root");
    assert_eq!(
        h.first_invalid_ancestor.as_ref().unwrap().status,
        AgentStatus::Terminated
    );
    let repair = h.repair_guidance.as_deref().unwrap();
    assert!(repair.contains("reports through terminated ancestor"));
    assert!(repair.contains("agent-leaf"));
}

#[test]
fn r544_org_chain_flags_missing_manager() {
    let leaf = agent("leaf", AgentStatus::Active, Some("ghost"));
    let roster = vec![leaf.clone()];
    let h = get_agent_org_chain_health(&input(&leaf, &roster));
    assert_eq!(h.status, AgentOrgChainHealthStatus::InvalidOrgChain);
    assert_eq!(h.reason, AgentOrgChainInvalidReason::MissingManager);
    let invalid = h.first_invalid_ancestor.as_ref().unwrap();
    assert_eq!(invalid.id, "ghost");
    assert_eq!(invalid.status, AgentStatus::Other("missing".into()));
    assert!(h
        .repair_guidance
        .as_deref()
        .unwrap()
        .contains("missing manager ghost"));
}

#[test]
fn r544_org_chain_flags_cross_company_manager_as_missing() {
    let mut root_other_company = active("root");
    root_other_company.company_id = "co-2".to_string();
    let leaf = agent("leaf", AgentStatus::Active, Some("root"));
    let roster = vec![root_other_company, leaf.clone()];
    let h = get_agent_org_chain_health(&input(&leaf, &roster));
    assert_eq!(h.reason, AgentOrgChainInvalidReason::MissingManager);
}

#[test]
fn r544_org_chain_detects_two_node_cycle() {
    let a = agent("a", AgentStatus::Active, Some("b"));
    let b = agent("b", AgentStatus::Active, Some("a"));
    let roster = vec![a.clone(), b.clone()];
    let h = get_agent_org_chain_health(&input(&a, &roster));
    assert_eq!(h.status, AgentOrgChainHealthStatus::InvalidOrgChain);
    assert_eq!(h.reason, AgentOrgChainInvalidReason::Cycle);
    let invalid = h.first_invalid_ancestor.as_ref().unwrap();
    // The cycle target is the parent we tried to follow (a), since a was
    // already in the seen set when we got to b.reports_to == "a".
    assert_eq!(invalid.id, "a");
    assert_eq!(invalid.status, AgentStatus::Other("cycle".into()));
    let repair = h.repair_guidance.as_deref().unwrap();
    assert!(repair.contains("cycle"));
    assert!(repair.contains("agent-a"));
}

#[test]
fn r544_org_chain_terminates_on_self_referential_cycle() {
    // agent that reports to itself
    let a = agent("a", AgentStatus::Active, Some("a"));
    let roster = vec![a.clone()];
    let h = get_agent_org_chain_health(&input(&a, &roster));
    assert_eq!(h.reason, AgentOrgChainInvalidReason::Cycle);
    // The chain should record the agent once as self + once as the cycle
    // detection ancestor.
    assert!(h.full_chain.len() >= 2);
}

#[test]
fn r544_org_chain_collects_multiple_invalid_ancestors() {
    let root = agent("root", AgentStatus::Terminated, None);
    let mid = agent("mid", AgentStatus::Terminated, Some("root"));
    let leaf = agent("leaf", AgentStatus::Active, Some("mid"));
    let roster = vec![root.clone(), mid.clone(), leaf.clone()];
    let h = get_agent_org_chain_health(&input(&leaf, &roster));
    // First invalid ancestor is the closest terminated one (mid).
    assert_eq!(h.reason, AgentOrgChainInvalidReason::TerminatedAncestor);
    assert_eq!(h.first_invalid_ancestor.as_ref().unwrap().id, "mid");
    // But the full list records every terminated ancestor walked past.
    assert_eq!(h.invalid_ancestors.len(), 2);
    let ids: Vec<_> = h.invalid_ancestors.iter().map(|a| a.id.clone()).collect();
    assert!(ids.contains(&"mid".to_string()));
    assert!(ids.contains(&"root".to_string()));
}

// ============================================================================
// get_agent_work_eligibility — full verdict
// ============================================================================

#[test]
fn r544_work_eligibility_eligible_active_with_healthy_chain() {
    let root = active("root");
    let leaf = agent("leaf", AgentStatus::Active, Some("root"));
    let roster = vec![root.clone(), leaf.clone()];
    let e = get_agent_work_eligibility(&input(&leaf, &roster));
    assert!(e.assignable);
    assert!(e.invokable);
    assert_eq!(
        e.assignability_reason,
        AgentEligibilityLifecycleReason::Eligible
    );
    assert_eq!(
        e.invokability_reason,
        AgentEligibilityLifecycleReason::Eligible
    );
}

#[test]
fn r544_work_eligibility_terminated_short_circuits() {
    let terminated = agent("a", AgentStatus::Terminated, None);
    let roster = vec![terminated.clone()];
    let e = get_agent_work_eligibility(&input(&terminated, &roster));
    assert!(!e.assignable);
    assert!(!e.invokable);
    assert_eq!(
        e.assignability_reason,
        AgentEligibilityLifecycleReason::Terminated
    );
    assert_eq!(
        e.invokability_reason,
        AgentEligibilityLifecycleReason::Terminated
    );
}

#[test]
fn r544_work_eligibility_pending_approval_short_circuits() {
    let p = agent("a", AgentStatus::PendingApproval, None);
    let roster = vec![p.clone()];
    let e = get_agent_work_eligibility(&input(&p, &roster));
    assert!(!e.assignable);
    assert!(!e.invokable);
    assert_eq!(
        e.assignability_reason,
        AgentEligibilityLifecycleReason::PendingApproval
    );
    assert_eq!(
        e.invokability_reason,
        AgentEligibilityLifecycleReason::PendingApproval
    );
}

#[test]
fn r544_work_eligibility_paused_is_assignable_but_not_invokable() {
    // Paused status: in ASSIGNABLE_STATUSES, not in INVOKABLE_STATUSES.
    let p = agent("a", AgentStatus::Paused, None);
    let roster = vec![p.clone()];
    let e = get_agent_work_eligibility(&input(&p, &roster));
    assert!(e.assignable);
    assert!(!e.invokable);
    assert_eq!(
        e.invokability_reason,
        AgentEligibilityLifecycleReason::Paused
    );
}

#[test]
fn r544_work_eligibility_unknown_status_returns_unknown() {
    let u = agent("a", AgentStatus::Other("mystery".into()), None);
    let roster = vec![u.clone()];
    let e = get_agent_work_eligibility(&input(&u, &roster));
    assert!(!e.assignable);
    assert!(!e.invokable);
    assert_eq!(
        e.assignability_reason,
        AgentEligibilityLifecycleReason::UnknownStatus
    );
    assert_eq!(
        e.invokability_reason,
        AgentEligibilityLifecycleReason::UnknownStatus
    );
}

#[test]
fn r544_work_eligibility_invalid_chain_overrides_status() {
    // Active + healthy-looking status but the chain is broken → invalid.
    let leaf = agent("leaf", AgentStatus::Active, Some("ghost"));
    let roster = vec![leaf.clone()];
    let e = get_agent_work_eligibility(&input(&leaf, &roster));
    assert!(!e.assignable);
    assert!(!e.invokable);
    assert_eq!(
        e.assignability_reason,
        AgentEligibilityLifecycleReason::InvalidOrgChain
    );
    assert_eq!(
        e.invokability_reason,
        AgentEligibilityLifecycleReason::InvalidOrgChain
    );
}

#[test]
fn r544_work_eligibility_terminated_overrides_invalid_chain() {
    // Both status and chain are bad → status wins (status short-circuits).
    let leaf = agent("leaf", AgentStatus::Terminated, Some("ghost"));
    let roster = vec![leaf.clone()];
    let e = get_agent_work_eligibility(&input(&leaf, &roster));
    assert_eq!(
        e.assignability_reason,
        AgentEligibilityLifecycleReason::Terminated
    );
}

// ============================================================================
// Convenience wrappers
// ============================================================================

#[test]
fn r544_is_agent_assignable_and_invokable_match_full_verdict() {
    let root = active("root");
    let leaf = agent("leaf", AgentStatus::Active, Some("root"));
    let roster = vec![root, leaf.clone()];
    assert!(is_agent_assignable_to_work(&input(&leaf, &roster)));
    assert!(is_agent_invokable(&input(&leaf, &roster)));

    let bad = agent("bad", AgentStatus::Terminated, None);
    let roster2 = vec![bad.clone()];
    assert!(!is_agent_assignable_to_work(&input(&bad, &roster2)));
    assert!(!is_agent_invokable(&input(&bad, &roster2)));
}

// ============================================================================
// Serde round-trips
// ============================================================================

#[test]
fn r544_agent_org_chain_health_serializes_to_camel_case_json() {
    let root = active("root");
    let leaf = agent("leaf", AgentStatus::Active, Some("root"));
    let roster = vec![root, leaf.clone()];
    let h = get_agent_org_chain_health(&input(&leaf, &roster));
    let json = serde_json::to_string(&h).unwrap();
    // full_chain camelCase
    assert!(json.contains("\"fullChain\""));
    // reason camelCase
    assert!(json.contains("\"reason\":\"healthy\""));
    // status camelCase
    assert!(json.contains("\"status\":\"healthy\""));
}

#[test]
fn r544_agent_work_eligibility_round_trips_via_json() {
    let root = active("root");
    let leaf = agent("leaf", AgentStatus::Active, Some("root"));
    let roster = vec![root, leaf.clone()];
    let e = get_agent_work_eligibility(&input(&leaf, &roster));
    let json = serde_json::to_string(&e).unwrap();
    let parsed: pc_agent_eligibility::AgentWorkEligibility = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.assignable, e.assignable);
    assert_eq!(parsed.invokable, e.invokable);
    assert_eq!(parsed.assignability_reason, e.assignability_reason);
}
