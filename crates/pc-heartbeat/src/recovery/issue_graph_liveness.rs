//! Issue graph liveness classifier（纯函数）。
//!
//! 对齐 Node `services/recovery/issue-graph-liveness.ts`：
//! - 类型 `IssueLivenessSeverity` / `IssueLivenessState` / `IssueLivenessIssueInput` /
//!   `IssueLivenessRelationInput` / `IssueLivenessAgentInput` /
//!   `IssueLivenessExecutionPathInput` / `IssueLivenessWaitingPathInput` /
//!   `IssueLivenessDependencyPathEntry` / `IssueLivenessOwnerCandidateReason` /
//!   `IssueLivenessOwnerCandidate` / `IssueLivenessFinding` /
//!   `IssueGraphLivenessInput`
//! - 辅助 `issueLabel` / `pathEntry` / `isInvokableAgent` /
//!   `hasActiveExecutionPath` / `hasWaitingPath` /
//!   `readRecord` / `readPositiveInteger` / `readDateMs` /
//!   `monitorFromIssue` / `hasScheduledMonitor` /
//!   `readPrincipalAgentId` / `principalIsResolvableUser`
//! - owner 候选 `addOwnerCandidate` / `addAgentChainCandidates` /
//!   `orderedInvokableAgents` / `ownerCandidatesForRecoveryIssue`
//! - 主函数 `classifyIssueGraphLiveness(input)` → `IssueLivenessFinding[]`
//!
//! 设计：
//! - 纯函数，无副作用，方便单测
//! - 用 `serde_json::Value` 表示动态 policy / state / principal
//! - 输入/输出字段命名与 Node 1:1（camelCase via serde），便于跨语言日志对照
//! - 用 `BTreeMap<Uuid, T>` 与 `Vec` 协作（不是 `HashMap`）方便可重现迭代
//! - `Uuid` 对应 Node 字符串 id（接受 hex 字符串解析为 Uuid）
//!
//! 调用方（典型为 heartbeat 周期）：
//! 1. 从 `pc-repos` 查出 issues / relations / agents / active runs / wake requests /
//!    pending interactions / approvals / open recovery issues
//! 2. 构造 `IssueGraphLivenessInput`
//! 3. 调用 `classify_issue_graph_liveness(&input)` 得到 findings
//! 4. 对每个 finding 用 `build_issue_graph_liveness_incident_key(...)` 计算 idempotency key
//! 5. 写回 `issue_graph_liveness_incident_keys` 表（pc-repos::recovery::*)

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use pc_repos::agent_invokability::{evaluate_agent_invokability, AgentOrgRow};

use super::origins::{build_issue_graph_liveness_incident_key, IncidentKeyInput};

// ============================================================================
// Constants
// ============================================================================

/// Liveness 严重度（与 Node `IssueLivenessSeverity` 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueLivenessSeverity {
    Warning,
    Critical,
}

impl IssueLivenessSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// Liveness 状态分类（与 Node `IssueLivenessState` 1:1）。
///
/// 字符串字面量与 Node 完全一致，跨语言日志可读。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IssueLivenessState {
    #[serde(rename = "blocked_by_unassigned_issue")]
    BlockedByUnassignedIssue,
    #[serde(rename = "blocked_by_assigned_backlog_issue")]
    BlockedByAssignedBacklogIssue,
    #[serde(rename = "blocked_by_uninvokable_assignee")]
    BlockedByUninvokableAssignee,
    #[serde(rename = "blocked_by_cancelled_issue")]
    BlockedByCancelledIssue,
    #[serde(rename = "invalid_review_participant")]
    InvalidReviewParticipant,
    #[serde(rename = "in_review_without_action_path")]
    InReviewWithoutActionPath,
}

impl IssueLivenessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BlockedByUnassignedIssue => "blocked_by_unassigned_issue",
            Self::BlockedByAssignedBacklogIssue => "blocked_by_assigned_backlog_issue",
            Self::BlockedByUninvokableAssignee => "blocked_by_uninvokable_assignee",
            Self::BlockedByCancelledIssue => "blocked_by_cancelled_issue",
            Self::InvalidReviewParticipant => "invalid_review_participant",
            Self::InReviewWithoutActionPath => "in_review_without_action_path",
        }
    }

    /// 默认严重度：阻塞类 + review 类 = critical。
    pub fn default_severity(self) -> IssueLivenessSeverity {
        match self {
            Self::BlockedByUnassignedIssue
            | Self::BlockedByAssignedBacklogIssue
            | Self::BlockedByUninvokableAssignee
            | Self::BlockedByCancelledIssue => IssueLivenessSeverity::Critical,
            Self::InvalidReviewParticipant | Self::InReviewWithoutActionPath => {
                IssueLivenessSeverity::Critical
            }
        }
    }
}

/// Owner 候选来源（与 Node `IssueLivenessOwnerCandidateReason` 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueLivenessOwnerCandidateReason {
    StalledBlockerAssignee,
    AssigneeReportingChain,
    CreatorReportingChain,
    RootAgent,
    OrderedInvokableFallback,
}

impl IssueLivenessOwnerCandidateReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StalledBlockerAssignee => "stalled_blocker_assignee",
            Self::AssigneeReportingChain => "assignee_reporting_chain",
            Self::CreatorReportingChain => "creator_reporting_chain",
            Self::RootAgent => "root_agent",
            Self::OrderedInvokableFallback => "ordered_invokable_fallback",
        }
    }
}

// ============================================================================
// Input types
// ============================================================================

/// Issue 输入（最小列集合）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessIssueInput {
    pub id: Uuid,
    pub company_id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub goal_id: Option<Uuid>,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub assignee_agent_id: Option<Uuid>,
    #[serde(default)]
    pub assignee_user_id: Option<String>,
    #[serde(default)]
    pub created_by_agent_id: Option<Uuid>,
    #[serde(default)]
    pub created_by_user_id: Option<String>,
    #[serde(default)]
    pub execution_policy: Option<Value>,
    #[serde(default)]
    pub execution_state: Option<Value>,
    #[serde(default)]
    pub monitor_next_check_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub monitor_attempt_count: Option<i32>,
}

/// 阻塞关系输入（blocker → blocked）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessRelationInput {
    pub company_id: Uuid,
    pub blocker_issue_id: Uuid,
    pub blocked_issue_id: Uuid,
}

/// Agent 输入（最小列集合）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessAgentInput {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub status: String,
    #[serde(default)]
    pub reports_to: Option<Uuid>,
}

/// Active run / queued wake 输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessExecutionPathInput {
    pub company_id: Uuid,
    #[serde(default)]
    pub issue_id: Option<Uuid>,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Pending interaction / approval / open recovery 输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessWaitingPathInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    #[serde(default)]
    pub status: Option<String>,
}

/// 依赖路径节点（用于 finding 输出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessDependencyPathEntry {
    pub issue_id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
}

/// Owner 候选输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessOwnerCandidate {
    pub agent_id: Uuid,
    pub reason: IssueLivenessOwnerCandidateReason,
    pub source_issue_id: Uuid,
}

/// Finding 输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessFinding {
    pub company_id: Uuid,
    pub incident_key: String,
    pub state: IssueLivenessState,
    pub severity: IssueLivenessSeverity,
    pub source_issue_id: Uuid,
    pub source_issue_label: String,
    pub reason: String,
    pub dependency_path: Vec<IssueLivenessDependencyPathEntry>,
    #[serde(default)]
    pub recovery_issue_id: Option<Uuid>,
    #[serde(default)]
    pub blocker_issue_id: Option<Uuid>,
    #[serde(default)]
    pub participant_agent_id: Option<Uuid>,
    pub recommended_owner_agent_id: Option<Uuid>,
    pub recommended_owner_candidate_agent_ids: Vec<Uuid>,
    pub recommended_owner_candidates: Vec<IssueLivenessOwnerCandidate>,
    pub recommended_action: String,
}

/// 顶层输入（与 Node `IssueGraphLivenessInput` 1:1）。
#[derive(Debug, Clone, Default)]
pub struct IssueGraphLivenessInput {
    pub issues: Vec<IssueLivenessIssueInput>,
    pub relations: Vec<IssueLivenessRelationInput>,
    pub agents: Vec<IssueLivenessAgentInput>,
    pub active_runs: Vec<IssueLivenessExecutionPathInput>,
    pub queued_wake_requests: Vec<IssueLivenessExecutionPathInput>,
    pub pending_interactions: Vec<IssueLivenessWaitingPathInput>,
    pub pending_approvals: Vec<IssueLivenessWaitingPathInput>,
    pub open_recovery_issues: Vec<IssueLivenessWaitingPathInput>,
    pub now: DateTime<Utc>,
}

// ============================================================================
// Helpers (private)
// ============================================================================

fn issue_label(issue: &IssueLivenessIssueInput) -> String {
    issue
        .identifier
        .clone()
        .unwrap_or_else(|| format!("issue {}", issue.id))
}

fn path_entry(issue: &IssueLivenessIssueInput) -> IssueLivenessDependencyPathEntry {
    IssueLivenessDependencyPathEntry {
        issue_id: issue.id,
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        status: issue.status.clone(),
    }
}

fn to_org_row(agent: &IssueLivenessAgentInput) -> AgentOrgRow {
    AgentOrgRow {
        id: agent.id,
        company_id: agent.company_id,
        name: agent.name.clone(),
        reports_to: agent.reports_to,
        status: agent.status.clone(),
    }
}

/// 评估 agent invokability（与 Node `isAgentInvokable({ agent, agents })` 1:1）。
///
/// Node 调用 `getAgentWorkEligibility({ agent, agents })` 计算 invokability，然后取
/// `invokable` 字段。我们直接复用 `pc_repos::agent_invokability::evaluate_agent_invokability`。
fn is_invokable_agent(agent: Option<&IssueLivenessAgentInput>, org_agents: &[AgentOrgRow]) -> bool {
    let Some(agent) = agent else {
        return false;
    };
    let org_row = to_org_row(agent);
    evaluate_agent_invokability(Some(&org_row), org_agents).is_invokable()
}

fn has_active_execution_path(
    company_id: Uuid,
    issue_id: Uuid,
    active_runs: &[IssueLivenessExecutionPathInput],
    queued_wake_requests: &[IssueLivenessExecutionPathInput],
) -> bool {
    active_runs
        .iter()
        .chain(queued_wake_requests.iter())
        .any(|entry| entry.company_id == company_id && entry.issue_id == Some(issue_id))
}

fn has_waiting_path(
    company_id: Uuid,
    issue_id: Uuid,
    waiting_paths: &[IssueLivenessWaitingPathInput],
) -> bool {
    waiting_paths
        .iter()
        .any(|entry| entry.company_id == company_id && entry.issue_id == issue_id)
}

fn read_record(value: Option<&Value>) -> Option<Map<String, Value>> {
    let value = value?;
    if let Value::Object(map) = value {
        Some(map.clone())
    } else {
        None
    }
}

fn read_positive_integer(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n.as_i64().filter(|v| *v > 0),
        _ => None,
    }
}

fn read_date_ms(value: Option<&DateTime<Utc>>) -> Option<i64> {
    value.map(|dt| dt.timestamp_millis())
}

fn monitor_from_issue(
    issue: &IssueLivenessIssueInput,
) -> (Option<Map<String, Value>>, Option<Map<String, Value>>) {
    let policy_monitor = read_record(
        read_record(issue.execution_policy.as_ref())
            .as_ref()
            .and_then(|p| p.get("monitor")),
    );
    let state_monitor = read_record(
        read_record(issue.execution_state.as_ref())
            .as_ref()
            .and_then(|s| s.get("monitor")),
    );
    (policy_monitor, state_monitor)
}

/// 判断 issue 是否有已 schedule、未 timeout、未达到 maxAttempts 的 monitor。
///
/// 字段语义（来自 Node `hasScheduledMonitor`）：
/// - `monitorNextCheckAt > now` 才算已 schedule
/// - `monitor.timeoutAt` 优先取 policy 后 state；<= now 则已超时
/// - `monitor.maxAttempts` 同样优先 policy 后 state；`attemptCount >= maxAttempts` 已耗尽
/// - `attemptCount` 取 `issue.monitorAttemptCount`，fallback 到 state monitor
fn has_scheduled_monitor(issue: &IssueLivenessIssueInput, now_ms: i64) -> bool {
    let next_check_at_ms = match issue.monitor_next_check_at {
        Some(dt) => dt.timestamp_millis(),
        None => return false,
    };
    if next_check_at_ms <= now_ms {
        return false;
    }
    let (policy_monitor, state_monitor) = monitor_from_issue(issue);
    let timeout_at_ms = read_date_ms_from_value(
        policy_monitor
            .as_ref()
            .and_then(|m| m.get("timeoutAt").cloned())
            .or_else(|| {
                state_monitor
                    .as_ref()
                    .and_then(|m| m.get("timeoutAt").cloned())
            })
            .as_ref(),
    );
    if let Some(timeout) = timeout_at_ms {
        if timeout <= now_ms {
            return false;
        }
    }
    let max_attempts = read_positive_integer(
        policy_monitor
            .as_ref()
            .and_then(|m| m.get("maxAttempts").cloned())
            .or_else(|| {
                state_monitor
                    .as_ref()
                    .and_then(|m| m.get("maxAttempts").cloned())
            })
            .as_ref(),
    );
    let state_attempt_count = read_positive_integer(
        state_monitor
            .as_ref()
            .and_then(|m| m.get("attemptCount").cloned())
            .as_ref(),
    )
    .unwrap_or(0);
    let attempt_count = issue
        .monitor_attempt_count
        .map(|v| v as i64)
        .unwrap_or(state_attempt_count);
    if let Some(max) = max_attempts {
        if attempt_count >= max {
            return false;
        }
    }
    true
}

fn read_date_ms_from_value(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s.as_str())
            .ok()
            .map(|dt| dt.timestamp_millis()),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

/// 从 `executionState.currentParticipant` 读出 agentId（如果 principal 是 agent）。
fn read_principal_agent_id(principal: Option<&Value>) -> Option<Uuid> {
    let value = principal?;
    let obj = value.as_object()?;
    if obj.get("type").and_then(|v| v.as_str()) != Some("agent") {
        return None;
    }
    let agent_id_str = obj.get("agentId").and_then(|v| v.as_str())?;
    Uuid::parse_str(agent_id_str).ok()
}

/// 从 `executionState.currentParticipant` 判断 principal 是否是合法 user。
fn principal_is_resolvable_user(principal: Option<&Value>) -> bool {
    let Some(value) = principal else {
        return false;
    };
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("type").and_then(|v| v.as_str()) == Some("user")
        && obj
            .get("userId")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
}

// ============================================================================
// Owner candidate helpers
// ============================================================================

#[derive(Debug)]
struct AddOwnerCandidateInput<'a> {
    candidates: &'a mut Vec<IssueLivenessOwnerCandidate>,
    seen: &'a mut BTreeSet<Uuid>,
    org_agents: &'a [AgentOrgRow],
    agents_by_id: &'a BTreeMap<Uuid, IssueLivenessAgentInput>,
    company_id: Uuid,
    agent_id: Option<Uuid>,
    reason: IssueLivenessOwnerCandidateReason,
    source_issue_id: Uuid,
}

fn add_owner_candidate(mut input: AddOwnerCandidateInput<'_>) {
    let Some(agent_id) = input.agent_id else {
        return;
    };
    if input.seen.contains(&agent_id) {
        return;
    }
    let Some(agent) = input.agents_by_id.get(&agent_id) else {
        return;
    };
    if agent.company_id != input.company_id {
        return;
    }
    let Some(agent_for_check) = input.agents_by_id.get(&agent_id) else {
        return;
    };
    if !is_invokable_agent(Some(agent_for_check), input.org_agents) {
        return;
    }
    input.seen.insert(agent_id);
    input.candidates.push(IssueLivenessOwnerCandidate {
        agent_id,
        reason: input.reason,
        source_issue_id: input.source_issue_id,
    });
}

/// 沿 agent 的 reportsTo 链向上走，把每一层 invokable manager 加入候选。
fn add_agent_chain_candidates(
    candidates: &mut Vec<IssueLivenessOwnerCandidate>,
    seen: &mut BTreeSet<Uuid>,
    start_agent_id: Option<Uuid>,
    agents_by_id: &BTreeMap<Uuid, IssueLivenessAgentInput>,
    org_agents: &[AgentOrgRow],
    company_id: Uuid,
    reason: IssueLivenessOwnerCandidateReason,
    source_issue_id: Uuid,
) {
    let Some(start) = start_agent_id else {
        return;
    };
    let mut current = match agents_by_id.get(&start) {
        Some(a) => a.clone(),
        None => return,
    };
    let mut chain_seen: BTreeSet<Uuid> = BTreeSet::new();
    while let Some(reports_to) = current.reports_to {
        if chain_seen.contains(&reports_to) {
            break;
        }
        chain_seen.insert(reports_to);
        let Some(manager) = agents_by_id.get(&reports_to).cloned() else {
            break;
        };
        if manager.company_id != company_id {
            break;
        }
        add_owner_candidate(AddOwnerCandidateInput {
            candidates,
            seen,
            org_agents,
            agents_by_id,
            company_id,
            agent_id: Some(manager.id),
            reason,
            source_issue_id,
        });
        current = manager;
    }
}

/// 按 id 升序列出 company 内所有 invokable agents。
fn ordered_invokable_agents(
    agents: &[IssueLivenessAgentInput],
    agents_by_id: &BTreeMap<Uuid, IssueLivenessAgentInput>,
    org_agents: &[AgentOrgRow],
    company_id: Uuid,
) -> Vec<IssueLivenessAgentInput> {
    let mut out: Vec<IssueLivenessAgentInput> = agents
        .iter()
        .filter(|a| a.company_id == company_id && is_invokable_agent(Some(a), org_agents))
        .cloned()
        .collect();
    out.sort_by(|left, right| left.id.to_string().cmp(&right.id.to_string()));
    out
}

#[derive(Debug, Clone, Copy, Default)]
struct OwnerCandidatesOptions {
    include_stalled_assignee: bool,
}

fn owner_candidates_for_recovery_issue(
    issue: &IssueLivenessIssueInput,
    agents: &[IssueLivenessAgentInput],
    agents_by_id: &BTreeMap<Uuid, IssueLivenessAgentInput>,
    org_agents: &[AgentOrgRow],
    options: OwnerCandidatesOptions,
) -> Vec<IssueLivenessOwnerCandidate> {
    let mut candidates: Vec<IssueLivenessOwnerCandidate> = Vec::new();
    let mut seen: BTreeSet<Uuid> = BTreeSet::new();

    if options.include_stalled_assignee && issue.status != "cancelled" && issue.status != "done" {
        add_owner_candidate(AddOwnerCandidateInput {
            candidates: &mut candidates,
            seen: &mut seen,
            org_agents,
            agents_by_id,
            company_id: issue.company_id,
            agent_id: issue.assignee_agent_id,
            reason: IssueLivenessOwnerCandidateReason::StalledBlockerAssignee,
            source_issue_id: issue.id,
        });
    }

    add_agent_chain_candidates(
        &mut candidates,
        &mut seen,
        issue.assignee_agent_id,
        agents_by_id,
        org_agents,
        issue.company_id,
        IssueLivenessOwnerCandidateReason::AssigneeReportingChain,
        issue.id,
    );
    add_agent_chain_candidates(
        &mut candidates,
        &mut seen,
        issue.created_by_agent_id,
        agents_by_id,
        org_agents,
        issue.company_id,
        IssueLivenessOwnerCandidateReason::CreatorReportingChain,
        issue.id,
    );

    let invokable = ordered_invokable_agents(agents, agents_by_id, org_agents, issue.company_id);
    for agent in &invokable {
        if agent.reports_to.is_none() {
            add_owner_candidate(AddOwnerCandidateInput {
                candidates: &mut candidates,
                seen: &mut seen,
                org_agents,
                agents_by_id,
                company_id: issue.company_id,
                agent_id: Some(agent.id),
                reason: IssueLivenessOwnerCandidateReason::RootAgent,
                source_issue_id: issue.id,
            });
        }
    }
    for agent in &invokable {
        add_owner_candidate(AddOwnerCandidateInput {
            candidates: &mut candidates,
            seen: &mut seen,
            org_agents,
            agents_by_id,
            company_id: issue.company_id,
            agent_id: Some(agent.id),
            reason: IssueLivenessOwnerCandidateReason::OrderedInvokableFallback,
            source_issue_id: issue.id,
        });
    }

    candidates
}

// ============================================================================
// Core classifier
// ============================================================================

#[derive(Debug, Clone)]
struct FindingBuilder {
    issue: IssueLivenessIssueInput,
    state: IssueLivenessState,
    reason: String,
    dependency_path: Vec<IssueLivenessDependencyPathEntry>,
    recovery_issue: Option<IssueLivenessIssueInput>,
    blocker_issue_id: Option<Uuid>,
    participant_agent_id: Option<Uuid>,
    recommended_owner_candidates: Vec<IssueLivenessOwnerCandidate>,
    recommended_action: String,
}

impl FindingBuilder {
    fn build(self) -> IssueLivenessFinding {
        let blocker_issue_id = self
            .blocker_issue_id
            .or_else(|| self.recovery_issue.as_ref().map(|i| i.id));
        let participant_agent_id = self.participant_agent_id;
        let recommended_owner_candidate_agent_ids: Vec<Uuid> = self
            .recommended_owner_candidates
            .iter()
            .map(|c| c.agent_id)
            .collect();
        let recommended_owner_agent_id = recommended_owner_candidate_agent_ids.first().copied();
        let incident_key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
            company_id: &self.issue.company_id.to_string(),
            issue_id: &self.issue.id.to_string(),
            state: self.state.as_str(),
            blocker_issue_id: blocker_issue_id
                .as_ref()
                .map(|id| id.to_string())
                .as_deref(),
            participant_agent_id: participant_agent_id
                .as_ref()
                .map(|id| id.to_string())
                .as_deref(),
        });
        IssueLivenessFinding {
            company_id: self.issue.company_id,
            incident_key,
            state: self.state,
            severity: self.state.default_severity(),
            source_issue_id: self.issue.id,
            source_issue_label: issue_label(&self.issue),
            reason: self.reason,
            dependency_path: self.dependency_path,
            recovery_issue_id: self.recovery_issue.map(|i| i.id),
            blocker_issue_id,
            participant_agent_id,
            recommended_owner_agent_id,
            recommended_owner_candidate_agent_ids,
            recommended_owner_candidates: self.recommended_owner_candidates,
            recommended_action: self.recommended_action,
        }
    }
}

/// 主函数：分类所有 issue 的 graph liveness findings。
///
/// 对齐 Node `classifyIssueGraphLiveness(input)` → `IssueLivenessFinding[]`。
///
/// 算法概要：
/// 1. 把 issues/agents 入索引为 `BTreeMap<Uuid, T>`
/// 2. 把 relations 按 `blockedIssueId` 分组
/// 3. 计算每个 issue 的 unresolved blockers（status ∈ {todo, in_progress, blocked,
///    in_review, backlog} 且 blocker.status ∉ {done, cancelled}）
/// 4. 对每个 issue：
///    a. 若 status == "blocked" 或有 unresolved blocker edge → 走 blocked chain finding
///    b. 若 status == "in_review" → 走 review finding
/// 5. blocked chain finding 通过 DFS 找到第一个 leaf finding（unassigned /
///    cancelled / assigned-backlog / uninvokable-assignee / invalid-review /
///    in-review-without-action-path）
pub fn classify_issue_graph_liveness(input: &IssueGraphLivenessInput) -> Vec<IssueLivenessFinding> {
    let now_ms = input.now.timestamp_millis();
    let issues_by_id: BTreeMap<Uuid, IssueLivenessIssueInput> =
        input.issues.iter().map(|i| (i.id, i.clone())).collect();
    let agents_by_id: BTreeMap<Uuid, IssueLivenessAgentInput> =
        input.agents.iter().map(|a| (a.id, a.clone())).collect();
    let mut agents_by_id_for_inv: Vec<AgentOrgRow> = input
        .agents
        .iter()
        .filter(|a| {
            // AgentOrgRow 不含 company_id 用于 inv 评估，但 evaluate_agent_invokability
            // 会校验 agent.companyId 与 chain agents 一致；这里直接传整个 input.agents
            // 给 evaluator 即可（evaluator 内部只看 chain 完整性）。
            true
        })
        .map(to_org_row)
        .collect();
    agents_by_id_for_inv.sort_by(|a, b| a.id.cmp(&b.id));

    // 1) blocker 分组
    let mut blockers_by_blocked: BTreeMap<Uuid, Vec<IssueLivenessRelationInput>> = BTreeMap::new();
    let mut unresolved_blockers: BTreeSet<Uuid> = BTreeSet::new();
    for relation in &input.relations {
        blockers_by_blocked
            .entry(relation.blocked_issue_id)
            .or_default()
            .push(relation.clone());
        let blocker = issues_by_id.get(&relation.blocker_issue_id);
        let blocked = issues_by_id.get(&relation.blocked_issue_id);
        if let (Some(blocker), Some(blocked)) = (blocker, blocked) {
            if blocker.company_id == relation.company_id
                && blocked.company_id == relation.company_id
                && blocker.status != "done"
                && blocker.status != "cancelled"
                && blocked.status == "blocked"
            {
                unresolved_blockers.insert(blocker.id);
            }
        }
    }

    // 2) 按 blocker label 排序（与 Node 行为一致）
    for relations in blockers_by_blocked.values_mut() {
        relations.sort_by(|left, right| {
            let left_label = issues_by_id
                .get(&left.blocker_issue_id)
                .map(issue_label)
                .unwrap_or_else(|| left.blocker_issue_id.to_string());
            let right_label = issues_by_id
                .get(&right.blocker_issue_id)
                .map(issue_label)
                .unwrap_or_else(|| right.blocker_issue_id.to_string());
            left_label.cmp(&right_label)
        });
    }

    // 3) 主循环
    let mut findings: Vec<IssueLivenessFinding> = Vec::new();
    let active_runs = &input.active_runs;
    let queued_wake_requests = &input.queued_wake_requests;
    let pending_interactions = &input.pending_interactions;
    let pending_approvals = &input.pending_approvals;
    let open_recovery_issues = &input.open_recovery_issues;

    // 闭包：判定 issue 是否已有显式 waiting path（assigneeUserId / monitor / run /
    // wake / pending interaction / approval / open recovery issue）
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
            active_runs,
            queued_wake_requests,
        ) {
            return true;
        }
        if has_waiting_path(issue.company_id, issue.id, pending_interactions) {
            return true;
        }
        if has_waiting_path(issue.company_id, issue.id, pending_approvals) {
            return true;
        }
        if has_waiting_path(issue.company_id, issue.id, open_recovery_issues) {
            return true;
        }
        false
    };

    // 闭包：构造 review finding（issue 在 in_review 状态下的两类异常）
    let review_finding = |source: &IssueLivenessIssueInput,
                          review_issue: &IssueLivenessIssueInput,
                          dependency_path: Vec<IssueLivenessDependencyPathEntry>|
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
            &agents_by_id_for_inv,
            OwnerCandidatesOptions {
                include_stalled_assignee: true,
            },
        );
        let current_participant = review_issue
            .execution_state
            .as_ref()
            .and_then(|es| es.get("currentParticipant"))
            .cloned();
        let participant_agent_id = read_principal_agent_id(current_participant.as_ref());
        if let Some(pid) = participant_agent_id {
            let participant_agent = agents_by_id.get(&pid).cloned();
            let invokable = is_invokable_agent(participant_agent.as_ref(), &agents_by_id_for_inv);
            let same_company = participant_agent
                .as_ref()
                .map(|a| a.company_id == review_issue.company_id)
                .unwrap_or(false);
            if invokable && same_company {
                return None;
            }
            let reason_text = match participant_agent.as_ref() {
                Some(a) => format!(
                    "{} is in review, but current participant agent is {}.",
                    issue_label(review_issue),
                    a.status
                ),
                None => format!(
                    "{} is in review, but current participant agent cannot be resolved.",
                    issue_label(review_issue)
                ),
            };
            return Some(
                FindingBuilder {
                    issue: source.clone(),
                    state: IssueLivenessState::InvalidReviewParticipant,
                    reason: reason_text,
                    dependency_path,
                    recovery_issue: Some(review_issue.clone()),
                    blocker_issue_id: Some(review_issue.id),
                    participant_agent_id: Some(pid),
                    recommended_owner_candidates: owner_candidates,
                    recommended_action: format!(
                        "Repair {}'s review participant or return the issue to an active assignee with a clear change request.",
                        issue_label(review_issue)
                    ),
                }
                .build(),
            );
        }
        if principal_is_resolvable_user(current_participant.as_ref()) {
            return None;
        }
        if review_issue.execution_state.is_some() {
            return Some(
                FindingBuilder {
                    issue: source.clone(),
                    state: IssueLivenessState::InvalidReviewParticipant,
                    reason: format!(
                        "{} is in review, but its current participant cannot be resolved.",
                        issue_label(review_issue)
                    ),
                    dependency_path,
                    recovery_issue: Some(review_issue.clone()),
                    blocker_issue_id: Some(review_issue.id),
                    participant_agent_id: None,
                    recommended_owner_candidates: owner_candidates,
                    recommended_action: format!(
                        "Repair {}'s review participant or return the issue to an active assignee with a clear change request.",
                        issue_label(review_issue)
                    ),
                }
                .build(),
            );
        }
        if review_issue.assignee_agent_id.is_none() || review_issue.assignee_user_id.is_some() {
            return None;
        }
        Some(
            FindingBuilder {
                issue: source.clone(),
                state: IssueLivenessState::InReviewWithoutActionPath,
                reason: format!(
                    "{} is in review with an agent assignee but no participant, interaction, approval, user owner, wake, active run, or recovery issue owning the next action.",
                    issue_label(review_issue)
                ),
                dependency_path,
                recovery_issue: Some(review_issue.clone()),
                blocker_issue_id: Some(review_issue.id),
                participant_agent_id: None,
                recommended_owner_candidates: owner_candidates,
                recommended_action: format!(
                    "Review {} and make the next action explicit: add a reviewer/interaction, return it to active work with a change request, mark it done if accepted, or open a bounded recovery issue.",
                    issue_label(review_issue)
                ),
            }
            .build(),
        )
    };

    // 闭包：构造 leaf blocker finding
    let blocked_finding_for_leaf = |source: &IssueLivenessIssueInput,
                                    blocker: &IssueLivenessIssueInput,
                                    dependency_path: Vec<IssueLivenessDependencyPathEntry>|
     -> Option<IssueLivenessFinding> {
        let owner_candidates = owner_candidates_for_recovery_issue(
            blocker,
            &input.agents,
            &agents_by_id,
            &agents_by_id_for_inv,
            OwnerCandidatesOptions {
                include_stalled_assignee: true,
            },
        );
        if blocker.status == "cancelled" {
            return Some(
                FindingBuilder {
                    issue: source.clone(),
                    state: IssueLivenessState::BlockedByCancelledIssue,
                    reason: format!(
                        "{} is still blocked by cancelled issue {}.",
                        issue_label(source),
                        issue_label(blocker)
                    ),
                    dependency_path,
                    recovery_issue: Some(blocker.clone()),
                    blocker_issue_id: Some(blocker.id),
                    participant_agent_id: None,
                    recommended_owner_candidates: owner_candidates,
                    recommended_action: format!(
                        "Inspect {} and either remove it from {}'s blockers or replace it with an actionable unblock issue.",
                        issue_label(blocker),
                        issue_label(source)
                    ),
                }
                .build(),
            );
        }
        if has_explicit_waiting_path(blocker) {
            return None;
        }
        if blocker.status == "in_review" {
            return review_finding(source, blocker, dependency_path);
        }
        if blocker.status == "backlog" && blocker.assignee_agent_id.is_some() {
            return Some(
                FindingBuilder {
                    issue: source.clone(),
                    state: IssueLivenessState::BlockedByAssignedBacklogIssue,
                    reason: format!(
                        "{} is blocked by assigned backlog issue {} with no wake, active run, human owner, interaction, approval, monitor, or recovery issue owning the next action.",
                        issue_label(source),
                        issue_label(blocker)
                    ),
                    dependency_path,
                    recovery_issue: Some(blocker.clone()),
                    blocker_issue_id: Some(blocker.id),
                    participant_agent_id: None,
                    recommended_owner_candidates: owner_candidates,
                    recommended_action: format!(
                        "Review {} and either move it to todo so the assignee wakes, assign a human owner or interaction if it is intentionally parked, or remove it from {}'s blockers if it is no longer required.",
                        issue_label(blocker),
                        issue_label(source)
                    ),
                }
                .build(),
            );
        }
        if blocker.assignee_agent_id.is_none() && blocker.assignee_user_id.is_none() {
            return Some(
                FindingBuilder {
                    issue: source.clone(),
                    state: IssueLivenessState::BlockedByUnassignedIssue,
                    reason: format!(
                        "{} is blocked by unassigned issue {} with no user owner.",
                        issue_label(source),
                        issue_label(blocker)
                    ),
                    dependency_path,
                    recovery_issue: Some(blocker.clone()),
                    blocker_issue_id: Some(blocker.id),
                    participant_agent_id: None,
                    recommended_owner_candidates: owner_candidates,
                    recommended_action: format!(
                        "Assign {} to an owner who can complete it, or remove it from {}'s blockers if it is no longer required.",
                        issue_label(blocker),
                        issue_label(source)
                    ),
                }
                .build(),
            );
        }
        if blocker.assignee_agent_id.is_none() {
            return None;
        }
        let blocker_agent = blocker
            .assignee_agent_id
            .and_then(|aid| agents_by_id.get(&aid).cloned());
        let blocker_invokable = is_invokable_agent(blocker_agent.as_ref(), &agents_by_id_for_inv);
        let blocker_same_company = blocker_agent
            .as_ref()
            .map(|a| a.company_id == source.company_id)
            .unwrap_or(false);
        if blocker_agent.is_none() || !blocker_same_company || !blocker_invokable {
            let reason_text = match blocker_agent.as_ref() {
                Some(a) => format!(
                    "{} is blocked by {}, but its assignee is {}.",
                    issue_label(source),
                    issue_label(blocker),
                    a.status
                ),
                None => format!(
                    "{} is blocked by {}, but its assignee no longer exists.",
                    issue_label(source),
                    issue_label(blocker)
                ),
            };
            return Some(
                FindingBuilder {
                    issue: source.clone(),
                    state: IssueLivenessState::BlockedByUninvokableAssignee,
                    reason: reason_text,
                    dependency_path,
                    recovery_issue: Some(blocker.clone()),
                    blocker_issue_id: Some(blocker.id),
                    participant_agent_id: None,
                    recommended_owner_candidates: owner_candidates,
                    recommended_action: format!(
                        "Review {} and assign it to an active owner or replace the blocker with an actionable issue.",
                        issue_label(blocker)
                    ),
                }
                .build(),
            );
        }
        None
    };

    // DFS：找 blocked chain 的首个 leaf finding
    fn first_blocked_chain<'a, F, G>(
        source: &'a IssueLivenessIssueInput,
        current: &'a IssueLivenessIssueInput,
        dependency_path: Vec<IssueLivenessDependencyPathEntry>,
        seen: &mut BTreeSet<Uuid>,
        issues_by_id: &'a BTreeMap<Uuid, IssueLivenessIssueInput>,
        blockers_by_blocked: &'a BTreeMap<Uuid, Vec<IssueLivenessRelationInput>>,
        blocked_finding_for_leaf: &F,
        review_finding: &G,
        has_explicit_waiting_path: &dyn Fn(&IssueLivenessIssueInput) -> bool,
    ) -> Option<IssueLivenessFinding>
    where
        F: Fn(
            &IssueLivenessIssueInput,
            &IssueLivenessIssueInput,
            Vec<IssueLivenessDependencyPathEntry>,
        ) -> Option<IssueLivenessFinding>,
        G: Fn(
            &IssueLivenessIssueInput,
            &IssueLivenessIssueInput,
            Vec<IssueLivenessDependencyPathEntry>,
        ) -> Option<IssueLivenessFinding>,
    {
        if !seen.insert(current.id) {
            return None;
        }
        let relations = blockers_by_blocked
            .get(&current.id)
            .cloned()
            .unwrap_or_default();
        for relation in relations {
            if relation.company_id != current.company_id || relation.company_id != source.company_id
            {
                continue;
            }
            let Some(blocker) = issues_by_id.get(&relation.blocker_issue_id) else {
                continue;
            };
            if blocker.company_id != source.company_id || blocker.status == "done" {
                continue;
            }
            let mut path = dependency_path.clone();
            path.push(path_entry(blocker));
            if blocker.status == "blocked" {
                let mut new_seen = seen.clone();
                if let Some(nested) = first_blocked_chain(
                    source,
                    blocker,
                    path.clone(),
                    &mut new_seen,
                    issues_by_id,
                    blockers_by_blocked,
                    blocked_finding_for_leaf,
                    review_finding,
                    has_explicit_waiting_path,
                ) {
                    return Some(nested);
                }
                if has_explicit_waiting_path(blocker) {
                    continue;
                }
            }
            if let Some(leaf) = blocked_finding_for_leaf(source, blocker, path.clone()) {
                return Some(leaf);
            }
            // 兜底：当前 leaf 不命中但 blocker 仍在 in_review 中（无 explicit waiting path）
            if blocker.status == "in_review" {
                if let Some(review) = review_finding(source, blocker, path) {
                    return Some(review);
                }
            }
        }
        None
    }

    // 主循环
    for issue in &input.issues {
        let has_unresolved_blocker_edge = (blockers_by_blocked
            .get(&issue.id)
            .cloned()
            .unwrap_or_default())
        .iter()
        .any(|relation| {
            if relation.company_id != issue.company_id {
                return false;
            }
            let Some(blocker) = issues_by_id.get(&relation.blocker_issue_id) else {
                return false;
            };
            blocker.company_id == issue.company_id && blocker.status != "done"
        });

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
            let mut seen: BTreeSet<Uuid> = BTreeSet::new();
            chain_finding = first_blocked_chain(
                issue,
                issue,
                vec![path_entry(issue)],
                &mut seen,
                &issues_by_id,
                &blockers_by_blocked,
                &blocked_finding_for_leaf,
                &review_finding,
                &has_explicit_waiting_path,
            );
            if let Some(f) = chain_finding.take() {
                findings.push(f);
            }
        }

        if issue.status == "in_review"
            && chain_finding.is_none()
            && !unresolved_blockers.contains(&issue.id)
        {
            if let Some(review) = review_finding(issue, issue, vec![path_entry(issue)]) {
                findings.push(review);
            }
        }
    }

    findings
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn company_id() -> Uuid {
        Uuid::nil()
    }

    fn fixed_now() -> DateTime<Utc> {
        chrono::Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap()
    }

    fn make_issue(
        id: Uuid,
        identifier: &str,
        title: &str,
        status: &str,
        assignee_agent_id: Option<Uuid>,
    ) -> IssueLivenessIssueInput {
        IssueLivenessIssueInput {
            id,
            company_id: company_id(),
            identifier: Some(identifier.to_string()),
            title: title.to_string(),
            status: status.to_string(),
            project_id: None,
            goal_id: None,
            parent_id: None,
            assignee_agent_id,
            assignee_user_id: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            execution_policy: None,
            execution_state: None,
            monitor_next_check_at: None,
            monitor_attempt_count: None,
        }
    }

    fn make_agent(
        id: Uuid,
        name: &str,
        role: &str,
        status: &str,
        reports_to: Option<Uuid>,
    ) -> IssueLivenessAgentInput {
        IssueLivenessAgentInput {
            id,
            company_id: company_id(),
            name: name.to_string(),
            role: Some(role.to_string()),
            title: None,
            status: status.to_string(),
            reports_to,
        }
    }

    #[test]
    fn detects_blocked_chain_with_unassigned_blocker() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);
        let manager_id = Uuid::from_u128(3);
        let root_id = Uuid::from_u128(4);

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1703", "Source", "blocked", None),
                make_issue(blocker_id, "PAP-1704", "Unassigned blocker", "todo", None),
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company_id(),
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![
                make_agent(manager_id, "Manager", "manager", "active", Some(root_id)),
                make_agent(root_id, "Root Operator", "operator", "active", None),
            ],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.state, IssueLivenessState::BlockedByUnassignedIssue);
        assert_eq!(f.source_issue_id, source_id);
        assert_eq!(f.recovery_issue_id, Some(blocker_id));
        assert_eq!(f.dependency_path.len(), 2);
        // Owner candidates: root_agent (root) → ordered_invokable_fallback (manager + root)
        // The blocker has no assignee → StalledBlockerAssignee not added
        // Then assignee_reporting_chain / creator_reporting_chain skipped (no start)
        // Then root_agent (root) + ordered_invokable_fallback (root, manager)
        assert_eq!(
            f.recommended_owner_candidates[0].reason,
            IssueLivenessOwnerCandidateReason::RootAgent
        );
        assert_eq!(f.recommended_owner_agent_id, Some(root_id));
    }

    #[test]
    fn does_not_flag_unassigned_blocker_with_active_run() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);

        let mut blocker = make_issue(blocker_id, "PAP-1704", "Unassigned", "todo", None);
        blocker.assignee_user_id = Some("alice".into());

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1703", "Source", "blocked", None),
                blocker,
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company_id(),
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_assigned_backlog_blocker_with_monitor() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(3);

        let mut blocker = make_issue(blocker_id, "PAP-1704", "Backlog", "backlog", Some(agent_id));
        blocker.monitor_next_check_at =
            Some(chrono::Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap());

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1703", "Source", "blocked", None),
                blocker,
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company_id(),
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![make_agent(agent_id, "Agent", "engineer", "active", None)],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert!(findings.is_empty());
    }

    #[test]
    fn detects_assigned_backlog_blocker_with_no_action_path() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(3);

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1703", "Source", "blocked", None),
                make_issue(blocker_id, "PAP-1704", "Backlog", "backlog", Some(agent_id)),
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company_id(),
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![make_agent(agent_id, "Agent", "engineer", "active", None)],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].state,
            IssueLivenessState::BlockedByAssignedBacklogIssue
        );
    }

    #[test]
    fn detects_cancelled_blocker() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1703", "Source", "blocked", None),
                make_issue(blocker_id, "PAP-1704", "Cancelled", "cancelled", None),
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company_id(),
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].state,
            IssueLivenessState::BlockedByCancelledIssue
        );
    }

    #[test]
    fn detects_blocked_by_uninvokable_assignee() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);
        let agent_id = Uuid::from_u128(3);
        let manager_id = Uuid::from_u128(4);

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1703", "Source", "blocked", None),
                make_issue(
                    blocker_id,
                    "PAP-1704",
                    "Blocked-by-terminated",
                    "todo",
                    Some(agent_id),
                ),
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company_id(),
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![
                make_agent(
                    agent_id,
                    "Terminated Agent",
                    "engineer",
                    "terminated",
                    Some(manager_id),
                ),
                make_agent(manager_id, "Manager", "manager", "active", None),
            ],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].state,
            IssueLivenessState::BlockedByUninvokableAssignee
        );
    }

    #[test]
    fn detects_invalid_review_participant() {
        let issue_id = Uuid::from_u128(1);

        let mut issue = make_issue(issue_id, "PAP-1", "Review", "in_review", None);
        issue.assignee_agent_id = Some(Uuid::from_u128(2));
        issue.execution_state = Some(json!({
            "currentParticipant": { "type": "ghost", "id": "x" }
        }));

        let input = IssueGraphLivenessInput {
            issues: vec![issue],
            relations: vec![],
            agents: vec![make_agent(
                Uuid::from_u128(2),
                "Agent",
                "engineer",
                "active",
                None,
            )],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].state,
            IssueLivenessState::InvalidReviewParticipant
        );
    }

    #[test]
    fn detects_in_review_without_action_path() {
        let issue_id = Uuid::from_u128(1);
        let agent_id = Uuid::from_u128(2);

        // No executionState, no user owner, no monitor, no runs → must flag
        let input = IssueGraphLivenessInput {
            issues: vec![make_issue(
                issue_id,
                "PAP-1",
                "Review",
                "in_review",
                Some(agent_id),
            )],
            relations: vec![],
            agents: vec![make_agent(agent_id, "Agent", "engineer", "active", None)],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].state,
            IssueLivenessState::InReviewWithoutActionPath
        );
    }

    #[test]
    fn does_not_flag_in_review_with_active_run() {
        let issue_id = Uuid::from_u128(1);
        let agent_id = Uuid::from_u128(2);

        let mut issue = make_issue(issue_id, "PAP-1", "Review", "in_review", Some(agent_id));
        issue.execution_state = Some(json!({
            "currentParticipant": { "type": "agent", "agentId": agent_id.to_string() }
        }));

        let input = IssueGraphLivenessInput {
            issues: vec![issue],
            relations: vec![],
            agents: vec![make_agent(agent_id, "Agent", "engineer", "active", None)],
            active_runs: vec![IssueLivenessExecutionPathInput {
                company_id: company_id(),
                issue_id: Some(issue_id),
                agent_id: Some(agent_id),
                status: Some("active".into()),
            }],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert!(findings.is_empty());
    }

    #[test]
    fn cross_company_relations_are_ignored() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);
        let other_company = Uuid::from_u128(99);

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1", "Source", "blocked", None),
                make_issue(blocker_id, "PAP-2", "Blocker", "todo", None),
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: other_company,
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert!(findings.is_empty());
    }

    #[test]
    fn incident_key_is_stable() {
        let source_id = Uuid::from_u128(1);
        let blocker_id = Uuid::from_u128(2);

        let input = IssueGraphLivenessInput {
            issues: vec![
                make_issue(source_id, "PAP-1", "Source", "blocked", None),
                make_issue(blocker_id, "PAP-2", "Blocker", "todo", None),
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company_id(),
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![],
            now: fixed_now(),
            ..Default::default()
        };
        let findings = classify_issue_graph_liveness(&input);
        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .incident_key
            .contains("blocked_by_unassigned_issue"));
        assert!(findings[0].incident_key.contains(&blocker_id.to_string()));
    }
}
