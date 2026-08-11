//! 纯函数分类器 — 对应 Node `services/recovery/issue-graph-liveness.ts`
//! 的 `classifyIssueGraphLiveness` 函数。
//!
//! 单一职责：消费 `IssueGraphLivenessInput`，返回 `IssueLivenessFinding` 列表。
//! 不依赖 DB / 网络 — 所有数据通过 input 传入。
//!
//! 关键复刻点：
//! - blocker chain DFS（沿 `blocked` status 向下探测，叶节点评估）
//! - owner candidate 收集（按 5 种 reason 排序，invokable 过滤）
//! - incident_key 生成（确定性字符串拼接）
//! - in_review 的两种 finding（participant invalid / 无 action path）

use std::collections::{HashMap, HashSet};

use pc_repos::agent_invokability::{
    evaluate_agent_invokability, AgentInvokabilityBlockReason, AgentOrgRow,
};
use serde_json::Value;

use super::incident_key::{build_issue_graph_liveness_incident_key, IncidentKeyInput};
use super::types::{
    IssueGraphLivenessInput, IssueLivenessAgentInput, IssueLivenessDependencyPathEntry,
    IssueLivenessExecutionPathInput, IssueLivenessFinding, IssueLivenessIssueInput,
    IssueLivenessOwnerCandidate, IssueLivenessOwnerCandidateReason, IssueLivenessRelationInput,
    IssueLivenessSeverity,
    IssueLivenessState, IssueLivenessWaitingPathInput,
};

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn issue_label(issue: &IssueLivenessIssueInput) -> String {
    issue.identifier.clone().unwrap_or_else(|| issue.id.to_string())
}

fn path_entry(issue: &IssueLivenessIssueInput) -> IssueLivenessDependencyPathEntry {
    IssueLivenessDependencyPathEntry {
        issue_id: issue.id,
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        status: issue.status.clone(),
    }
}

fn is_invokable_agent(
    agent: Option<&IssueLivenessAgentInput>,
    company_agents: &[IssueLivenessAgentInput],
) -> bool {
    match agent {
        None => false,
        Some(a) => {
            let org_row = to_org_row(a);
            let company_org: Vec<AgentOrgRow> =
                company_agents.iter().map(to_org_row).collect();
            evaluate_agent_invokability(Some(&org_row), &company_org).is_invokable()
        }
    }
}

fn to_org_row(a: &IssueLivenessAgentInput) -> AgentOrgRow {
    AgentOrgRow {
        id: a.id,
        company_id: a.company_id,
        name: a.name.clone(),
        reports_to: a.reports_to,
        status: a.status.clone(),
    }
}

fn has_active_execution_path(
    company_id: uuid::Uuid,
    issue_id: uuid::Uuid,
    active_runs: &[IssueLivenessExecutionPathInput],
    queued_wake_requests: &[IssueLivenessExecutionPathInput],
) -> bool {
    active_runs
        .iter()
        .chain(queued_wake_requests.iter())
        .any(|entry| entry.company_id == company_id && entry.issue_id == Some(issue_id))
}

fn has_waiting_path(
    company_id: uuid::Uuid,
    issue_id: uuid::Uuid,
    waiting_paths: &[IssueLivenessWaitingPathInput],
) -> bool {
    waiting_paths
        .iter()
        .any(|entry| entry.company_id == company_id && entry.issue_id == issue_id)
}

fn read_record(value: Option<&Value>) -> Option<Value> {
    value.and_then(|v| {
        if v.is_object() {
            Some(v.clone())
        } else {
            None
        }
    })
}

fn read_positive_integer(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(v) => v
            .as_i64()
            .filter(|n| *n > 0),
        None => None,
    }
}

fn read_date_ms(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(v) => {
            if let Some(s) = v.as_str() {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.timestamp_millis())
            } else if v.is_i64() {
                Some(v.as_i64().unwrap_or(0))
            } else {
                None
            }
        }
        None => None,
    }
}

fn monitor_from_issue(issue: &IssueLivenessIssueInput) -> (Option<Value>, Option<Value>) {
    let policy_monitor = issue
        .execution_policy
        .as_ref()
        .and_then(|p| read_record(p.as_object().and_then(|_| p.get("monitor"))));
    let state_monitor = issue
        .execution_state
        .as_ref()
        .and_then(|s| read_record(s.as_object().and_then(|_| s.get("monitor"))));
    (policy_monitor, state_monitor)
}

fn has_scheduled_monitor(issue: &IssueLivenessIssueInput, now_ms: i64) -> bool {
    let next_check_ms = issue
        .monitor_next_check_at
        .as_ref()
        .and_then(|t| Some(t.as_datetime().timestamp_millis()));
    if let Some(ms) = next_check_ms {
        if ms <= now_ms {
            return false;
        }
    } else {
        return false;
    }

    let (policy_monitor, state_monitor) = monitor_from_issue(issue);

    let timeout_at = policy_monitor
        .as_ref()
        .and_then(|m| read_date_ms(m.get("timeoutAt")))
        .or_else(|| {
            state_monitor
                .as_ref()
                .and_then(|m| read_date_ms(m.get("timeoutAt")))
        });
    if let Some(ms) = timeout_at {
        if ms <= now_ms {
            return false;
        }
    }

    let max_attempts = policy_monitor
        .as_ref()
        .and_then(|m| read_positive_integer(m.get("maxAttempts")))
        .or_else(|| {
            state_monitor
                .as_ref()
                .and_then(|m| read_positive_integer(m.get("maxAttempts")))
        });
    let state_attempt_count = state_monitor
        .as_ref()
        .and_then(|m| read_positive_integer(m.get("attemptCount")))
        .unwrap_or(0);
    let attempt_count = issue
        .monitor_attempt_count
        .map(|n| n as i64)
        .unwrap_or(state_attempt_count);
    if let Some(max) = max_attempts {
        if attempt_count >= max {
            return false;
        }
    }

    true
}

fn read_principal_agent_id(principal: Option<&Value>) -> Option<uuid::Uuid> {
    let p = principal?;
    let obj = p.as_object()?;
    if obj.get("type")?.as_str()? != "agent" {
        return None;
    }
    let agent_id_str = obj.get("agentId")?.as_str()?;
    if agent_id_str.is_empty() {
        return None;
    }
    uuid::Uuid::parse_str(agent_id_str).ok()
}

fn principal_is_resolvable_user(principal: Option<&Value>) -> bool {
    match principal.and_then(|p| p.as_object()) {
        Some(obj) => {
            obj.get("type").and_then(|v| v.as_str()) == Some("user")
                && obj
                    .get("userId")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
        }
        None => false,
    }
}

fn add_owner_candidate(
    candidates: &mut Vec<IssueLivenessOwnerCandidate>,
    seen: &mut HashSet<uuid::Uuid>,
    company_agents: &[IssueLivenessAgentInput],
    company_id: uuid::Uuid,
    agent_id: Option<uuid::Uuid>,
    reason: IssueLivenessOwnerCandidateReason,
    source_issue_id: uuid::Uuid,
) {
    let Some(agent_id) = agent_id else { return };
    eprintln!("DBG: add_owner_candidate id={} reason={:?} seen={}", agent_id, reason, seen.contains(&agent_id));
    if seen.contains(&agent_id) {
        return;
    }
    let agent = company_agents.iter().find(|a| a.id == agent_id);
    match agent {
        None => { eprintln!("DBG:   agent not in company_agents"); return; },
        Some(a) => {
            if a.company_id != company_id {
                        return;
            }
            if !is_invokable_agent(Some(a), company_agents) {
                return;
            }
        }
    }
    seen.insert(agent_id);
    candidates.push(IssueLivenessOwnerCandidate {
        agent_id,
        reason,
        source_issue_id,
    });
}

fn add_agent_chain_candidates(
    candidates: &mut Vec<IssueLivenessOwnerCandidate>,
    seen: &mut HashSet<uuid::Uuid>,
    start_agent_id: Option<uuid::Uuid>,
    agents_by_id: &HashMap<uuid::Uuid, &IssueLivenessAgentInput>,
    company_id: uuid::Uuid,
    reason: IssueLivenessOwnerCandidateReason,
    source_issue_id: uuid::Uuid,
) {
    let mut chain_seen = HashSet::new();
    let mut current = start_agent_id.and_then(|id| agents_by_id.get(&id).copied());
    while let Some(agent) = current {
        let Some(reports_to) = agent.reports_to else {
            break;
        };
        if chain_seen.contains(&reports_to) {
            break;
        }
        chain_seen.insert(reports_to);
        let manager = match agents_by_id.get(&reports_to) {
            Some(m) => *m,
            None => break,
        };
        if manager.company_id != company_id {
            break;
        }
        let company_agents: Vec<IssueLivenessAgentInput> =
            agents_by_id.values().map(|a| (*a).clone()).collect();
        add_owner_candidate(
            candidates,
            seen,
            &company_agents,
            company_id,
            Some(manager.id),
            reason,
            source_issue_id,
        );
        current = Some(manager);
    }
}

fn ordered_invokable_agents(
    agents: &[IssueLivenessAgentInput],
    company_id: uuid::Uuid,
) -> Vec<IssueLivenessAgentInput> {
    let mut filtered: Vec<IssueLivenessAgentInput> = agents
        .iter()
        .filter(|a| a.company_id == company_id && is_invokable_agent(Some(a), agents))
        .cloned()
        .collect();
    filtered.sort_by(|a, b| a.id.to_string().cmp(&b.id.to_string()));
    filtered
}

fn owner_candidates_for_recovery_issue(
    issue: &IssueLivenessIssueInput,
    agents: &[IssueLivenessAgentInput],
    agents_by_id: &HashMap<uuid::Uuid, &IssueLivenessAgentInput>,
    include_stalled_assignee: bool,
) -> Vec<IssueLivenessOwnerCandidate> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    if include_stalled_assignee && issue.status != "cancelled" && issue.status != "done" {
        add_owner_candidate(
            &mut candidates,
            &mut seen,
            agents,
            issue.company_id,
            issue.assignee_agent_id,
            IssueLivenessOwnerCandidateReason::StalledBlockerAssignee,
            issue.id,
        );
    }

    add_agent_chain_candidates(
        &mut candidates,
        &mut seen,
        issue.assignee_agent_id,
        agents_by_id,
        issue.company_id,
        IssueLivenessOwnerCandidateReason::AssigneeReportingChain,
        issue.id,
    );
    add_agent_chain_candidates(
        &mut candidates,
        &mut seen,
        issue.created_by_agent_id,
        agents_by_id,
        issue.company_id,
        IssueLivenessOwnerCandidateReason::CreatorReportingChain,
        issue.id,
    );

    let invokable_agents = ordered_invokable_agents(agents, issue.company_id);
    eprintln!("DBG: invokable_agents count={} ids={:?}", invokable_agents.len(), invokable_agents.iter().map(|a| a.id).collect::<Vec<_>>());
    for agent in &invokable_agents {
        if agent.reports_to.is_none() {
            add_owner_candidate(
                &mut candidates,
                &mut seen,
                agents,
                issue.company_id,
                Some(agent.id),
                IssueLivenessOwnerCandidateReason::RootAgent,
                issue.id,
            );
        }
    }
    for agent in &invokable_agents {
        add_owner_candidate(
            &mut candidates,
            &mut seen,
            agents,
            issue.company_id,
            Some(agent.id),
            IssueLivenessOwnerCandidateReason::OrderedInvokableFallback,
            issue.id,
        );
    }

    candidates
}

// -----------------------------------------------------------------------------
// Finding builder
// -----------------------------------------------------------------------------

struct FindingInput<'a> {
    issue: &'a IssueLivenessIssueInput,
    state: IssueLivenessState,
    severity: IssueLivenessSeverity,
    reason: String,
    dependency_path: &'a [&'a IssueLivenessIssueInput],
    recovery_issue: &'a IssueLivenessIssueInput,
    recommended_owner_candidate_agent_ids: Vec<uuid::Uuid>,
    recommended_owner_candidates: Vec<IssueLivenessOwnerCandidate>,
    recommended_action: String,
    blocker_issue_id: Option<uuid::Uuid>,
    participant_agent_id: Option<uuid::Uuid>,
}

fn build_finding(input: FindingInput<'_>) -> IssueLivenessFinding {
    let state_str = input.state.as_str();
    let incident_key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
        company_id: input.issue.company_id,
        issue_id: input.issue.id,
        state: state_str,
        blocker_issue_id: input.blocker_issue_id,
        participant_agent_id: input.participant_agent_id,
    });

    IssueLivenessFinding {
        issue_id: input.issue.id,
        company_id: input.issue.company_id,
        identifier: input.issue.identifier.clone(),
        state: input.state,
        severity: input.severity,
        reason: input.reason,
        dependency_path: input.dependency_path.iter().map(|i| path_entry(*i)).collect(),
        recovery_issue_id: input.recovery_issue.id,
        recommended_owner_agent_id: input
            .recommended_owner_candidate_agent_ids
            .first()
            .copied(),
        recommended_owner_candidate_agent_ids: input.recommended_owner_candidate_agent_ids,
        recommended_owner_candidates: input.recommended_owner_candidates,
        recommended_action: input.recommended_action,
        incident_key,
        participant_agent_id: input.participant_agent_id,
        blocker_issue_id: input.blocker_issue_id,
    }
}

// -----------------------------------------------------------------------------
// Main classifier
// -----------------------------------------------------------------------------

/// 主分类函数（与 Node `classifyIssueGraphLiveness` 1:1 对齐）。
///
/// 输入：完整 issue graph + agent 列表 + execution / waiting paths。
/// 输出：所有 liveness findings。
pub fn classify_issue_graph_liveness(
    input: &IssueGraphLivenessInput,
) -> Vec<IssueLivenessFinding> {
    let now_ms = input
        .now
        .as_ref()
        .map(|t| t.as_datetime().timestamp_millis())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

    let issues_by_id: HashMap<uuid::Uuid, &IssueLivenessIssueInput> =
        input.issues.iter().map(|i| (i.id, i)).collect();
    let agents_by_id: HashMap<uuid::Uuid, &IssueLivenessAgentInput> =
        input.agents.iter().map(|a| (a.id, a)).collect();

    let mut blockers_by_blocked_issue_id: HashMap<uuid::Uuid, Vec<&IssueLivenessRelationInput>> =
        HashMap::new();
    let mut unresolved_blockers: HashSet<uuid::Uuid> = HashSet::new();
    let mut findings: Vec<IssueLivenessFinding> = Vec::new();

    let active_runs = input.active_runs.clone().unwrap_or_default();
    let queued_wake_requests = input.queued_wake_requests.clone().unwrap_or_default();
    let pending_interactions = input.pending_interactions.clone().unwrap_or_default();
    let pending_approvals = input.pending_approvals.clone().unwrap_or_default();
    let open_recovery_issues = input.open_recovery_issues.clone().unwrap_or_default();

    for relation in &input.relations {
        blockers_by_blocked_issue_id
            .entry(relation.blocked_issue_id)
            .or_default()
            .push(relation);

        let blocker = issues_by_id.get(&relation.blocker_issue_id).copied();
        let blocked = issues_by_id.get(&relation.blocked_issue_id).copied();
        if let (Some(b), Some(d)) = (blocker, blocked) {
            if b.company_id == relation.company_id
                && d.company_id == relation.company_id
                && b.status != "done"
                && b.status != "cancelled"
                && d.status == "blocked"
            {
                unresolved_blockers.insert(b.id);
            }
        }
    }

    for list in blockers_by_blocked_issue_id.values_mut() {
        list.sort_by(|left, right| {
            let left_label = issues_by_id
                .get(&left.blocker_issue_id)
                .map(|i| issue_label(i))
                .unwrap_or_else(|| left.blocker_issue_id.to_string());
            let right_label = issues_by_id
                .get(&right.blocker_issue_id)
                .map(|i| issue_label(i))
                .unwrap_or_else(|| right.blocker_issue_id.to_string());
            left_label.cmp(&right_label)
        });
    }

    // ------------------------------------------------------------------
    // 闭包：检测 issue 是否有 explicit waiting path
    // ------------------------------------------------------------------

    let has_explicit_waiting_path = |issue: &IssueLivenessIssueInput| -> bool {
        if issue.assignee_user_id.is_some() {
            return true;
        }
        if has_scheduled_monitor(issue, now_ms) {
            return true;
        }
        if has_active_execution_path(
            issue.company_id,
            issue.id,
            &active_runs,
            &queued_wake_requests,
        ) {
            return true;
        }
        if has_waiting_path(issue.company_id, issue.id, &pending_interactions) {
            return true;
        }
        if has_waiting_path(issue.company_id, issue.id, &pending_approvals) {
            return true;
        }
        if has_waiting_path(issue.company_id, issue.id, &open_recovery_issues) {
            return true;
        }
        false
    };

    // ------------------------------------------------------------------
    // review finding
    // ------------------------------------------------------------------

    let review_finding = |source: &IssueLivenessIssueInput,
                          review_issue: &IssueLivenessIssueInput,
                          dependency_path: &[&IssueLivenessIssueInput]|
     -> Option<IssueLivenessFinding> {
        if review_issue.status != "in_review" {
            return None;
        }
        if has_explicit_waiting_path(review_issue) {
            return None;
        }

        let owner_candidates = owner_candidates_for_recovery_issue(
            review_issue,
            &input.agents,
            &agents_by_id,
            true,
        );

        let participant = review_issue
            .execution_state
            .as_ref()
            .and_then(|s| s.get("currentParticipant"));
        let participant_agent_id = read_principal_agent_id(participant);

        if let Some(pid) = participant_agent_id {
            let participant_agent = agents_by_id.get(&pid).copied();
            if let Some(pa) = participant_agent {
                if pa.company_id == review_issue.company_id
                    && is_invokable_agent(Some(pa), &input.agents)
                {
                    return None;
                }
            }
            return Some(build_finding(FindingInput {
                issue: source,
                state: IssueLivenessState::InvalidReviewParticipant,
                severity: IssueLivenessSeverity::Critical,
                reason: if participant_agent.is_some() {
                    format!(
                        "{} is in review, but current participant agent is {}.",
                        issue_label(review_issue),
                        participant_agent.unwrap().status
                    )
                } else {
                    format!(
                        "{} is in review, but current participant agent cannot be resolved.",
                        issue_label(review_issue)
                    )
                },
                dependency_path,
                recovery_issue: review_issue,
                recommended_owner_candidate_agent_ids: owner_candidates
                    .iter()
                    .map(|c| c.agent_id)
                    .collect(),
                recommended_owner_candidates: owner_candidates,
                recommended_action: format!(
                    "Repair {}'s review participant or return the issue to an active assignee with a clear change request.",
                    issue_label(review_issue)
                ),
                blocker_issue_id: None,
                participant_agent_id: Some(pid),
            }));
        }

        if principal_is_resolvable_user(participant) {
            return None;
        }

        if review_issue.execution_state.is_some() {
            return Some(build_finding(FindingInput {
                issue: source,
                state: IssueLivenessState::InvalidReviewParticipant,
                severity: IssueLivenessSeverity::Critical,
                reason: format!(
                    "{} is in review, but its current participant cannot be resolved.",
                    issue_label(review_issue)
                ),
                dependency_path,
                recovery_issue: review_issue,
                recommended_owner_candidate_agent_ids: owner_candidates
                    .iter()
                    .map(|c| c.agent_id)
                    .collect(),
                recommended_owner_candidates: owner_candidates,
                recommended_action: format!(
                    "Repair {}'s review participant or return the issue to an active assignee with a clear change request.",
                    issue_label(review_issue)
                ),
                blocker_issue_id: None,
                participant_agent_id: None,
            }));
        }

        if review_issue.assignee_agent_id.is_none() || review_issue.assignee_user_id.is_some() {
            return None;
        }

        Some(build_finding(FindingInput {
            issue: source,
            state: IssueLivenessState::InReviewWithoutActionPath,
            severity: IssueLivenessSeverity::Critical,
            reason: format!(
                "{} is in review with an agent assignee but no participant, interaction, approval, user owner, wake, active run, or recovery issue owning the next action.",
                issue_label(review_issue)
            ),
            dependency_path,
            recovery_issue: review_issue,
            recommended_owner_candidate_agent_ids: owner_candidates
                .iter()
                .map(|c| c.agent_id)
                .collect(),
            recommended_owner_candidates: owner_candidates,
            recommended_action: format!(
                "Review {} and make the next action explicit: add a reviewer/interaction, return it to active work with a change request, mark it done if accepted, or open a bounded recovery issue.",
                issue_label(review_issue)
            ),
            blocker_issue_id: Some(review_issue.id),
            participant_agent_id: None,
        }))
    };

    // ------------------------------------------------------------------
    // leaf blocker finding
    // ------------------------------------------------------------------

    let blocked_finding_for_leaf = |source: &IssueLivenessIssueInput,
                                    blocker: &IssueLivenessIssueInput,
                                    dependency_path: &[&IssueLivenessIssueInput]|
     -> Option<IssueLivenessFinding> {
        let owner_candidates = owner_candidates_for_recovery_issue(
            blocker,
            &input.agents,
            &agents_by_id,
            true,
        );

        if blocker.status == "cancelled" {
            return Some(build_finding(FindingInput {
                issue: source,
                state: IssueLivenessState::BlockedByCancelledIssue,
                severity: IssueLivenessSeverity::Critical,
                reason: format!(
                    "{} is still blocked by cancelled issue {}.",
                    issue_label(source),
                    issue_label(blocker)
                ),
                dependency_path,
                recovery_issue: blocker,
                recommended_owner_candidate_agent_ids: owner_candidates
                    .iter()
                    .map(|c| c.agent_id)
                    .collect(),
                recommended_owner_candidates: owner_candidates,
                recommended_action: format!(
                    "Inspect {} and either remove it from {}'s blockers or replace it with an actionable unblock issue.",
                    issue_label(blocker),
                    issue_label(source)
                ),
                blocker_issue_id: Some(blocker.id),
                participant_agent_id: None,
            }));
        }

        if has_explicit_waiting_path(blocker) {
            return None;
        }

        if blocker.status == "in_review" {
            return review_finding(source, blocker, dependency_path);
        }

        if blocker.status == "backlog" && blocker.assignee_agent_id.is_some() {
            return Some(build_finding(FindingInput {
                issue: source,
                state: IssueLivenessState::BlockedByAssignedBacklogIssue,
                severity: IssueLivenessSeverity::Critical,
                reason: format!(
                    "{} is blocked by assigned backlog issue {} with no wake, active run, human owner, interaction, approval, monitor, or recovery issue owning the next action.",
                    issue_label(source),
                    issue_label(blocker)
                ),
                dependency_path,
                recovery_issue: blocker,
                recommended_owner_candidate_agent_ids: owner_candidates
                    .iter()
                    .map(|c| c.agent_id)
                    .collect(),
                recommended_owner_candidates: owner_candidates,
                recommended_action: format!(
                    "Review {} and either move it to todo so the assignee wakes, assign a human owner or interaction if it is intentionally parked, or remove it from {}'s blockers if it is no longer required.",
                    issue_label(blocker),
                    issue_label(source)
                ),
                blocker_issue_id: Some(blocker.id),
                participant_agent_id: None,
            }));
        }

        if blocker.assignee_agent_id.is_none() && blocker.assignee_user_id.is_none() {
            return Some(build_finding(FindingInput {
                issue: source,
                state: IssueLivenessState::BlockedByUnassignedIssue,
                severity: IssueLivenessSeverity::Critical,
                reason: format!(
                    "{} is blocked by unassigned issue {} with no user owner.",
                    issue_label(source),
                    issue_label(blocker)
                ),
                dependency_path,
                recovery_issue: blocker,
                recommended_owner_candidate_agent_ids: owner_candidates
                    .iter()
                    .map(|c| c.agent_id)
                    .collect(),
                recommended_owner_candidates: owner_candidates,
                recommended_action: format!(
                    "Assign {} to an owner who can complete it, or remove it from {}'s blockers if it is no longer required.",
                    issue_label(blocker),
                    issue_label(source)
                ),
                blocker_issue_id: Some(blocker.id),
                participant_agent_id: None,
            }));
        }

        if blocker.assignee_agent_id.is_none() {
            return None;
        }

        let blocker_agent = agents_by_id.get(&blocker.assignee_agent_id.unwrap()).copied();
        let blocker_eligibility_invalid_org = blocker_agent
            .map(|a| {
                let org_row = to_org_row(a);
                let company_org: Vec<AgentOrgRow> =
                    input.agents.iter().map(to_org_row).collect();
                let inv = evaluate_agent_invokability(Some(&org_row), &company_org);
                (inv, org_row.status)
            });

        eprintln!("DBG: blocker_eligibility_invalid_org = {:?}", blocker_eligibility_invalid_org.is_some());
        match blocker_eligibility_invalid_org {
            None => Some(build_finding(FindingInput {
                issue: source,
                state: IssueLivenessState::BlockedByUninvokableAssignee,
                severity: IssueLivenessSeverity::Critical,
                reason: format!(
                    "{} is blocked by {}, but its assignee no longer exists.",
                    issue_label(source),
                    issue_label(blocker)
                ),
                dependency_path,
                recovery_issue: blocker,
                recommended_owner_candidate_agent_ids: owner_candidates
                    .iter()
                    .map(|c| c.agent_id)
                    .collect(),
                recommended_owner_candidates: owner_candidates,
                recommended_action: format!(
                    "Review {} and assign it to an active owner or replace the blocker with an actionable issue.",
                    issue_label(blocker)
                ),
                blocker_issue_id: Some(blocker.id),
                participant_agent_id: None,
            })),
            Some((inv, status)) => {
                if inv.is_invokable() {
                    return None;
                }
                let reason = match inv {
                    pc_repos::agent_invokability::AgentInvokability::Blocked {
                        reason: AgentInvokabilityBlockReason::ManagerCompanyMismatch,
                        ..
                    } => "in an invalid org chain".to_string(),
                    _ => status,
                };
                Some(build_finding(FindingInput {
                    issue: source,
                    state: IssueLivenessState::BlockedByUninvokableAssignee,
                    severity: IssueLivenessSeverity::Critical,
                    reason: format!(
                        "{} is blocked by {}, but its assignee is {}.",
                        issue_label(source),
                        issue_label(blocker),
                        reason
                    ),
                    dependency_path,
                    recovery_issue: blocker,
                    recommended_owner_candidate_agent_ids: owner_candidates
                        .iter()
                        .map(|c| c.agent_id)
                        .collect(),
                    recommended_owner_candidates: owner_candidates,
                    recommended_action: format!(
                        "Review {} and assign it to an active owner or replace the blocker with an actionable issue.",
                        issue_label(blocker)
                    ),
                    blocker_issue_id: Some(blocker.id),
                    participant_agent_id: None,
                }))
            }
        }
    };

    // ------------------------------------------------------------------
    // DFS over blocked chain
    // ------------------------------------------------------------------

    fn first_blocked_chain_finding<'a>(
        source: &IssueLivenessIssueInput,
        current: &IssueLivenessIssueInput,
        dependency_path: Vec<&'a IssueLivenessIssueInput>,
        seen: &mut HashSet<uuid::Uuid>,
        issues_by_id: &'a HashMap<uuid::Uuid, &IssueLivenessIssueInput>,
        blockers_by_blocked_issue_id: &'a HashMap<uuid::Uuid, Vec<&IssueLivenessRelationInput>>,
        blocked_finding_for_leaf: &impl Fn(
            &IssueLivenessIssueInput,
            &IssueLivenessIssueInput,
            &[&IssueLivenessIssueInput],
        ) -> Option<IssueLivenessFinding>,
        has_explicit_waiting_path: &impl Fn(&IssueLivenessIssueInput) -> bool,
    ) -> Option<IssueLivenessFinding> {
        eprintln!("DBG: DFS current={} path_len={}", current.id, dependency_path.len());
        if seen.contains(&current.id) {
            return None;
        }
        seen.insert(current.id);

        let relations = blockers_by_blocked_issue_id
            .get(&current.id)
            .cloned()
            .unwrap_or_default();
        for relation in relations {
            if relation.company_id != current.company_id || relation.company_id != source.company_id
            {
                continue;
            }
            let blocker = match issues_by_id.get(&relation.blocker_issue_id) {
                Some(b) => *b,
                None => continue,
            };
            if blocker.company_id != source.company_id || blocker.status == "done" {
                continue;
            }
            let mut path = dependency_path.clone();
            path.push(blocker);
            if blocker.status == "blocked" {
                if let Some(nested) = first_blocked_chain_finding(
                    source,
                    blocker,
                    path.clone(),
                    &mut seen.clone(),
                    issues_by_id,
                    blockers_by_blocked_issue_id,
                    blocked_finding_for_leaf,
                    has_explicit_waiting_path,
                ) {
                    return Some(nested);
                }
                if has_explicit_waiting_path(blocker) {
                    continue;
                }
            }

            if let Some(leaf) = blocked_finding_for_leaf(source, blocker, &path) {
                return Some(leaf);
            }
        }
        None
    }

    for issue in &input.issues {
        let has_unresolved_blocker_edge = blockers_by_blocked_issue_id
            .get(&issue.id)
            .map(|rels| {
                rels.iter().any(|relation| {
                    if relation.company_id != issue.company_id {
                        return false;
                    }
                    match issues_by_id.get(&relation.blocker_issue_id) {
                        Some(b) => {
                            b.company_id == issue.company_id && b.status != "done"
                        }
                        None => false,
                    }
                })
            })
            .unwrap_or(false);

        let should_inspect_blocked_chain = issue.status == "blocked"
            || (issue.status != "done"
                && issue.status != "cancelled"
                && issue.assignee_agent_id.is_some()
                && has_unresolved_blocker_edge);

        let mut chain_finding: Option<IssueLivenessFinding> = None;
        if should_inspect_blocked_chain {
            if unresolved_blockers.contains(&issue.id) {
                continue;
            }
            chain_finding = first_blocked_chain_finding(
                issue,
                issue,
                vec![issue],
                &mut HashSet::new(),
                &issues_by_id,
                &blockers_by_blocked_issue_id,
                &blocked_finding_for_leaf,
                &has_explicit_waiting_path,
            );
            if let Some(ref f) = chain_finding {
                findings.push(f.clone());
            }
        }

        if issue.status == "in_review"
            && chain_finding.is_none()
            && !unresolved_blockers.contains(&issue.id)
        {
            let path = vec![issue];
            if let Some(r) = review_finding(issue, issue, &path) {
                findings.push(r);
            }
        }
    }

    findings
}
