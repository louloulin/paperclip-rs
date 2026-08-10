//! End-to-end tests for `pc-issue-liveness`.
//!
//! 包含：
//! - 纯函数分类器测试（incident_key + classifier + service helpers）— 不依赖 DB
//! - 真实 DB 集成测试（create 公司 / agents / issues / blocker relations 后跑分类）

use pc_core::Timestamp;
use pc_issue_liveness::{
    build_incident_key, build_issue_graph_liveness_incident_key, classify,
    classify_issue_graph_liveness, dedup_by_incident_key, filter_by_company, filter_by_issue,
    filter_by_state, make_issue_input, parse_issue_graph_liveness_incident_key, summarize,
    IncidentKeyInput, IssueGraphLivenessInput, IssueLivenessAgentInput, IssueLivenessError,
    IssueLivenessExecutionPathInput, IssueLivenessFinding, IssueLivenessIssueInput,
    IssueLivenessOwnerCandidate, IssueLivenessOwnerCandidateReason, IssueLivenessRelationInput,
    IssueLivenessResult, IssueLivenessSeverity, IssueLivenessState, IssueLivenessSummary,
    IssueLivenessWaitingPathInput,
};
use serde_json::json;
use uuid::Uuid;

// ============================================================================
// Incident key 单元测试（无 DB）
// ============================================================================

#[test]
fn r656_incident_key_build_format() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
        company_id,
        issue_id,
        state: "blocked_by_unassigned_issue",
        blocker_issue_id: Some(blocker_id),
        participant_agent_id: None,
    });
    assert!(key.starts_with("harness_liveness:"));
    assert!(key.contains(&company_id.to_string()));
    assert!(key.contains(&issue_id.to_string()));
    assert!(key.contains("blocked_by_unassigned_issue"));
    assert!(key.ends_with(&blocker_id.to_string()));
}

#[test]
fn r656_incident_key_participant_fallback() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
        company_id,
        issue_id,
        state: "invalid_review_participant",
        blocker_issue_id: None,
        participant_agent_id: Some(agent_id),
    });
    assert!(key.ends_with(&agent_id.to_string()));
}

#[test]
fn r656_incident_key_none_fallback() {
    let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        state: "in_review_without_action_path",
        blocker_issue_id: None,
        participant_agent_id: None,
    });
    assert!(key.ends_with(":none"));
}

#[test]
fn r656_incident_key_parse_round_trip() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
        company_id,
        issue_id,
        state: "blocked_by_cancelled_issue",
        blocker_issue_id: Some(blocker_id),
        participant_agent_id: None,
    });
    let parsed = parse_issue_graph_liveness_incident_key(&key).expect("parse");
    assert_eq!(parsed.company_id, company_id);
    assert_eq!(parsed.issue_id, issue_id);
    assert_eq!(parsed.state, "blocked_by_cancelled_issue");
    assert_eq!(parsed.leaf_issue_id, Some(blocker_id));
}

#[test]
fn r656_incident_key_parse_rejects_garbage() {
    assert!(parse_issue_graph_liveness_incident_key("not:a:key").is_none());
    assert!(parse_issue_graph_liveness_incident_key("harness_liveness:not_uuid:foo:bar:baz").is_none());
    assert!(parse_issue_graph_liveness_incident_key("").is_none());
    assert!(parse_issue_graph_liveness_incident_key("wrong_prefix:a:b:c:d").is_none());
}

#[test]
fn r656_incident_key_parse_none_leaf() {
    let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        state: "in_review_without_action_path",
        blocker_issue_id: None,
        participant_agent_id: None,
    });
    let parsed = parse_issue_graph_liveness_incident_key(&key).expect("parse");
    assert_eq!(parsed.leaf_issue_id, None);
}

#[test]
fn r656_service_build_incident_key_re_export() {
    let cid = Uuid::new_v4();
    let iid = Uuid::new_v4();
    let bid = Uuid::new_v4();
    let key = build_incident_key(IncidentKeyInput {
        company_id: cid,
        issue_id: iid,
        state: "blocked_by_assigned_backlog_issue",
        blocker_issue_id: Some(bid),
        participant_agent_id: None,
    });
    assert!(key.contains("blocked_by_assigned_backlog_issue"));
}

// ============================================================================
// make_issue_input / types helpers
// ============================================================================

#[test]
fn r656_make_issue_input_basic() {
    let cid = Uuid::new_v4();
    let iid = Uuid::new_v4();
    let issue = make_issue_input(iid, cid, Some("TEST-1".to_string()), "Test Issue", "todo");
    assert_eq!(issue.id, iid);
    assert_eq!(issue.company_id, cid);
    assert_eq!(issue.identifier.as_deref(), Some("TEST-1"));
    assert_eq!(issue.title, "Test Issue");
    assert_eq!(issue.status, "todo");
    assert!(issue.assignee_agent_id.is_none());
    assert!(issue.assignee_user_id.is_none());
    assert!(issue.parent_id.is_none());
    assert!(issue.execution_policy.is_none());
    assert!(issue.monitor_next_check_at.is_none());
}

// ============================================================================
// Classifier 纯函数测试（无 DB）
// ============================================================================

fn make_agent(id: Uuid, company_id: Uuid, status: &str, reports_to: Option<Uuid>) -> IssueLivenessAgentInput {
    IssueLivenessAgentInput {
        id,
        company_id,
        name: format!("agent-{id}"),
        role: "engineer".to_string(),
        title: None,
        status: status.to_string(),
        reports_to,
    }
}

fn make_issue_with_assignee(
    id: Uuid,
    company_id: Uuid,
    identifier: &str,
    title: &str,
    status: &str,
    assignee_agent_id: Option<Uuid>,
    assignee_user_id: Option<&str>,
) -> IssueLivenessIssueInput {
    IssueLivenessIssueInput {
        id,
        company_id,
        identifier: Some(identifier.to_string()),
        title: title.to_string(),
        status: status.to_string(),
        project_id: None,
        goal_id: None,
        parent_id: None,
        assignee_agent_id,
        assignee_user_id: assignee_user_id.map(|s| s.to_string()),
        created_by_agent_id: None,
        created_by_user_id: None,
        execution_policy: None,
        execution_state: None,
        monitor_next_check_at: None,
        monitor_attempt_count: None,
    }
}

#[test]
fn r656_classifier_empty_input_returns_empty_findings() {
    let input = IssueGraphLivenessInput::default();
    let findings = classify_issue_graph_liveness(&input);
    assert!(findings.is_empty());
}

#[test]
fn r656_classifier_done_issue_never_generates_finding() {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent = make_agent(agent_id, company_id, "active", None);
    let issue = make_issue_with_assignee(
        Uuid::new_v4(),
        company_id,
        "AA-1",
        "Done Issue",
        "done",
        Some(agent_id),
        None,
    );
    let input = IssueGraphLivenessInput {
        issues: vec![issue],
        relations: vec![],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert!(findings.is_empty(), "done issues should not generate findings");
}

#[test]
fn r656_classifier_cancelled_issue_never_generates_finding() {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent = make_agent(agent_id, company_id, "active", None);
    let issue = make_issue_with_assignee(
        Uuid::new_v4(),
        company_id,
        "AA-2",
        "Cancelled Issue",
        "cancelled",
        Some(agent_id),
        None,
    );
    let input = IssueGraphLivenessInput {
        issues: vec![issue],
        relations: vec![],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert!(findings.is_empty(), "cancelled issues should not generate findings");
}

#[test]
fn r656_classifier_blocked_by_cancelled_issue() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let cancelled_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "AA-10",
        "Source Issue",
        "blocked",
        Some(agent_id),
        None,
    );
    let cancelled = make_issue_with_assignee(
        cancelled_id,
        company_id,
        "AA-11",
        "Cancelled Blocker",
        "cancelled",
        Some(agent_id),
        None,
    );
    let agent = make_agent(agent_id, company_id, "active", None);

    let relation = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: cancelled_id,
        blocked_issue_id: source_id,
    };

    let input = IssueGraphLivenessInput {
        issues: vec![source, cancelled],
        relations: vec![relation],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::BlockedByCancelledIssue);
    assert_eq!(f.issue_id, source_id);
    assert_eq!(f.recovery_issue_id, cancelled_id);
    assert!(f.reason.contains("cancelled"));
    assert!(f.recommended_owner_agent_id.is_some());
    assert!(f.recommended_owner_candidates
        .iter()
        .any(|c| c.reason == IssueLivenessOwnerCandidateReason::RootAgent));
}

#[test]
fn r656_classifier_blocked_by_unassigned_issue() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "BB-1",
        "Source",
        "blocked",
        Some(agent_id),
        None,
    );
    let blocker = make_issue_with_assignee(blocker_id, company_id, "BB-2", "Blocker", "todo", None, None);
    let agent = make_agent(agent_id, company_id, "active", None);

    let relation = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: blocker_id,
        blocked_issue_id: source_id,
    };

    let input = IssueGraphLivenessInput {
        issues: vec![source, blocker],
        relations: vec![relation],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::BlockedByUnassignedIssue);
    assert!(f.reason.contains("unassigned"));
}

#[test]
fn r656_classifier_blocked_by_assigned_backlog_issue() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let source_agent = Uuid::new_v4();
    let blocker_agent = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "CC-1",
        "Source",
        "blocked",
        Some(source_agent),
        None,
    );
    let blocker = make_issue_with_assignee(
        blocker_id,
        company_id,
        "CC-2",
        "Blocker",
        "backlog",
        Some(blocker_agent),
        None,
    );
    let agents = vec![
        make_agent(source_agent, company_id, "active", None),
        make_agent(blocker_agent, company_id, "active", None),
    ];

    let relation = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: blocker_id,
        blocked_issue_id: source_id,
    };

    let input = IssueGraphLivenessInput {
        issues: vec![source, blocker],
        relations: vec![relation],
        agents,
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::BlockedByAssignedBacklogIssue);
    assert!(f.reason.contains("backlog"));
}

#[test]
fn r656_classifier_blocked_by_terminated_assignee() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let source_agent = Uuid::new_v4();
    let blocker_agent = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "DD-1",
        "Source",
        "blocked",
        Some(source_agent),
        None,
    );
    let blocker = make_issue_with_assignee(
        blocker_id,
        company_id,
        "DD-2",
        "Blocker",
        "todo",
        Some(blocker_agent),
        None,
    );
    let agents = vec![
        make_agent(source_agent, company_id, "active", None),
        make_agent(blocker_agent, company_id, "terminated", None),
    ];

    let relation = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: blocker_id,
        blocked_issue_id: source_id,
    };

    let input = IssueGraphLivenessInput {
        issues: vec![source, blocker],
        relations: vec![relation],
        agents,
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::BlockedByUninvokableAssignee);
    assert!(f.recommended_owner_candidates
        .iter()
        .any(|c| c.reason == IssueLivenessOwnerCandidateReason::RootAgent));
}

#[test]
fn r656_classifier_blocked_by_invokable_assignee_no_finding() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let blocker_agent = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "EE-1",
        "Source",
        "blocked",
        Some(blocker_agent),
        None,
    );
    let blocker = make_issue_with_assignee(
        blocker_id,
        company_id,
        "EE-2",
        "Blocker",
        "todo",
        Some(blocker_agent),
        None,
    );
    let agent = make_agent(blocker_agent, company_id, "active", None);

    let relation = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: blocker_id,
        blocked_issue_id: source_id,
    };

    let input = IssueGraphLivenessInput {
        issues: vec![source, blocker],
        relations: vec![relation],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert!(findings.is_empty(), "invokable blocker should not generate finding");
}

#[test]
fn r656_classifier_nested_blocked_chain_propagates_to_leaf() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let mid_id = Uuid::new_v4();
    let leaf_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "FF-1",
        "Source",
        "blocked",
        Some(agent_id),
        None,
    );
    let mid = make_issue_with_assignee(
        mid_id,
        company_id,
        "FF-2",
        "Mid",
        "blocked",
        Some(agent_id),
        None,
    );
    let leaf = make_issue_with_assignee(
        leaf_id,
        company_id,
        "FF-3",
        "Leaf",
        "cancelled",
        Some(agent_id),
        None,
    );
    let agent = make_agent(agent_id, company_id, "active", None);

    let rel_mid_to_source = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: mid_id,
        blocked_issue_id: source_id,
    };
    let rel_leaf_to_mid = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: leaf_id,
        blocked_issue_id: mid_id,
    };

    let input = IssueGraphLivenessInput {
        issues: vec![source, mid, leaf],
        relations: vec![rel_mid_to_source, rel_leaf_to_mid],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::BlockedByCancelledIssue);
    assert_eq!(f.issue_id, source_id);
    assert_eq!(f.recovery_issue_id, leaf_id);
    assert_eq!(f.dependency_path.len(), 3, "path: source -> mid -> leaf");
}

#[test]
fn r656_classifier_in_review_without_action_path() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent = make_agent(agent_id, company_id, "active", None);
    let issue = make_issue_with_assignee(
        issue_id,
        company_id,
        "GG-1",
        "In Review",
        "in_review",
        Some(agent_id),
        None,
    );
    // No execution_state → falls through to in_review_without_action_path

    let input = IssueGraphLivenessInput {
        issues: vec![issue],
        relations: vec![],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::InReviewWithoutActionPath);
    assert!(f.reason.contains("in review"));
}

#[test]
fn r656_classifier_in_review_with_empty_execution_state_returns_invalid_participant() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent = make_agent(agent_id, company_id, "active", None);
    let mut issue = make_issue_with_assignee(
        issue_id,
        company_id,
        "GG-2",
        "In Review",
        "in_review",
        Some(agent_id),
        None,
    );
    // execution_state exists but has no currentParticipant → InvalidReviewParticipant
    issue.execution_state = Some(json!({}));

    let input = IssueGraphLivenessInput {
        issues: vec![issue],
        relations: vec![],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::InvalidReviewParticipant);
    assert!(f.reason.contains("cannot be resolved"));
}

#[test]
fn r656_classifier_in_review_with_active_run_no_finding() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent = make_agent(agent_id, company_id, "active", None);
    let issue = make_issue_with_assignee(
        issue_id,
        company_id,
        "HH-1",
        "In Review",
        "in_review",
        Some(agent_id),
        None,
    );
    let active_run = IssueLivenessExecutionPathInput {
        company_id,
        issue_id: Some(issue_id),
        agent_id: Some(agent_id),
        status: "running".to_string(),
    };
    let input = IssueGraphLivenessInput {
        issues: vec![issue],
        relations: vec![],
        agents: vec![agent],
        active_runs: Some(vec![active_run]),
        ..Default::default()
    };
    let findings = classify(&input);
    assert!(
        findings.is_empty(),
        "in_review with active run should not generate finding"
    );
}

#[test]
fn r656_classifier_in_review_with_pending_interaction_no_finding() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent = make_agent(agent_id, company_id, "active", None);
    let issue = make_issue_with_assignee(
        issue_id,
        company_id,
        "II-1",
        "In Review",
        "in_review",
        Some(agent_id),
        None,
    );
    let pending = IssueLivenessWaitingPathInput {
        company_id,
        issue_id,
        status: "pending".to_string(),
    };
    let input = IssueGraphLivenessInput {
        issues: vec![issue],
        relations: vec![],
        agents: vec![agent],
        pending_interactions: Some(vec![pending]),
        ..Default::default()
    };
    let findings = classify(&input);
    assert!(
        findings.is_empty(),
        "in_review with pending interaction should not generate finding"
    );
}

#[test]
fn r656_classifier_invalid_review_participant_terminated() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let participant_id = Uuid::new_v4();
    let agents = vec![
        make_agent(agent_id, company_id, "active", None),
        make_agent(participant_id, company_id, "terminated", None),
    ];
    let mut issue = make_issue_with_assignee(
        issue_id,
        company_id,
        "JJ-1",
        "In Review",
        "in_review",
        Some(agent_id),
        None,
    );
    issue.execution_state = Some(json!({
        "currentParticipant": {
            "type": "agent",
            "agentId": participant_id.to_string()
        }
    }));

    let input = IssueGraphLivenessInput {
        issues: vec![issue],
        relations: vec![],
        agents,
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::InvalidReviewParticipant);
    assert_eq!(f.participant_agent_id.unwrap_or(participant_id), participant_id);
}

#[test]
fn r656_classifier_owner_candidates_collects_chain() {
    let company_id = Uuid::new_v4();
    let leaf_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();

    // Manager -> Engineer (blocker, backlog, invokable) -> Source
    let manager_id = Uuid::new_v4();
    let engineer_id = Uuid::new_v4();
    let source_agent_id = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "KK-1",
        "Source",
        "blocked",
        Some(source_agent_id),
        None,
    );
    // Blocker is "backlog" + invokable engineer → blocked_by_assigned_backlog_issue
    // and engineer IS invokable so StalledBlockerAssignee gets added
    let blocker = make_issue_with_assignee(
        blocker_id,
        company_id,
        "KK-2",
        "Blocker",
        "backlog",
        Some(engineer_id),
        None,
    );
    let _ = leaf_id;

    // source_agent reports to manager -> only manager is root (no reports_to)
    let agents = vec![
        make_agent(source_agent_id, company_id, "active", Some(manager_id)),
        make_agent(engineer_id, company_id, "active", Some(manager_id)),
        make_agent(manager_id, company_id, "active", None),
    ];

    let relation = IssueLivenessRelationInput {
        company_id,
        blocker_issue_id: blocker_id,
        blocked_issue_id: source_id,
    };

    let input = IssueGraphLivenessInput {
        issues: vec![source, blocker],
        relations: vec![relation],
        agents,
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    eprintln!("DBG: candidate reasons: {:?}", f.recommended_owner_candidates.iter().map(|c| (c.agent_id, c.reason)).collect::<Vec<_>>());
    eprintln!("DBG: candidate ids: {:?}", f.recommended_owner_candidate_agent_ids);
    // Candidates should include engineer (stalled), manager (assignee_reporting_chain), manager (creator chain not applicable here), root_agent, fallback
    let reasons: Vec<_> = f
        .recommended_owner_candidates
        .iter()
        .map(|c| c.reason)
        .collect();
    assert!(reasons.contains(&IssueLivenessOwnerCandidateReason::StalledBlockerAssignee));
    assert!(reasons.contains(&IssueLivenessOwnerCandidateReason::AssigneeReportingChain));
    // manager is already added via AssigneeReportingChain so RootAgent is skipped (seen)
    // source_agent reports to manager and isn't yet in `seen`, so it's added as fallback
    assert!(reasons.contains(&IssueLivenessOwnerCandidateReason::OrderedInvokableFallback));
}

#[test]
fn r656_classifier_blocked_by_uninvokable_assignee_terminated_message() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let blocker_agent = Uuid::new_v4();
    let source_agent = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "LL-1",
        "Source",
        "blocked",
        Some(source_agent),
        None,
    );
    let blocker = make_issue_with_assignee(
        blocker_id,
        company_id,
        "LL-2",
        "Blocker",
        "todo",
        Some(blocker_agent),
        None,
    );
    let agents = vec![
        make_agent(source_agent, company_id, "active", None),
        make_agent(blocker_agent, company_id, "terminated", None),
    ];

    let input = IssueGraphLivenessInput {
        issues: vec![source, blocker],
        relations: vec![IssueLivenessRelationInput {
            company_id,
            blocker_issue_id: blocker_id,
            blocked_issue_id: source_id,
        }],
        agents,
        ..Default::default()
    };
    let findings = classify(&input);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.state, IssueLivenessState::BlockedByUninvokableAssignee);
    assert!(f.reason.contains("terminated"));
}

#[test]
fn r656_classifier_incident_key_format_in_finding() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let source_agent = Uuid::new_v4();

    let source = make_issue_with_assignee(
        source_id,
        company_id,
        "MM-1",
        "Source",
        "blocked",
        Some(source_agent),
        None,
    );
    let blocker = make_issue_with_assignee(
        blocker_id,
        company_id,
        "MM-2",
        "Blocker",
        "cancelled",
        Some(source_agent),
        None,
    );
    let agent = make_agent(source_agent, company_id, "active", None);

    let input = IssueGraphLivenessInput {
        issues: vec![source, blocker],
        relations: vec![IssueLivenessRelationInput {
            company_id,
            blocker_issue_id: blocker_id,
            blocked_issue_id: source_id,
        }],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    let f = &findings[0];
    let expected = format!(
        "harness_liveness:{}:{}:blocked_by_cancelled_issue:{}",
        company_id, source_id, blocker_id
    );
    assert_eq!(f.incident_key, expected);
}

#[test]
fn r656_classifier_dependency_path_includes_all_intermediates() {
    let company_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let mid_id = Uuid::new_v4();
    let leaf_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let source = make_issue_with_assignee(source_id, company_id, "NN-1", "Source", "blocked", Some(agent_id), None);
    let mid = make_issue_with_assignee(mid_id, company_id, "NN-2", "Mid", "blocked", Some(agent_id), None);
    let leaf = make_issue_with_assignee(leaf_id, company_id, "NN-3", "Leaf", "cancelled", Some(agent_id), None);
    let agent = make_agent(agent_id, company_id, "active", None);

    let input = IssueGraphLivenessInput {
        issues: vec![source, mid, leaf],
        relations: vec![
            IssueLivenessRelationInput {
                company_id,
                blocker_issue_id: mid_id,
                blocked_issue_id: source_id,
            },
            IssueLivenessRelationInput {
                company_id,
                blocker_issue_id: leaf_id,
                blocked_issue_id: mid_id,
            },
        ],
        agents: vec![agent],
        ..Default::default()
    };
    let findings = classify(&input);
    let f = &findings[0];
    assert_eq!(f.dependency_path.len(), 3);
    assert_eq!(f.dependency_path[0].issue_id, source_id);
    assert_eq!(f.dependency_path[1].issue_id, mid_id);
    assert_eq!(f.dependency_path[2].issue_id, leaf_id);
}

#[test]
fn r656_classifier_scheduled_monitor_marks_explicit_waiting_path() {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent = make_agent(agent_id, company_id, "active", None);

    // Schedule monitor in the future to make it "scheduled"
    let future_dt = Timestamp::now().as_datetime() + chrono::Duration::seconds(3600);
    let future = Timestamp::from_dt(future_dt);
    let timeout_dt = future_dt + chrono::Duration::seconds(7200);
    let mut issue = make_issue_with_assignee(
        issue_id,
        company_id,
        "OO-1",
        "Monitored",
        "todo",
        Some(agent_id),
        None,
    );
    issue.monitor_next_check_at = Some(future);
    issue.execution_policy = Some(json!({
        "monitor": {
            "timeoutAt": timeout_dt.to_rfc3339()
        }
    }));

    let source_id = Uuid::new_v4();
    let source_agent = Uuid::new_v4();
    let source = make_issue_with_assignee(source_id, company_id, "OO-2", "Source", "blocked", Some(source_agent), None);

    let input = IssueGraphLivenessInput {
        issues: vec![source, issue.clone()],
        relations: vec![IssueLivenessRelationInput {
            company_id,
            blocker_issue_id: issue_id,
            blocked_issue_id: source_id,
        }],
        agents: vec![agent, make_agent(source_agent, company_id, "active", None)],
        ..Default::default()
    };
    let findings = classify(&input);
    assert!(findings.is_empty(), "scheduled monitor should suppress finding");
}

// ============================================================================
// Service 辅助函数测试
// ============================================================================

fn make_finding(
    company_id: Uuid,
    issue_id: Uuid,
    state: IssueLivenessState,
    severity: IssueLivenessSeverity,
) -> IssueLivenessFinding {
    let recovery_id = Uuid::new_v4();
    let incident_key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
        company_id,
        issue_id,
        state: state.as_str(),
        blocker_issue_id: Some(recovery_id),
        participant_agent_id: None,
    });
    IssueLivenessFinding {
        issue_id,
        company_id,
        identifier: Some(format!("ZZ-{issue_id}")),
        state,
        severity,
        reason: "test reason".to_string(),
        dependency_path: vec![],
        recovery_issue_id: recovery_id,
        recommended_owner_agent_id: None,
        recommended_owner_candidate_agent_ids: vec![],
        recommended_owner_candidates: vec![IssueLivenessOwnerCandidate {
            agent_id: Uuid::new_v4(),
            reason: IssueLivenessOwnerCandidateReason::RootAgent,
            source_issue_id: issue_id,
        }],
        recommended_action: "test action".to_string(),
        incident_key,
        participant_agent_id: None,
        blocker_issue_id: Some(recovery_id),
    }
}

#[test]
fn r656_service_filter_by_company() {
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    let findings = vec![
        make_finding(c1, Uuid::new_v4(), IssueLivenessState::BlockedByUnassignedIssue, IssueLivenessSeverity::Critical),
        make_finding(c2, Uuid::new_v4(), IssueLivenessState::BlockedByCancelledIssue, IssueLivenessSeverity::Critical),
        make_finding(c1, Uuid::new_v4(), IssueLivenessState::InReviewWithoutActionPath, IssueLivenessSeverity::Critical),
    ];
    let filtered = filter_by_company(&findings, c1);
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|f| f.company_id == c1));
}

#[test]
fn r656_service_filter_by_state() {
    let c = Uuid::new_v4();
    let findings = vec![
        make_finding(c, Uuid::new_v4(), IssueLivenessState::BlockedByUnassignedIssue, IssueLivenessSeverity::Critical),
        make_finding(c, Uuid::new_v4(), IssueLivenessState::BlockedByCancelledIssue, IssueLivenessSeverity::Critical),
        make_finding(c, Uuid::new_v4(), IssueLivenessState::BlockedByUnassignedIssue, IssueLivenessSeverity::Critical),
    ];
    let filtered = filter_by_state(&findings, IssueLivenessState::BlockedByUnassignedIssue);
    assert_eq!(filtered.len(), 2);
}

#[test]
fn r656_service_filter_by_issue() {
    let c = Uuid::new_v4();
    let i1 = Uuid::new_v4();
    let i2 = Uuid::new_v4();
    let findings = vec![
        make_finding(c, i1, IssueLivenessState::BlockedByUnassignedIssue, IssueLivenessSeverity::Critical),
        make_finding(c, i2, IssueLivenessState::BlockedByCancelledIssue, IssueLivenessSeverity::Critical),
        make_finding(c, i1, IssueLivenessState::InReviewWithoutActionPath, IssueLivenessSeverity::Critical),
    ];
    let filtered = filter_by_issue(&findings, i1);
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|f| f.issue_id == i1));
}

#[test]
fn r656_service_dedup_by_incident_key() {
    let c = Uuid::new_v4();
    let i = Uuid::new_v4();
    let f1 = make_finding(c, i, IssueLivenessState::BlockedByUnassignedIssue, IssueLivenessSeverity::Critical);
    let f2 = f1.clone();
    let f3 = make_finding(c, i, IssueLivenessState::BlockedByCancelledIssue, IssueLivenessSeverity::Critical);
    let deduped = dedup_by_incident_key(&[f1, f2, f3]);
    assert_eq!(deduped.len(), 2);
}

#[test]
fn r656_service_summarize_groups_by_company() {
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    let findings = vec![
        make_finding(c1, Uuid::new_v4(), IssueLivenessState::BlockedByUnassignedIssue, IssueLivenessSeverity::Critical),
        make_finding(c1, Uuid::new_v4(), IssueLivenessState::BlockedByCancelledIssue, IssueLivenessSeverity::Warning),
        make_finding(c2, Uuid::new_v4(), IssueLivenessState::InvalidReviewParticipant, IssueLivenessSeverity::Critical),
    ];
    let summaries = summarize(&findings);
    assert_eq!(summaries.len(), 2);
    let s1 = summaries.iter().find(|s| s.company_id == c1).unwrap();
    assert_eq!(s1.total_findings, 2);
    let s2 = summaries.iter().find(|s| s.company_id == c2).unwrap();
    assert_eq!(s2.total_findings, 1);
}

#[test]
fn r656_service_summarize_includes_unique_issue_ids() {
    let c = Uuid::new_v4();
    let i1 = Uuid::new_v4();
    let i2 = Uuid::new_v4();
    let findings = vec![
        make_finding(c, i1, IssueLivenessState::BlockedByUnassignedIssue, IssueLivenessSeverity::Critical),
        make_finding(c, i2, IssueLivenessState::BlockedByCancelledIssue, IssueLivenessSeverity::Critical),
        make_finding(c, i1, IssueLivenessState::InReviewWithoutActionPath, IssueLivenessSeverity::Critical),
    ];
    let summaries = summarize(&findings);
    let s = &summaries[0];
    assert_eq!(s.issue_ids.len(), 2);
    assert!(s.issue_ids.contains(&i1));
    assert!(s.issue_ids.contains(&i2));
}

#[test]
fn r656_service_summarize_empty_input() {
    let summaries = summarize(&[]);
    assert!(summaries.is_empty());
}

#[test]
fn r656_service_owner_reason_str() {
    assert_eq!(
        pc_issue_liveness::owner_reason_str(IssueLivenessOwnerCandidateReason::StalledBlockerAssignee),
        "stalled_blocker_assignee"
    );
    assert_eq!(
        pc_issue_liveness::owner_reason_str(IssueLivenessOwnerCandidateReason::RootAgent),
        "root_agent"
    );
}

#[test]
fn r656_service_error_display() {
    let err = IssueLivenessError::Validation("bad input".to_string());
    assert_eq!(err.to_string(), "validation: bad input");
}

// ============================================================================
// Type serialization round-trip
// ============================================================================

#[test]
fn r656_types_serialize_camel_case() {
    let f = make_finding(
        Uuid::new_v4(),
        Uuid::new_v4(),
        IssueLivenessState::BlockedByUnassignedIssue,
        IssueLivenessSeverity::Critical,
    );
    let json = serde_json::to_value(&f).unwrap();
    assert!(json.get("issueId").is_some());
    assert!(json.get("companyId").is_some());
    assert!(json.get("recoveryIssueId").is_some());
    assert!(json.get("incidentKey").is_some());
    assert!(json.get("recommendedOwnerCandidates").is_some());
}

#[test]
fn r656_types_state_serde_round_trip() {
    for state in [
        IssueLivenessState::BlockedByUnassignedIssue,
        IssueLivenessState::BlockedByAssignedBacklogIssue,
        IssueLivenessState::BlockedByUninvokableAssignee,
        IssueLivenessState::BlockedByCancelledIssue,
        IssueLivenessState::InvalidReviewParticipant,
        IssueLivenessState::InReviewWithoutActionPath,
    ] {
        let json = serde_json::to_string(&state).unwrap();
        let back: IssueLivenessState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }
}

#[test]
fn r656_types_severity_serde_round_trip() {
    for sev in [IssueLivenessSeverity::Warning, IssueLivenessSeverity::Critical] {
        let json = serde_json::to_string(&sev).unwrap();
        let back: IssueLivenessSeverity = serde_json::from_str(&json).unwrap();
        assert_eq!(sev, back);
    }
}

#[test]
fn r656_types_summary_default() {
    let s = IssueLivenessSummary::default();
    assert_eq!(s.total_findings, 0);
    assert!(s.by_state.is_empty());
    assert!(s.by_severity.is_empty());
    assert!(s.issue_ids.is_empty());
}

// ============================================================================
// Result type alias sanity check
// ============================================================================

#[test]
fn r656_result_type_alias_compiles() {
    let r: IssueLivenessResult<()> = Ok(());
    assert!(r.is_ok());
}

// ============================================================================
// 真实 DB 端到端测试
// ============================================================================

mod db_tests {
    use pc_issue_liveness::{
        classify, IssueGraphLivenessInput, IssueLivenessAgentInput, IssueLivenessIssueInput,
        IssueLivenessRelationInput, IssueLivenessState,
    };
    use pc_repos::{
        agent::AgentRepo,
        company::CompanyRepo,
        issue::{CreateIssueInput, IssueRepo},
        project::{NewProject, ProjectRepo, ProjectStatus},
        Db,
    };
    use uuid::Uuid;

    const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

    async fn connect() -> Db {
        Db::connect(DB_URL, 5, 1).await.expect("connect to db")
    }

    async fn make_company(db: &Db, tag: &str) -> Uuid {
        let repo = CompanyRepo::new(db);
        let name = format!("IL Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
        let repo = ProjectRepo::new(db);
        let name = format!("IL proj {tag} {}", Uuid::new_v4());
        repo.create(&NewProject {
            company_id,
            goal_id: None,
            name,
            description: None,
            status: ProjectStatus::Active,
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        })
        .await
        .expect("create project")
        .id
    }

    async fn make_issue_with_identifier(
        db: &Db,
        company_id: Uuid,
        project_id: Uuid,
        prefix: &str,
        title: &str,
        status: &str,
        assignee_agent_id: Option<Uuid>,
    ) -> Uuid {
        let repo = IssueRepo::new(db);
        let input = CreateIssueInput {
            company_id,
            title,
            description: None,
            status: Some(status),
            work_mode: None,
            harness_kind: None,
            priority: Some("medium"),
            assignee_agent_id,
            assignee_user_id: None,
            project_id: Some(project_id),
            project_workspace_id: None,
            goal_id: None,
            parent_id: None,
            inherit_execution_workspace_from_issue_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            billing_code: None,
            request_depth: 0,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            blocked_by_issue_ids: None,
            label_ids: None,
            unblock_descriptor: None,
        };
        let row = repo.create_full(&input).await.expect("create issue");
        let digits: String = Uuid::new_v4().simple().to_string().chars().map(|ch| {
            let n = ch.to_digit(16).unwrap_or(0);
            char::from(b'0' + (n % 10) as u8)
        }).collect();
        let unique = format!("{prefix}-{digits}");
        sqlx::query("UPDATE issues SET identifier = $1 WHERE id = $2")
            .bind(&unique)
            .bind(row.id)
            .execute(db.pool())
            .await
            .expect("set identifier");
        row.id
    }

    async fn make_agent(db: &Db, company_id: Uuid, name: &str, status: &str) -> Uuid {
        let repo = AgentRepo::new(db);
        let row = repo.create_simple(company_id, name, "engineer").await.expect("create agent");
        if status != "active" {
            sqlx::query("UPDATE agents SET status = $1 WHERE id = $2")
                .bind(status)
                .bind(row.id)
                .execute(db.pool())
                .await
                .expect("set agent status");
        }
        row.id
    }

    async fn insert_block_relation(
        db: &Db,
        company_id: Uuid,
        blocker_id: Uuid,
        blocked_id: Uuid,
    ) {
        sqlx::query(
            "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type) VALUES ($1, $2, $3, 'blocks')",
        )
        .bind(company_id)
        .bind(blocked_id)
        .bind(blocker_id)
        .execute(db.pool())
        .await
        .expect("insert relation");
    }

    async fn set_issue_status(db: &Db, issue_id: Uuid, status: &str) {
        sqlx::query("UPDATE issues SET status = $1 WHERE id = $2")
            .bind(status)
            .bind(issue_id)
            .execute(db.pool())
            .await
            .expect("set status");
    }

    async fn reset_tables(db: &Db) {
        sqlx::query(
            "DELETE FROM issue_relations WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IL Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset issue_relations");
        sqlx::query(
            "DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IL Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset issues");
        sqlx::query(
            "DELETE FROM projects WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IL Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset projects");
        sqlx::query(
            "DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IL Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset agents");
        sqlx::query(
            "DELETE FROM companies WHERE name LIKE 'IL Co %'",
        )
        .execute(db.pool())
        .await
        .expect("reset companies");
    }

    /// Helper: load a single issue row from DB and convert to IssueLivenessIssueInput.
    async fn load_issue_input(db: &Db, issue_id: Uuid) -> IssueLivenessIssueInput {
        let row: (Uuid, Uuid, Option<String>, String, String, Option<Uuid>, Option<String>) =
            sqlx::query_as(
                "SELECT id, company_id, identifier, title, status, assignee_agent_id, assignee_user_id FROM issues WHERE id = $1",
            )
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .expect("load issue");
        IssueLivenessIssueInput {
            id: row.0,
            company_id: row.1,
            identifier: row.2,
            title: row.3,
            status: row.4,
            project_id: None,
            goal_id: None,
            parent_id: None,
            assignee_agent_id: row.5,
            assignee_user_id: row.6,
            created_by_agent_id: None,
            created_by_user_id: None,
            execution_policy: None,
            execution_state: None,
            monitor_next_check_at: None,
            monitor_attempt_count: None,
        }
    }

    /// Helper: load company agents as IssueLivenessAgentInput.
    async fn load_company_agents(db: &Db, company_id: Uuid) -> Vec<IssueLivenessAgentInput> {
        let rows: Vec<(Uuid, Uuid, String, String, String, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, company_id, name, role, status, reports_to FROM agents WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_all(db.pool())
        .await
        .expect("load agents");
        rows.into_iter()
            .map(|r| IssueLivenessAgentInput {
                id: r.0,
                company_id: r.1,
                name: r.2,
                role: r.3,
                title: None,
                status: r.4,
                reports_to: r.5,
            })
            .collect()
    }

    /// Helper: load issue_relations of type 'blocks' for company.
    async fn load_block_relations(db: &Db, company_id: Uuid) -> Vec<IssueLivenessRelationInput> {
        let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT company_id, issue_id, related_issue_id FROM issue_relations WHERE company_id = $1 AND type = 'blocks'",
        )
        .bind(company_id)
        .fetch_all(db.pool())
        .await
        .expect("load relations");
        rows.into_iter()
            .map(|r| IssueLivenessRelationInput {
                company_id: r.0,
                blocker_issue_id: r.2,
                blocked_issue_id: r.1,
            })
            .collect()
    }

    #[tokio::test]
    async fn r656_db_blocked_by_cancelled_issue_e2e() {
        let db = connect().await;
        reset_tables(&db).await;

        let company_id = make_company(&db, "bc").await;
        let project_id = make_project(&db, company_id, "bc").await;
        let agent_id = make_agent(&db, company_id, "alice", "active").await;

        let source_id = make_issue_with_identifier(
            &db, company_id, project_id, "BC", "Source issue", "todo", Some(agent_id),
        ).await;
        let blocker_id = make_issue_with_identifier(
            &db, company_id, project_id, "BC", "Cancelled blocker", "todo", Some(agent_id),
        ).await;
        // Mark blocker as cancelled
        set_issue_status(&db, blocker_id, "cancelled").await;
        // Mark source as blocked
        set_issue_status(&db, source_id, "blocked").await;
        // Insert relation
        insert_block_relation(&db, company_id, blocker_id, source_id).await;

        // Load and classify
        let issues = vec![
            load_issue_input(&db, source_id).await,
            load_issue_input(&db, blocker_id).await,
        ];
        let agents = load_company_agents(&db, company_id).await;
        let relations = load_block_relations(&db, company_id).await;

        let input = IssueGraphLivenessInput {
            issues,
            relations,
            agents,
            ..Default::default()
        };
        let findings = classify(&input);
        assert_eq!(findings.len(), 1, "expected exactly one finding");
        let f = &findings[0];
        assert_eq!(f.state, IssueLivenessState::BlockedByCancelledIssue);
        assert_eq!(f.issue_id, source_id);
        assert_eq!(f.recovery_issue_id, blocker_id);
    }

    #[tokio::test]
    async fn r656_db_blocked_by_uninvokable_assignee_e2e() {
        let db = connect().await;
        reset_tables(&db).await;

        let company_id = make_company(&db, "bu").await;
        let project_id = make_project(&db, company_id, "bu").await;

        // Active source agent
        let active_id = make_agent(&db, company_id, "alice", "active").await;
        // Terminated blocker agent
        let terminated_id = make_agent(&db, company_id, "bob", "terminated").await;

        let source_id = make_issue_with_identifier(
            &db, company_id, project_id, "BU", "Source", "todo", Some(active_id),
        ).await;
        let blocker_id = make_issue_with_identifier(
            &db, company_id, project_id, "BU", "Blocker", "todo", Some(terminated_id),
        ).await;
        set_issue_status(&db, source_id, "blocked").await;
        insert_block_relation(&db, company_id, blocker_id, source_id).await;

        let issues = vec![
            load_issue_input(&db, source_id).await,
            load_issue_input(&db, blocker_id).await,
        ];
        let agents = load_company_agents(&db, company_id).await;
        let relations = load_block_relations(&db, company_id).await;

        let input = IssueGraphLivenessInput {
            issues,
            relations,
            agents,
            ..Default::default()
        };
        let findings = classify(&input);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].state, IssueLivenessState::BlockedByUninvokableAssignee);
        assert!(findings[0].reason.contains("terminated"));
    }

    #[tokio::test]
    async fn r656_db_no_findings_for_invokable_blocker_e2e() {
        let db = connect().await;
        reset_tables(&db).await;

        let company_id = make_company(&db, "ni").await;
        let project_id = make_project(&db, company_id, "ni").await;
        let active_id = make_agent(&db, company_id, "alice", "active").await;

        let source_id = make_issue_with_identifier(
            &db, company_id, project_id, "NI", "Source", "todo", Some(active_id),
        ).await;
        let blocker_id = make_issue_with_identifier(
            &db, company_id, project_id, "NI", "Blocker", "todo", Some(active_id),
        ).await;
        set_issue_status(&db, source_id, "blocked").await;
        insert_block_relation(&db, company_id, blocker_id, source_id).await;

        let issues = vec![
            load_issue_input(&db, source_id).await,
            load_issue_input(&db, blocker_id).await,
        ];
        let agents = load_company_agents(&db, company_id).await;
        let relations = load_block_relations(&db, company_id).await;

        let input = IssueGraphLivenessInput {
            issues,
            relations,
            agents,
            ..Default::default()
        };
        let findings = classify(&input);
        assert!(findings.is_empty(), "invokable blocker should suppress finding");
    }

    #[tokio::test]
    async fn r656_db_blocked_by_unassigned_issue_e2e() {
        let db = connect().await;
        reset_tables(&db).await;

        let company_id = make_company(&db, "ua").await;
        let project_id = make_project(&db, company_id, "ua").await;
        let source_agent_id = make_agent(&db, company_id, "alice", "active").await;

        let source_id = make_issue_with_identifier(
            &db, company_id, project_id, "UA", "Source", "todo", Some(source_agent_id),
        ).await;
        let blocker_id = make_issue_with_identifier(
            &db, company_id, project_id, "UA", "Unassigned blocker", "todo", None,
        ).await;
        set_issue_status(&db, source_id, "blocked").await;
        insert_block_relation(&db, company_id, blocker_id, source_id).await;

        let issues = vec![
            load_issue_input(&db, source_id).await,
            load_issue_input(&db, blocker_id).await,
        ];
        let agents = load_company_agents(&db, company_id).await;
        let relations = load_block_relations(&db, company_id).await;

        let input = IssueGraphLivenessInput {
            issues,
            relations,
            agents,
            ..Default::default()
        };
        let findings = classify(&input);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].state, IssueLivenessState::BlockedByUnassignedIssue);
        assert!(findings[0].reason.contains("unassigned"));
    }

    #[tokio::test]
    async fn r656_db_done_issues_no_findings_e2e() {
        let db = connect().await;
        reset_tables(&db).await;

        let company_id = make_company(&db, "dn").await;
        let project_id = make_project(&db, company_id, "dn").await;
        let agent_id = make_agent(&db, company_id, "alice", "active").await;

        let source_id = make_issue_with_identifier(
            &db, company_id, project_id, "DN", "Done source", "todo", Some(agent_id),
        ).await;
        let blocker_id = make_issue_with_identifier(
            &db, company_id, project_id, "DN", "Done blocker", "todo", Some(agent_id),
        ).await;
        set_issue_status(&db, source_id, "done").await;
        set_issue_status(&db, blocker_id, "done").await;
        insert_block_relation(&db, company_id, blocker_id, source_id).await;

        let issues = vec![
            load_issue_input(&db, source_id).await,
            load_issue_input(&db, blocker_id).await,
        ];
        let agents = load_company_agents(&db, company_id).await;
        let relations = load_block_relations(&db, company_id).await;

        let input = IssueGraphLivenessInput {
            issues,
            relations,
            agents,
            ..Default::default()
        };
        let findings = classify(&input);
        assert!(findings.is_empty(), "done issues should not generate findings");
    }
}
