//! `issue_execution_policy` — Issue execution policy 的纯 helpers。
//!
//! 与 Node `issue-execution-policy.ts` 中除高阶 `apply*Transition` 系列外的
//! 全部 pure helpers 1:1 对齐：
//!
//! - 常量：`DEFAULT_MAX_REVIEW_ROUNDS` / `MONITOR_INVALID_MESSAGE` 等
//! - 主体 helpers：
//!   - `assignee_principal` / `principals_equal`
//!   - `resolve_max_review_rounds` / `review_escalation_user_id`
//!   - `find_stage_by_id` / `next_pending_stage` / `next_pending_stage_after`
//!   - `select_stage_participant` / `stage_has_participant` / `patch_for_principal`
//!   - `next_assignee_ids`
//!   - `strip_monitor_from_execution_policy` /
//!     `set_issue_execution_policy_monitor_scheduled_by`
//!   - `issue_allows_monitor` / `monitor_clear_reason_for_issue` /
//!     `parse_monitor_date` / `exhausted_monitor_clear_reason`
//! - state builders：
//!   - `build_completed_state` / `build_state_with_completed_stages` /
//!     `build_skipped_stage_completed_state` / `build_pending_state` /
//!     `build_changes_requested_state`
//! - patch builders：
//!   - `build_pending_stage_patch` / `clear_execution_state_patch` /
//!     `can_auto_skip_pending_stage`
//! - monitor state builders：
//!   - `derive_persisted_monitor_state` / `build_scheduled_monitor_state` /
//!     `build_triggered_monitor_state` / `build_cleared_monitor_state`
//!
//! 设计目标：纯函数模块，无 IO/DB/clock 依赖；与
//! `issue_execution_monitor_state` 协同形成完整的 policy 工具集。
//!
//! 未在本轮实现：高阶 `apply*Transition`（含阶段流转、escalation、escalated
//! hold 等复杂分支），将在后续 rounds 单独 port。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::issue_execution_monitor_state::{
    normalize_monitor_notes, normalize_monitor_text, redact_issue_monitor_external_ref,
    IssueExecutionMonitorClearReason, IssueExecutionMonitorKind, IssueExecutionMonitorPolicy,
    IssueExecutionMonitorState, IssueExecutionMonitorStateStatus, IssueExecutionStagePrincipal,
    IssueExecutionStageType, IssueExecutionState, IssueExecutionStateStatus,
    IssueMonitorScheduledBy, MonitorMetadata, ReviewRequest,
};

// ============================================================================
// Constants
// ============================================================================

pub const DEFAULT_MAX_REVIEW_ROUNDS: i64 = 3;

pub const MONITOR_INVALID_MESSAGE: &str =
    "Monitor can only be scheduled on issues assigned to an agent in in_progress or in_review";

pub const MONITOR_BOUNDS_EXHAUSTED_MESSAGE: &str = "Monitor bounds are already exhausted";

pub const STAGE_DECISION_COMMENT_HINT: &str =
    "Include the decision comment in the same PATCH request; prior comments are not considered.";

// ============================================================================
// Enums / Structs
// ============================================================================

/// `IssueExecutionPolicyMode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueExecutionPolicyMode {
    Normal,
}

impl IssueExecutionPolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
        }
    }
}

/// `IssueExecutionDecisionOutcome`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueExecutionDecisionOutcome {
    Approved,
    ChangesRequested,
    Rejected,
}

impl Default for IssueExecutionDecisionOutcome {
    fn default() -> Self {
        Self::Approved
    }
}

impl IssueExecutionDecisionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes_requested",
            Self::Rejected => "rejected",
        }
    }
}

/// `IssueExecutionParticipant`：stage 内的单个 participant。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IssueExecutionParticipant {
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: IssueExecutionStageType,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
}

/// `IssueExecutionStage`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueExecutionStage {
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: IssueExecutionStageType,
    pub approvals_needed: i64,
    pub participants: Vec<IssueExecutionParticipant>,
}

/// `IssueExecutionPolicy`：完整的 policy 结构。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueExecutionPolicy {
    pub mode: Option<IssueExecutionPolicyMode>,
    pub comment_required: bool,
    pub stages: Vec<IssueExecutionStage>,
    pub monitor: Option<IssueExecutionMonitorPolicy>,
    pub max_review_rounds: Option<i64>,
}

/// `IssueExecutionDecision`：决策记录。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IssueExecutionDecision {
    pub stage_id: String,
    #[serde(rename = "type")]
    pub stage_type: IssueExecutionStageType,
    pub outcome: IssueExecutionDecisionOutcome,
    pub body: String,
}

// ============================================================================
// Input/Output wrapper structs
// ============================================================================

/// `StageParticipantSelectorOpts`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageParticipantSelectorOpts {
    pub preferred: Option<IssueExecutionStagePrincipal>,
    pub exclude: Option<IssueExecutionStagePrincipal>,
}

/// `BuildPendingStagePatchInput`。
#[derive(Debug, Clone)]
pub struct BuildPendingStagePatchInput<'a> {
    pub patch: Map<String, Value>,
    pub previous: Option<&'a IssueExecutionState>,
    pub policy: &'a IssueExecutionPolicy,
    pub stage: &'a IssueExecutionStage,
    pub participant: &'a IssueExecutionStagePrincipal,
    pub return_assignee: Option<IssueExecutionStagePrincipal>,
    pub review_request: Option<ReviewRequest>,
    pub changes_requested_count: Option<i64>,
}

/// `BuildPendingStateInput`。
#[derive(Debug, Clone)]
pub struct BuildPendingStateInput<'a> {
    pub previous: Option<&'a IssueExecutionState>,
    pub stage: &'a IssueExecutionStage,
    pub stage_index: i64,
    pub participant: IssueExecutionStagePrincipal,
    pub return_assignee: Option<IssueExecutionStagePrincipal>,
    pub review_request: Option<ReviewRequest>,
    pub changes_requested_count: Option<i64>,
}

/// `BuildStateWithCompletedStagesInput`。
#[derive(Debug, Clone)]
pub struct BuildStateWithCompletedStagesInput<'a> {
    pub previous: Option<&'a IssueExecutionState>,
    pub completed_stage_ids: Vec<String>,
    pub return_assignee: Option<IssueExecutionStagePrincipal>,
}

/// `ClearExecutionStatePatchInput`。
#[derive(Debug, Clone)]
pub struct ClearExecutionStatePatchInput<'a> {
    pub patch: Map<String, Value>,
    pub issue_status: &'a str,
    pub requested_status: Option<&'a str>,
    pub return_assignee: Option<IssueExecutionStagePrincipal>,
}

/// `CanAutoSkipPendingStageInput`。
#[derive(Debug, Clone)]
pub struct CanAutoSkipPendingStageInput<'a> {
    pub stage: &'a IssueExecutionStage,
    pub return_assignee: Option<IssueExecutionStagePrincipal>,
    pub requested_status: Option<&'a str>,
}

/// `AssigneeLike`：assignee 视图。
#[derive(Debug, Clone, Default)]
pub struct AssigneeLike {
    pub assignee_agent_id: Option<String>,
    pub assignee_user_id: Option<String>,
}

/// `ActorLike`：actor 视图。
#[derive(Debug, Clone, Default)]
pub struct ActorLike {
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
}

/// `RequestedAssigneePatch`：请求 patch。
#[derive(Debug, Clone, Default)]
pub struct RequestedAssigneePatch {
    pub assignee_agent_id: Option<String>,
    pub assignee_user_id: Option<String>,
}

/// `IssueLike`：最小 issue 视图。
#[derive(Debug, Clone, Default)]
pub struct IssueLike {
    pub assignee_agent_id: Option<String>,
    pub assignee_user_id: Option<String>,
    pub status: String,
    pub responsible_user_id: Option<String>,
    pub created_by_user_id: Option<String>,
    pub execution_policy: Option<IssueExecutionPolicy>,
    pub execution_state: Option<IssueExecutionState>,
    pub monitor_next_check_at: Option<chrono::DateTime<chrono::Utc>>,
    pub monitor_wake_requested_at: Option<chrono::DateTime<chrono::Utc>>,
    pub monitor_last_triggered_at: Option<chrono::DateTime<chrono::Utc>>,
    pub monitor_attempt_count: Option<i64>,
    pub monitor_notes: Option<String>,
    pub monitor_scheduled_by: Option<IssueMonitorScheduledBy>,
}

/// `NextAssigneeIdsInput`。
#[derive(Debug, Clone)]
pub struct NextAssigneeIdsInput<'a> {
    pub issue: &'a IssueLike,
    pub requested_assignee_patch: &'a RequestedAssigneePatch,
    pub stage_patch: &'a Map<String, Value>,
}

/// `ExhaustedMonitorClearReasonInput`。
#[derive(Debug, Clone)]
pub struct ExhaustedMonitorClearReasonInput<'a> {
    pub monitor: &'a IssueExecutionMonitorPolicy,
    pub attempt_count: i64,
    pub now: chrono::DateTime<chrono::Utc>,
}

/// `DerivePersistedMonitorStateInput`。
#[derive(Debug, Clone)]
pub struct DerivePersistedMonitorStateInput<'a> {
    pub issue: &'a IssueLike,
    pub state: Option<&'a IssueExecutionState>,
    pub policy: Option<&'a IssueExecutionPolicy>,
}

/// `BuildTriggeredMonitorStateInput`。
#[derive(Debug, Clone)]
pub struct BuildTriggeredMonitorStateInput<'a> {
    pub previous: Option<&'a IssueExecutionMonitorState>,
    pub triggered_at: chrono::DateTime<chrono::Utc>,
}

/// `BuildClearedMonitorStateInput`。
#[derive(Debug, Clone)]
pub struct BuildClearedMonitorStateInput<'a> {
    pub previous: Option<&'a IssueExecutionMonitorState>,
    pub clear_reason: IssueExecutionMonitorClearReason,
    pub cleared_at: chrono::DateTime<chrono::Utc>,
}

// ============================================================================
// assigneePrincipal / actorPrincipal / principalsEqual
// ============================================================================

/// `assigneePrincipal(input)`：assigneeAgentId/userId → principal。
pub fn assignee_principal(input: &AssigneeLike) -> Option<IssueExecutionStagePrincipal> {
    if let Some(agent_id) = input.assignee_agent_id.as_deref() {
        if !agent_id.is_empty() {
            return Some(IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: Some(agent_id.to_string()),
                user_id: None,
            });
        }
    }
    if let Some(user_id) = input.assignee_user_id.as_deref() {
        if !user_id.is_empty() {
            return Some(IssueExecutionStagePrincipal {
                principal_type: "user".to_string(),
                agent_id: None,
                user_id: Some(user_id.to_string()),
            });
        }
    }
    None
}

/// `actorPrincipal(actor)`：actor.agentId/userId → principal。
pub fn actor_principal(actor: &ActorLike) -> Option<IssueExecutionStagePrincipal> {
    if let Some(agent_id) = actor.agent_id.as_deref() {
        if !agent_id.is_empty() {
            return Some(IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: Some(agent_id.to_string()),
                user_id: None,
            });
        }
    }
    if let Some(user_id) = actor.user_id.as_deref() {
        if !user_id.is_empty() {
            return Some(IssueExecutionStagePrincipal {
                principal_type: "user".to_string(),
                agent_id: None,
                user_id: Some(user_id.to_string()),
            });
        }
    }
    None
}

/// `principalsEqual(a, b)`：principal 等值比较。
pub fn principals_equal(
    a: Option<&IssueExecutionStagePrincipal>,
    b: Option<&IssueExecutionStagePrincipal>,
) -> bool {
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => return false,
    };
    a.agent_id == b.agent_id && a.user_id == b.user_id
}

// ============================================================================
// resolveMaxReviewRounds / reviewEscalationUserId
// ============================================================================

/// `resolveMaxReviewRounds(policy)`：maxReviewRounds → 实际值。
pub fn resolve_max_review_rounds(policy: Option<&IssueExecutionPolicy>) -> i64 {
    if let Some(rounds) = policy.and_then(|p| p.max_review_rounds) {
        if rounds > 0 {
            return rounds;
        }
    }
    DEFAULT_MAX_REVIEW_ROUNDS
}

/// `reviewEscalationUserId(issue)`：升级目标 user。
pub fn review_escalation_user_id(issue: &IssueLike) -> Option<String> {
    if let Some(responsible) = issue.responsible_user_id.as_deref() {
        let trimmed = responsible.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(creator) = issue.created_by_user_id.as_deref() {
        let trimmed = creator.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

// ============================================================================
// findStageById / nextPendingStage / nextPendingStageAfter
// ============================================================================

/// `findStageById(policy, stageId)`：按 id 查 stage。
pub fn find_stage_by_id<'a>(
    policy: &'a IssueExecutionPolicy,
    stage_id: Option<&str>,
) -> Option<&'a IssueExecutionStage> {
    let id = stage_id?;
    policy.stages.iter().find(|s| s.id.as_deref() == Some(id))
}

/// `nextPendingStage(policy, state)`：下一个未完成 stage。
pub fn next_pending_stage<'a>(
    policy: &'a IssueExecutionPolicy,
    state: Option<&IssueExecutionState>,
) -> Option<&'a IssueExecutionStage> {
    let completed: std::collections::HashSet<&str> = state
        .map(|s| s.completed_stage_ids.iter().map(String::as_str).collect())
        .unwrap_or_default();
    policy.stages.iter().find(|s| {
        s.id.as_deref()
            .map(|id| !completed.contains(id))
            .unwrap_or(false)
    })
}

/// `nextPendingStageAfter(policy, completedStage, state)`：completedStage
/// 之后的下一个未完成 stage。
pub fn next_pending_stage_after<'a>(
    policy: &'a IssueExecutionPolicy,
    completed_stage: &IssueExecutionStage,
    state: Option<&IssueExecutionState>,
) -> Option<&'a IssueExecutionStage> {
    let completed: std::collections::HashSet<&str> = state
        .map(|s| s.completed_stage_ids.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let completed_index = policy
        .stages
        .iter()
        .position(|s| s.id == completed_stage.id);
    let start = match completed_index {
        Some(i) => i + 1,
        None => return None,
    };
    policy.stages.iter().skip(start).find(|s| {
        s.id.as_deref()
            .map(|id| !completed.contains(id))
            .unwrap_or(false)
    })
}

// ============================================================================
// selectStageParticipant / stageHasParticipant / patchForPrincipal
// ============================================================================

/// `selectStageParticipant(stage, opts)`：从 stage.participants 中选择 participant。
pub fn select_stage_participant(
    stage: &IssueExecutionStage,
    opts: Option<&StageParticipantSelectorOpts>,
) -> Option<IssueExecutionStagePrincipal> {
    let empty = StageParticipantSelectorOpts::default();
    let opts = opts.unwrap_or(&empty);
    let candidates: Vec<&IssueExecutionParticipant> = stage
        .participants
        .iter()
        .filter(|p| {
            let principal = IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: p.agent_id.clone(),
                user_id: p.user_id.clone(),
            };
            !principals_equal(Some(&principal), opts.exclude.as_ref())
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if let Some(preferred) = opts.preferred.as_ref() {
        for candidate in &candidates {
            let principal = IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: candidate.agent_id.clone(),
                user_id: candidate.user_id.clone(),
            };
            if principals_equal(Some(&principal), Some(preferred)) {
                return Some(principal);
            }
        }
    }
    let first = candidates[0];
    Some(IssueExecutionStagePrincipal {
        principal_type: "agent".to_string(),
        agent_id: first.agent_id.clone(),
        user_id: first.user_id.clone(),
    })
}

/// `stageHasParticipant(stage, participant)`：stage 是否包含该 participant。
pub fn stage_has_participant(
    stage: &IssueExecutionStage,
    participant: Option<&IssueExecutionStagePrincipal>,
) -> bool {
    let Some(participant) = participant else {
        return false;
    };
    stage.participants.iter().any(|candidate| {
        principals_equal(
            Some(&IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: candidate.agent_id.clone(),
                user_id: candidate.user_id.clone(),
            }),
            Some(participant),
        )
    })
}

/// `patchForPrincipal(principal)`：principal → assignee patch dict。
pub fn patch_for_principal(principal: Option<&IssueExecutionStagePrincipal>) -> Map<String, Value> {
    let mut map = Map::new();
    match principal {
        None => {
            map.insert("assigneeAgentId".into(), Value::Null);
            map.insert("assigneeUserId".into(), Value::Null);
        }
        Some(p) => {
            if let Some(agent_id) = p.agent_id.clone() {
                map.insert("assigneeAgentId".into(), Value::String(agent_id));
                map.insert("assigneeUserId".into(), Value::Null);
            } else if let Some(user_id) = p.user_id.clone() {
                map.insert("assigneeAgentId".into(), Value::Null);
                map.insert("assigneeUserId".into(), Value::String(user_id));
            } else {
                map.insert("assigneeAgentId".into(), Value::Null);
                map.insert("assigneeUserId".into(), Value::Null);
            }
        }
    }
    map
}

// ============================================================================
// nextAssigneeIds
// ============================================================================

/// `nextAssigneeIds(input)`：derive final assigneeAgentId/userId。
pub fn next_assignee_ids(input: &NextAssigneeIdsInput<'_>) -> (Option<String>, Option<String>) {
    let assignee_agent_id = next_assignee_id_value(
        input.stage_patch.get("assigneeAgentId"),
        input.requested_assignee_patch.assignee_agent_id.as_deref(),
        input.issue.assignee_agent_id.as_deref(),
    );
    let assignee_user_id = next_assignee_id_value(
        input.stage_patch.get("assigneeUserId"),
        input.requested_assignee_patch.assignee_user_id.as_deref(),
        input.issue.assignee_user_id.as_deref(),
    );
    (assignee_agent_id, assignee_user_id)
}

fn next_assignee_id_value(
    stage_value: Option<&Value>,
    requested_value: Option<&str>,
    issue_value: Option<&str>,
) -> Option<String> {
    if let Some(v) = stage_value {
        return match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            _ => None,
        };
    }
    if let Some(req) = requested_value {
        return Some(req.to_string());
    }
    issue_value.map(|s| s.to_string())
}

// ============================================================================
// stripMonitorFromExecutionPolicy / setIssueExecutionPolicyMonitorScheduledBy
// ============================================================================

/// `stripMonitorFromExecutionPolicy(policy)`：剥除 monitor 字段。
pub fn strip_monitor_from_execution_policy(
    policy: Option<&IssueExecutionPolicy>,
) -> Option<IssueExecutionPolicy> {
    let policy = policy?;
    if policy.monitor.is_none() {
        return Some(policy.clone());
    }
    if policy.stages.is_empty() {
        return None;
    }
    Some(IssueExecutionPolicy {
        mode: policy.mode,
        comment_required: policy.comment_required,
        stages: policy.stages.clone(),
        monitor: None,
        max_review_rounds: policy.max_review_rounds,
    })
}

/// `setIssueExecutionPolicyMonitorScheduledBy(policy, scheduledBy)`：patch scheduled_by。
pub fn set_issue_execution_policy_monitor_scheduled_by(
    policy: Option<&IssueExecutionPolicy>,
    scheduled_by: IssueMonitorScheduledBy,
) -> Option<IssueExecutionPolicy> {
    let policy = policy?;
    if policy.monitor.is_none() {
        return Some(policy.clone());
    }
    let monitor = policy.monitor.as_ref().unwrap();
    let mut new_policy = policy.clone();
    new_policy.monitor = Some(IssueExecutionMonitorPolicy {
        scheduled_by,
        ..monitor.clone()
    });
    Some(new_policy)
}

// ============================================================================
// issueAllowsMonitor / monitorClearReasonForIssue / parseMonitorDate /
//   exhaustedMonitorClearReason
// ============================================================================

/// `issueAllowsMonitor(status, assigneeAgentId, assigneeUserId)`：是否允许 monitor。
pub fn issue_allows_monitor(
    status: &str,
    assignee_agent_id: Option<&str>,
    assignee_user_id: Option<&str>,
) -> bool {
    let has_agent = assignee_agent_id.map(|s| !s.is_empty()).unwrap_or(false);
    let no_user = assignee_user_id.map(|s| s.is_empty()).unwrap_or(true);
    has_agent && no_user && (status == "in_progress" || status == "in_review")
}

/// `monitorClearReasonForIssue(status, assigneeAgentId, assigneeUserId)`。
pub fn monitor_clear_reason_for_issue(
    status: &str,
    assignee_agent_id: Option<&str>,
    assignee_user_id: Option<&str>,
) -> Option<IssueExecutionMonitorClearReason> {
    if status == "done" {
        return Some(IssueExecutionMonitorClearReason::Completed);
    }
    if status == "cancelled" {
        return Some(IssueExecutionMonitorClearReason::Cancelled);
    }
    if !issue_allows_monitor(status, assignee_agent_id, assignee_user_id) {
        if assignee_user_id.map(|s| !s.is_empty()).unwrap_or(false) || assignee_agent_id.is_none() {
            return Some(IssueExecutionMonitorClearReason::Stale);
        }
        return Some(IssueExecutionMonitorClearReason::Expired);
    }
    None
}

/// `parseMonitorDate(value)`：解析 ISO 字符串为 DateTime。
pub fn parse_monitor_date(value: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    let value = value?;
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// `exhaustedMonitorClearReason(input)`：检查 timeoutAt / maxAttempts。
pub fn exhausted_monitor_clear_reason(
    input: &ExhaustedMonitorClearReasonInput<'_>,
) -> Option<IssueExecutionMonitorClearReason> {
    if let Some(timeout_at) = parse_monitor_date(input.monitor.timeout_at.as_deref()) {
        if input.now >= timeout_at {
            return Some(IssueExecutionMonitorClearReason::Expired);
        }
    }
    if let Some(max_attempts) = input.monitor.max_attempts {
        if input.attempt_count >= max_attempts {
            return Some(IssueExecutionMonitorClearReason::Exhausted);
        }
    }
    None
}

// ============================================================================
// State builders
// ============================================================================

/// `buildCompletedState(previous, currentStage)`。
pub fn build_completed_state(
    previous: Option<&IssueExecutionState>,
    current_stage: &IssueExecutionStage,
) -> IssueExecutionState {
    let mut completed_stage_ids: Vec<String> = previous
        .map(|s| s.completed_stage_ids.clone())
        .unwrap_or_default();
    if let Some(id) = current_stage.id.as_deref() {
        if !completed_stage_ids.iter().any(|x| x == id) {
            completed_stage_ids.push(id.to_string());
        }
    }
    IssueExecutionState {
        status: IssueExecutionStateStatus::Completed,
        current_stage_id: None,
        current_stage_index: None,
        current_stage_type: None,
        current_participant: None,
        return_assignee: previous.and_then(|p| p.return_assignee.clone()),
        review_request: None,
        completed_stage_ids,
        last_decision_id: previous.and_then(|p| p.last_decision_id.clone()),
        last_decision_outcome: Some("approved".to_string()),
        monitor: previous.and_then(|p| p.monitor.clone()),
        changes_requested_count: Some(0),
    }
}

/// `buildStateWithCompletedStages(input)`。
pub fn build_state_with_completed_stages(
    input: &BuildStateWithCompletedStagesInput<'_>,
) -> IssueExecutionState {
    let prev = input.previous;
    IssueExecutionState {
        status: prev
            .map(|s| s.status)
            .unwrap_or(IssueExecutionStateStatus::Pending),
        current_stage_id: prev.and_then(|s| s.current_stage_id.clone()),
        current_stage_index: prev.and_then(|s| s.current_stage_index),
        current_stage_type: prev.and_then(|s| s.current_stage_type),
        current_participant: prev.and_then(|s| s.current_participant.clone()),
        return_assignee: prev
            .and_then(|s| s.return_assignee.clone())
            .or_else(|| input.return_assignee.clone()),
        review_request: prev.and_then(|s| s.review_request.clone()),
        completed_stage_ids: input.completed_stage_ids.clone(),
        last_decision_id: prev.and_then(|s| s.last_decision_id.clone()),
        last_decision_outcome: prev.and_then(|s| s.last_decision_outcome.clone()),
        monitor: prev.and_then(|s| s.monitor.clone()),
        changes_requested_count: prev.and_then(|s| s.changes_requested_count),
    }
}

/// `buildSkippedStageCompletedState(input)`。
pub fn build_skipped_stage_completed_state(
    input: &BuildStateWithCompletedStagesInput<'_>,
) -> IssueExecutionState {
    let prev = input.previous;
    IssueExecutionState {
        status: IssueExecutionStateStatus::Completed,
        current_stage_id: None,
        current_stage_index: None,
        current_stage_type: None,
        current_participant: None,
        return_assignee: prev
            .and_then(|s| s.return_assignee.clone())
            .or_else(|| input.return_assignee.clone()),
        review_request: None,
        completed_stage_ids: input.completed_stage_ids.clone(),
        last_decision_id: prev.and_then(|s| s.last_decision_id.clone()),
        last_decision_outcome: prev.and_then(|s| s.last_decision_outcome.clone()),
        monitor: prev.and_then(|s| s.monitor.clone()),
        changes_requested_count: Some(0),
    }
}

/// `buildPendingState(input)`。
pub fn build_pending_state(input: &BuildPendingStateInput<'_>) -> IssueExecutionState {
    let prev = input.previous;
    IssueExecutionState {
        status: IssueExecutionStateStatus::Pending,
        current_stage_id: input.stage.id.clone(),
        current_stage_index: Some(input.stage_index),
        current_stage_type: Some(input.stage.kind),
        current_participant: Some(input.participant.clone()),
        return_assignee: input.return_assignee.clone(),
        review_request: input.review_request.clone(),
        completed_stage_ids: prev
            .map(|s| s.completed_stage_ids.clone())
            .unwrap_or_default(),
        last_decision_id: prev.and_then(|s| s.last_decision_id.clone()),
        last_decision_outcome: prev.and_then(|s| s.last_decision_outcome.clone()),
        monitor: prev.and_then(|s| s.monitor.clone()),
        changes_requested_count: Some(
            input
                .changes_requested_count
                .or_else(|| prev.and_then(|s| s.changes_requested_count))
                .unwrap_or(0),
        ),
    }
}

/// `buildChangesRequestedState(previous, currentStage, count)`。
pub fn build_changes_requested_state(
    previous: &IssueExecutionState,
    current_stage: &IssueExecutionStage,
    changes_requested_count: i64,
) -> IssueExecutionState {
    let mut state = previous.clone();
    state.status = IssueExecutionStateStatus::ChangesRequested;
    state.current_stage_id = current_stage.id.clone();
    state.current_stage_type = Some(current_stage.kind);
    state.review_request = None;
    state.last_decision_outcome = Some("changes_requested".to_string());
    state.changes_requested_count = Some(changes_requested_count);
    state
}

// ============================================================================
// Patch builders
// ============================================================================

/// `buildPendingStagePatch(input)`。
pub fn build_pending_stage_patch(input: &BuildPendingStagePatchInput<'_>) -> Map<String, Value> {
    let mut patch = input.patch.clone();
    patch.insert("status".into(), Value::String("in_review".into()));
    let assignee = patch_for_principal(Some(input.participant));
    for (k, v) in assignee {
        patch.insert(k, v);
    }
    let stage_index = input
        .policy
        .stages
        .iter()
        .position(|s| s.id == input.stage.id)
        .map(|i| i as i64)
        .unwrap_or(0);
    let pending_state = build_pending_state(&BuildPendingStateInput {
        previous: input.previous,
        stage: input.stage,
        stage_index,
        participant: input.participant.clone(),
        return_assignee: input.return_assignee.clone(),
        review_request: input.review_request.clone(),
        changes_requested_count: input.changes_requested_count,
    });
    let state_json = serde_json::to_value(pending_state).unwrap_or(Value::Null);
    patch.insert("executionState".into(), state_json);
    patch
}

/// `clearExecutionStatePatch(input)`。
pub fn clear_execution_state_patch(
    input: &ClearExecutionStatePatchInput<'_>,
) -> Map<String, Value> {
    let mut patch = input.patch.clone();
    patch.insert("executionState".into(), Value::Null);
    if input.requested_status.is_none() && input.issue_status == "in_review" {
        if let Some(return_assignee) = input.return_assignee.as_ref() {
            patch.insert("status".into(), Value::String("in_progress".into()));
            let assignee = patch_for_principal(Some(return_assignee));
            for (k, v) in assignee {
                patch.insert(k, v);
            }
        }
    }
    patch
}

/// `canAutoSkipPendingStage(input)`。
pub fn can_auto_skip_pending_stage(input: &CanAutoSkipPendingStageInput<'_>) -> bool {
    if input.requested_status != Some("done") || input.stage.kind != IssueExecutionStageType::Agent
    {
        return false;
    }
    let Some(return_assignee) = input.return_assignee.as_ref() else {
        return false;
    };
    if input.stage.participants.is_empty() {
        return false;
    }
    input.stage.participants.iter().all(|p| {
        principals_equal(
            Some(&IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: p.agent_id.clone(),
                user_id: p.user_id.clone(),
            }),
            Some(return_assignee),
        )
    })
}

// ============================================================================
// Monitor state builders
// ============================================================================

/// `derive_persisted_monitor_state(input)`：从 issue/state/policy 派生最终
/// monitor state。
pub fn derive_persisted_monitor_state(
    input: &DerivePersistedMonitorStateInput<'_>,
) -> Option<IssueExecutionMonitorState> {
    let from_state = input.state.and_then(|s| s.monitor.clone());
    let scheduled_monitor = input.policy.and_then(|p| p.monitor.as_ref());

    let next_check_at_raw = input
        .issue
        .monitor_next_check_at
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .or_else(|| {
            scheduled_monitor
                .map(|m| m.next_check_at.clone())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| from_state.as_ref().and_then(|s| s.next_check_at.clone()));
    let last_triggered_at = input
        .issue
        .monitor_last_triggered_at
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .or_else(|| {
            from_state
                .as_ref()
                .and_then(|s| s.last_triggered_at.clone())
        });
    let attempt_count = input
        .issue
        .monitor_attempt_count
        .or_else(|| from_state.as_ref().map(|s| s.attempt_count))
        .unwrap_or(0);
    let notes = scheduled_monitor
        .and_then(|m| normalize_monitor_notes(m.notes.as_deref()))
        .or_else(|| normalize_monitor_notes(input.issue.monitor_notes.as_deref()))
        .or_else(|| {
            from_state
                .as_ref()
                .and_then(|s| normalize_monitor_notes(s.notes.as_deref()))
        });
    let scheduled_by_raw = input
        .issue
        .monitor_scheduled_by
        .or_else(|| scheduled_monitor.map(|m| m.scheduled_by))
        .or_else(|| from_state.as_ref().and_then(|s| s.scheduled_by));

    let metadata = if scheduled_monitor.is_some() {
        monitor_metadata_from_policy_view(scheduled_monitor.unwrap())
    } else {
        monitor_metadata_from_state_view(from_state.as_ref())
    };

    if next_check_at_raw.is_some() {
        return Some(IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Scheduled,
            next_check_at: next_check_at_raw,
            last_triggered_at,
            attempt_count,
            notes,
            scheduled_by: scheduled_by_raw,
            kind: metadata.kind.and_then(|k| parse_monitor_kind(&k)),
            service_name: metadata.service_name,
            external_ref: metadata.external_ref,
            timeout_at: metadata.timeout_at,
            max_attempts: metadata.max_attempts,
            recovery_policy: metadata.recovery_policy,
            cleared_at: None,
            clear_reason: None,
        });
    }

    if from_state.as_ref().map(|s| s.status) == Some(IssueExecutionMonitorStateStatus::Cleared) {
        let mut state = from_state?;
        state.notes = notes;
        state.scheduled_by = scheduled_by_raw;
        state.attempt_count = attempt_count;
        state.last_triggered_at = last_triggered_at;
        if let Some(kind) = metadata.kind {
            state.kind = parse_monitor_kind(&kind);
        }
        if let Some(svc) = metadata.service_name {
            state.service_name = normalize_monitor_text(Some(svc.as_str()));
        }
        if let Some(ext) = metadata.external_ref {
            state.external_ref =
                redact_issue_monitor_external_ref(Some(ext.as_str())).map(str::to_string);
        }
        if metadata.timeout_at.is_some() {
            state.timeout_at = metadata.timeout_at;
        }
        if metadata.max_attempts.is_some() {
            state.max_attempts = metadata.max_attempts;
        }
        if metadata.recovery_policy.is_some() {
            state.recovery_policy = metadata.recovery_policy;
        }
        return Some(state);
    }

    let is_triggered = from_state.as_ref().map(|s| s.status)
        == Some(IssueExecutionMonitorStateStatus::Running)
        || last_triggered_at.is_some()
        || attempt_count > 0;
    if is_triggered {
        return Some(IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Running,
            next_check_at: None,
            last_triggered_at,
            attempt_count,
            notes,
            scheduled_by: scheduled_by_raw,
            kind: metadata.kind.and_then(|k| parse_monitor_kind(&k)),
            service_name: metadata.service_name,
            external_ref: metadata.external_ref,
            timeout_at: metadata.timeout_at,
            max_attempts: metadata.max_attempts,
            recovery_policy: metadata.recovery_policy,
            cleared_at: None,
            clear_reason: None,
        });
    }

    None
}

fn monitor_metadata_from_policy_view(monitor: &IssueExecutionMonitorPolicy) -> MonitorMetadata {
    MonitorMetadata {
        kind: monitor.kind.map(|k| k.as_str().to_string()),
        service_name: normalize_monitor_text(monitor.service_name.as_deref()),
        external_ref: redact_issue_monitor_external_ref(monitor.external_ref.as_deref())
            .map(str::to_string),
        timeout_at: monitor.timeout_at.clone(),
        max_attempts: monitor.max_attempts,
        recovery_policy: monitor.recovery_policy.clone(),
    }
}

fn monitor_metadata_from_state_view(state: Option<&IssueExecutionMonitorState>) -> MonitorMetadata {
    match state {
        Some(s) => MonitorMetadata {
            kind: s.kind.map(|k| k.as_str().to_string()),
            service_name: normalize_monitor_text(s.service_name.as_deref()),
            external_ref: redact_issue_monitor_external_ref(s.external_ref.as_deref())
                .map(str::to_string),
            timeout_at: s.timeout_at.clone(),
            max_attempts: s.max_attempts,
            recovery_policy: s.recovery_policy.clone(),
        },
        None => MonitorMetadata::default(),
    }
}

fn parse_monitor_kind(s: &str) -> Option<IssueExecutionMonitorKind> {
    match s {
        "external_service" => Some(IssueExecutionMonitorKind::ExternalService),

        _ => None,
    }
}

/// `build_scheduled_monitor_state(previous, monitor)`。
pub fn build_scheduled_monitor_state(
    previous: Option<&IssueExecutionMonitorState>,
    monitor: &IssueExecutionMonitorPolicy,
) -> IssueExecutionMonitorState {
    let metadata = monitor_metadata_from_policy_view(monitor);
    IssueExecutionMonitorState {
        status: IssueExecutionMonitorStateStatus::Scheduled,
        next_check_at: Some(monitor.next_check_at.clone()),
        last_triggered_at: previous.and_then(|p| p.last_triggered_at.clone()),
        attempt_count: previous.map(|p| p.attempt_count).unwrap_or(0),
        notes: monitor
            .notes
            .clone()
            .and_then(|n| normalize_monitor_notes(Some(&n))),
        scheduled_by: Some(monitor.scheduled_by),
        kind: metadata.kind.and_then(|k| parse_monitor_kind(&k)),
        service_name: metadata.service_name,
        external_ref: metadata.external_ref,
        timeout_at: metadata.timeout_at,
        max_attempts: metadata.max_attempts,
        recovery_policy: metadata.recovery_policy,
        cleared_at: None,
        clear_reason: None,
    }
}

/// `build_triggered_monitor_state(input)`。
pub fn build_triggered_monitor_state(
    input: &BuildTriggeredMonitorStateInput<'_>,
) -> IssueExecutionMonitorState {
    let metadata = monitor_metadata_from_state_view(input.previous);
    IssueExecutionMonitorState {
        status: IssueExecutionMonitorStateStatus::Running,
        next_check_at: None,
        last_triggered_at: Some(
            input
                .triggered_at
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
        attempt_count: input.previous.map(|p| p.attempt_count).unwrap_or(0) + 1,
        notes: input.previous.and_then(|p| p.notes.clone()),
        scheduled_by: input.previous.and_then(|p| p.scheduled_by),
        kind: metadata.kind.and_then(|k| parse_monitor_kind(&k)),
        service_name: metadata.service_name,
        external_ref: metadata.external_ref,
        timeout_at: metadata.timeout_at,
        max_attempts: metadata.max_attempts,
        recovery_policy: metadata.recovery_policy,
        cleared_at: None,
        clear_reason: None,
    }
}

/// `build_cleared_monitor_state(input)`。
pub fn build_cleared_monitor_state(
    input: &BuildClearedMonitorStateInput<'_>,
) -> IssueExecutionMonitorState {
    let metadata = monitor_metadata_from_state_view(input.previous);
    IssueExecutionMonitorState {
        status: IssueExecutionMonitorStateStatus::Cleared,
        next_check_at: None,
        last_triggered_at: input.previous.and_then(|p| p.last_triggered_at.clone()),
        attempt_count: input.previous.map(|p| p.attempt_count).unwrap_or(0),
        notes: input.previous.and_then(|p| p.notes.clone()),
        scheduled_by: input.previous.and_then(|p| p.scheduled_by),
        kind: metadata.kind.and_then(|k| parse_monitor_kind(&k)),
        service_name: metadata.service_name,
        external_ref: metadata.external_ref,
        timeout_at: metadata.timeout_at,
        max_attempts: metadata.max_attempts,
        recovery_policy: metadata.recovery_policy,
        cleared_at: Some(
            input
                .cleared_at
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
        clear_reason: Some(input.clear_reason),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_execution_monitor_state::{
        blank_execution_state, IssueExecutionMonitorClearReason, IssueExecutionMonitorKind,
        IssueExecutionMonitorPolicy, IssueExecutionMonitorState, IssueExecutionMonitorStateStatus,
        IssueExecutionStagePrincipal, IssueExecutionStageType, IssueExecutionState,
        IssueExecutionStateStatus, IssueMonitorScheduledBy, MonitorRecoveryPolicy,
    };
    use chrono::TimeZone;
    use serde_json::json;

    fn utc_dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    fn make_stage(id: &str, kind: IssueExecutionStageType, agent: &str) -> IssueExecutionStage {
        IssueExecutionStage {
            id: Some(id.into()),
            kind,
            approvals_needed: 1,
            participants: vec![IssueExecutionParticipant {
                id: Some(format!("p-{id}")),
                kind,
                agent_id: Some(agent.into()),
                user_id: None,
            }],
        }
    }

    fn make_stage_with_users(id: &str, user_id: &str) -> IssueExecutionStage {
        IssueExecutionStage {
            id: Some(id.into()),
            kind: IssueExecutionStageType::User,
            approvals_needed: 1,
            participants: vec![IssueExecutionParticipant {
                id: Some(format!("p-{id}")),
                kind: IssueExecutionStageType::User,
                agent_id: None,
                user_id: Some(user_id.into()),
            }],
        }
    }

    fn make_policy(stages: Vec<IssueExecutionStage>) -> IssueExecutionPolicy {
        IssueExecutionPolicy {
            mode: Some(IssueExecutionPolicyMode::Normal),
            comment_required: true,
            stages,
            monitor: None,
            max_review_rounds: None,
        }
    }

    // ----- constants -----

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(DEFAULT_MAX_REVIEW_ROUNDS, 3);
        assert!(MONITOR_INVALID_MESSAGE.contains("Monitor can only be scheduled"));
        assert_eq!(
            MONITOR_BOUNDS_EXHAUSTED_MESSAGE,
            "Monitor bounds are already exhausted"
        );
        assert!(STAGE_DECISION_COMMENT_HINT.contains("decision comment"));
    }

    // ----- enum roundtrip -----

    #[test]
    fn policy_mode_as_str() {
        assert_eq!(IssueExecutionPolicyMode::Normal.as_str(), "normal");
    }

    #[test]
    fn decision_outcome_as_str() {
        assert_eq!(IssueExecutionDecisionOutcome::Approved.as_str(), "approved");
        assert_eq!(
            IssueExecutionDecisionOutcome::ChangesRequested.as_str(),
            "changes_requested"
        );
        assert_eq!(IssueExecutionDecisionOutcome::Rejected.as_str(), "rejected");
    }

    // ----- assigneePrincipal / actorPrincipal / principalsEqual -----

    #[test]
    fn assignee_principal_agent() {
        let input = AssigneeLike {
            assignee_agent_id: Some("a1".into()),
            assignee_user_id: None,
        };
        let p = assignee_principal(&input).unwrap();
        assert_eq!(p.agent_id.as_deref(), Some("a1"));
        assert!(p.user_id.is_none());
    }

    #[test]
    fn assignee_principal_user() {
        let input = AssigneeLike {
            assignee_agent_id: None,
            assignee_user_id: Some("u1".into()),
        };
        let p = assignee_principal(&input).unwrap();
        assert_eq!(p.user_id.as_deref(), Some("u1"));
        assert!(p.agent_id.is_none());
    }

    #[test]
    fn assignee_principal_empty() {
        let input = AssigneeLike::default();
        assert!(assignee_principal(&input).is_none());
    }

    #[test]
    fn actor_principal_agent() {
        let actor = ActorLike {
            agent_id: Some("a2".into()),
            user_id: None,
        };
        let p = actor_principal(&actor).unwrap();
        assert_eq!(p.agent_id.as_deref(), Some("a2"));
    }

    #[test]
    fn actor_principal_user() {
        let actor = ActorLike {
            agent_id: None,
            user_id: Some("u2".into()),
        };
        let p = actor_principal(&actor).unwrap();
        assert_eq!(p.user_id.as_deref(), Some("u2"));
    }

    #[test]
    fn actor_principal_empty() {
        let actor = ActorLike::default();
        assert!(actor_principal(&actor).is_none());
    }

    #[test]
    fn principals_equal_same_agent() {
        let a = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let b = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        assert!(principals_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn principals_equal_different_agent() {
        let a = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let b = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a2".into()),
            user_id: None,
        };
        assert!(!principals_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn principals_equal_none() {
        let a = IssueExecutionStagePrincipal::default();
        assert!(!principals_equal(Some(&a), None));
        assert!(!principals_equal(None, Some(&a)));
        assert!(!principals_equal(None, None));
    }

    // ----- resolveMaxReviewRounds / reviewEscalationUserId -----

    #[test]
    fn resolve_max_review_rounds_default() {
        assert_eq!(resolve_max_review_rounds(None), 3);
    }

    #[test]
    fn resolve_max_review_rounds_from_policy() {
        let p = IssueExecutionPolicy {
            max_review_rounds: Some(5),
            ..Default::default()
        };
        assert_eq!(resolve_max_review_rounds(Some(&p)), 5);
    }

    #[test]
    fn resolve_max_review_rounds_zero_falls_back() {
        let p = IssueExecutionPolicy {
            max_review_rounds: Some(0),
            ..Default::default()
        };
        assert_eq!(resolve_max_review_rounds(Some(&p)), 3);
    }

    #[test]
    fn review_escalation_user_id_responsible() {
        let issue = IssueLike {
            responsible_user_id: Some("res-1".into()),
            created_by_user_id: Some("creator".into()),
            ..Default::default()
        };
        assert_eq!(review_escalation_user_id(&issue), Some("res-1".into()));
    }

    #[test]
    fn review_escalation_user_id_creator() {
        let issue = IssueLike {
            created_by_user_id: Some("creator".into()),
            ..Default::default()
        };
        assert_eq!(review_escalation_user_id(&issue), Some("creator".into()));
    }

    #[test]
    fn review_escalation_user_id_none() {
        let issue = IssueLike::default();
        assert!(review_escalation_user_id(&issue).is_none());
    }

    #[test]
    fn review_escalation_user_id_whitespace_skipped() {
        let issue = IssueLike {
            responsible_user_id: Some("   ".into()),
            created_by_user_id: Some("creator".into()),
            ..Default::default()
        };
        assert_eq!(review_escalation_user_id(&issue), Some("creator".into()));
    }

    // ----- findStageById / nextPendingStage / nextPendingStageAfter -----

    #[test]
    fn find_stage_by_id_present() {
        let p = make_policy(vec![
            make_stage("s1", IssueExecutionStageType::Agent, "a1"),
            make_stage("s2", IssueExecutionStageType::User, "u1"),
        ]);
        let s = find_stage_by_id(&p, Some("s2")).unwrap();
        assert_eq!(s.id.as_deref(), Some("s2"));
    }

    #[test]
    fn find_stage_by_id_missing() {
        let p = make_policy(vec![make_stage("s1", IssueExecutionStageType::Agent, "a1")]);
        assert!(find_stage_by_id(&p, Some("nope")).is_none());
        assert!(find_stage_by_id(&p, None).is_none());
    }

    #[test]
    fn next_pending_stage_no_completed() {
        let p = make_policy(vec![
            make_stage("s1", IssueExecutionStageType::Agent, "a1"),
            make_stage("s2", IssueExecutionStageType::Agent, "a2"),
        ]);
        let s = next_pending_stage(&p, None).unwrap();
        assert_eq!(s.id.as_deref(), Some("s1"));
    }

    #[test]
    fn next_pending_stage_skips_completed() {
        let p = make_policy(vec![
            make_stage("s1", IssueExecutionStageType::Agent, "a1"),
            make_stage("s2", IssueExecutionStageType::Agent, "a2"),
        ]);
        let state = IssueExecutionState {
            completed_stage_ids: vec!["s1".into()],
            ..Default::default()
        };
        let s = next_pending_stage(&p, Some(&state)).unwrap();
        assert_eq!(s.id.as_deref(), Some("s2"));
    }

    #[test]
    fn next_pending_stage_all_done() {
        let p = make_policy(vec![make_stage("s1", IssueExecutionStageType::Agent, "a1")]);
        let state = IssueExecutionState {
            completed_stage_ids: vec!["s1".into()],
            ..Default::default()
        };
        assert!(next_pending_stage(&p, Some(&state)).is_none());
    }

    #[test]
    fn next_pending_stage_after_basic() {
        let p = make_policy(vec![
            make_stage("s1", IssueExecutionStageType::Agent, "a1"),
            make_stage("s2", IssueExecutionStageType::Agent, "a2"),
            make_stage("s3", IssueExecutionStageType::Agent, "a3"),
        ]);
        let s1 = &p.stages[0];
        let next = next_pending_stage_after(&p, s1, None).unwrap();
        assert_eq!(next.id.as_deref(), Some("s2"));
    }

    #[test]
    fn next_pending_stage_after_skips_completed() {
        let p = make_policy(vec![
            make_stage("s1", IssueExecutionStageType::Agent, "a1"),
            make_stage("s2", IssueExecutionStageType::Agent, "a2"),
            make_stage("s3", IssueExecutionStageType::Agent, "a3"),
        ]);
        let s1 = &p.stages[0];
        let state = IssueExecutionState {
            completed_stage_ids: vec!["s1".into(), "s2".into()],
            ..Default::default()
        };
        let next = next_pending_stage_after(&p, s1, Some(&state)).unwrap();
        assert_eq!(next.id.as_deref(), Some("s3"));
    }

    // ----- selectStageParticipant / stageHasParticipant / patchForPrincipal -----

    #[test]
    fn select_stage_participant_first() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let p = select_stage_participant(&stage, None).unwrap();
        assert_eq!(p.agent_id.as_deref(), Some("a1"));
    }

    #[test]
    fn select_stage_participant_with_preferred() {
        let stage = IssueExecutionStage {
            id: Some("s1".into()),
            kind: IssueExecutionStageType::Agent,
            approvals_needed: 1,
            participants: vec![
                IssueExecutionParticipant {
                    id: Some("p1".into()),
                    kind: IssueExecutionStageType::Agent,
                    agent_id: Some("a1".into()),
                    user_id: None,
                },
                IssueExecutionParticipant {
                    id: Some("p2".into()),
                    kind: IssueExecutionStageType::Agent,
                    agent_id: Some("a2".into()),
                    user_id: None,
                },
            ],
        };
        let preferred = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a2".into()),
            user_id: None,
        };
        let opts = StageParticipantSelectorOpts {
            preferred: Some(preferred),
            exclude: None,
        };
        let p = select_stage_participant(&stage, Some(&opts)).unwrap();
        assert_eq!(p.agent_id.as_deref(), Some("a2"));
    }

    #[test]
    fn select_stage_participant_with_exclude() {
        let stage = IssueExecutionStage {
            id: Some("s1".into()),
            kind: IssueExecutionStageType::Agent,
            approvals_needed: 1,
            participants: vec![
                IssueExecutionParticipant {
                    id: Some("p1".into()),
                    kind: IssueExecutionStageType::Agent,
                    agent_id: Some("a1".into()),
                    user_id: None,
                },
                IssueExecutionParticipant {
                    id: Some("p2".into()),
                    kind: IssueExecutionStageType::Agent,
                    agent_id: Some("a2".into()),
                    user_id: None,
                },
            ],
        };
        let exclude = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let opts = StageParticipantSelectorOpts {
            preferred: None,
            exclude: Some(exclude),
        };
        let p = select_stage_participant(&stage, Some(&opts)).unwrap();
        assert_eq!(p.agent_id.as_deref(), Some("a2"));
    }

    #[test]
    fn select_stage_participant_empty() {
        let stage = IssueExecutionStage {
            id: Some("s1".into()),
            kind: IssueExecutionStageType::Agent,
            approvals_needed: 1,
            participants: vec![],
        };
        assert!(select_stage_participant(&stage, None).is_none());
    }

    #[test]
    fn stage_has_participant_present() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let p = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        assert!(stage_has_participant(&stage, Some(&p)));
    }

    #[test]
    fn stage_has_participant_missing() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let p = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a9".into()),
            user_id: None,
        };
        assert!(!stage_has_participant(&stage, Some(&p)));
    }

    #[test]
    fn stage_has_participant_none() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        assert!(!stage_has_participant(&stage, None));
    }

    #[test]
    fn patch_for_principal_none() {
        let p = patch_for_principal(None);
        assert_eq!(p.get("assigneeAgentId"), Some(&Value::Null));
        assert_eq!(p.get("assigneeUserId"), Some(&Value::Null));
    }

    #[test]
    fn patch_for_principal_agent() {
        let p = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let m = patch_for_principal(Some(&p));
        assert_eq!(m.get("assigneeAgentId"), Some(&Value::String("a1".into())));
        assert_eq!(m.get("assigneeUserId"), Some(&Value::Null));
    }

    #[test]
    fn patch_for_principal_user() {
        let p = IssueExecutionStagePrincipal {
            principal_type: "user".to_string(),
            agent_id: None,
            user_id: Some("u1".into()),
        };
        let m = patch_for_principal(Some(&p));
        assert_eq!(m.get("assigneeAgentId"), Some(&Value::Null));
        assert_eq!(m.get("assigneeUserId"), Some(&Value::String("u1".into())));
    }

    // ----- nextAssigneeIds -----

    #[test]
    fn next_assignee_ids_from_issue() {
        let issue = IssueLike {
            assignee_agent_id: Some("issue-a".into()),
            assignee_user_id: Some("issue-u".into()),
            ..Default::default()
        };
        let patch_map = Map::new();
        let req = RequestedAssigneePatch::default();
        let input = NextAssigneeIdsInput {
            issue: &issue,
            requested_assignee_patch: &req,
            stage_patch: &patch_map,
        };
        let (a, u) = next_assignee_ids(&input);
        assert_eq!(a.as_deref(), Some("issue-a"));
        assert_eq!(u.as_deref(), Some("issue-u"));
    }

    #[test]
    fn next_assignee_ids_overridden_by_patch() {
        let issue = IssueLike {
            assignee_agent_id: Some("issue-a".into()),
            assignee_user_id: None,
            ..Default::default()
        };
        let req = RequestedAssigneePatch {
            assignee_agent_id: Some("req-a".into()),
            ..Default::default()
        };
        let input = NextAssigneeIdsInput {
            issue: &issue,
            requested_assignee_patch: &req,
            stage_patch: &Map::new(),
        };
        let (a, _) = next_assignee_ids(&input);
        assert_eq!(a.as_deref(), Some("req-a"));
    }

    #[test]
    fn next_assignee_ids_stage_patch_wins() {
        let issue = IssueLike {
            assignee_agent_id: Some("issue-a".into()),
            ..Default::default()
        };
        let req = RequestedAssigneePatch {
            assignee_agent_id: Some("req-a".into()),
            ..Default::default()
        };
        let mut patch_map = Map::new();
        patch_map.insert("assigneeAgentId".into(), Value::String("stage-a".into()));
        let input = NextAssigneeIdsInput {
            issue: &issue,
            requested_assignee_patch: &req,
            stage_patch: &patch_map,
        };
        let (a, _) = next_assignee_ids(&input);
        assert_eq!(a.as_deref(), Some("stage-a"));
    }

    #[test]
    fn next_assignee_ids_stage_null_clears() {
        let issue = IssueLike {
            assignee_agent_id: Some("issue-a".into()),
            ..Default::default()
        };
        let mut patch_map = Map::new();
        patch_map.insert("assigneeAgentId".into(), Value::Null);
        let input = NextAssigneeIdsInput {
            issue: &issue,
            requested_assignee_patch: &RequestedAssigneePatch::default(),
            stage_patch: &patch_map,
        };
        let (a, _) = next_assignee_ids(&input);
        assert!(a.is_none());
    }

    // ----- stripMonitorFromExecutionPolicy / setIssueExecutionPolicyMonitorScheduledBy -----

    #[test]
    fn strip_monitor_from_execution_policy_none() {
        assert!(strip_monitor_from_execution_policy(None).is_none());
    }

    #[test]
    fn strip_monitor_from_execution_policy_no_monitor() {
        let p = make_policy(vec![make_stage("s1", IssueExecutionStageType::Agent, "a1")]);
        let out = strip_monitor_from_execution_policy(Some(&p)).unwrap();
        assert!(out.monitor.is_none());
        assert_eq!(out.stages.len(), 1);
    }

    #[test]
    fn strip_monitor_from_execution_policy_with_monitor_and_stages() {
        let p = IssueExecutionPolicy {
            mode: Some(IssueExecutionPolicyMode::Normal),
            comment_required: true,
            stages: vec![make_stage("s1", IssueExecutionStageType::Agent, "a1")],
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "2025-01-01T00:00:00Z".into(),
                notes: Some("note".into()),
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            max_review_rounds: None,
        };
        let out = strip_monitor_from_execution_policy(Some(&p)).unwrap();
        assert!(out.monitor.is_none());
        assert_eq!(out.stages.len(), 1);
    }

    #[test]
    fn strip_monitor_from_execution_policy_only_monitor() {
        let p = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "2025-01-01T00:00:00Z".into(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            stages: vec![],
            ..Default::default()
        };
        assert!(strip_monitor_from_execution_policy(Some(&p)).is_none());
    }

    #[test]
    fn set_monitor_scheduled_by_patches_value() {
        let p = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "t".into(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            stages: vec![],
            ..Default::default()
        };
        let out = set_issue_execution_policy_monitor_scheduled_by(
            Some(&p),
            IssueMonitorScheduledBy::Board,
        )
        .unwrap();
        assert_eq!(
            out.monitor.as_ref().unwrap().scheduled_by,
            IssueMonitorScheduledBy::Board
        );
    }

    #[test]
    fn set_monitor_scheduled_by_noop_without_monitor() {
        let p = make_policy(vec![make_stage("s1", IssueExecutionStageType::Agent, "a1")]);
        let out = set_issue_execution_policy_monitor_scheduled_by(
            Some(&p),
            IssueMonitorScheduledBy::Board,
        );
        assert!(out.is_some());
        assert!(out.unwrap().monitor.is_none());
    }

    // ----- issueAllowsMonitor / monitorClearReasonForIssue -----

    #[test]
    fn issue_allows_monitor_in_progress_with_agent() {
        assert!(issue_allows_monitor("in_progress", Some("a1"), None));
    }

    #[test]
    fn issue_allows_monitor_in_review_with_agent() {
        assert!(issue_allows_monitor("in_review", Some("a1"), None));
    }

    #[test]
    fn issue_does_not_allow_monitor_with_user() {
        assert!(!issue_allows_monitor("in_progress", Some("a1"), Some("u1")));
    }

    #[test]
    fn issue_does_not_allow_monitor_wrong_status() {
        assert!(!issue_allows_monitor("todo", Some("a1"), None));
        assert!(!issue_allows_monitor("done", Some("a1"), None));
    }

    #[test]
    fn issue_does_not_allow_monitor_without_agent() {
        assert!(!issue_allows_monitor("in_progress", None, None));
    }

    #[test]
    fn monitor_clear_reason_done() {
        let r = monitor_clear_reason_for_issue("done", Some("a1"), None);
        assert_eq!(r, Some(IssueExecutionMonitorClearReason::Completed));
    }

    #[test]
    fn monitor_clear_reason_cancelled() {
        let r = monitor_clear_reason_for_issue("cancelled", Some("a1"), None);
        assert_eq!(r, Some(IssueExecutionMonitorClearReason::Cancelled));
    }

    #[test]
    fn monitor_clear_reason_invalid_status() {
        let r = monitor_clear_reason_for_issue("todo", Some("a1"), None);
        assert_eq!(r, Some(IssueExecutionMonitorClearReason::Expired));
    }

    #[test]
    fn monitor_clear_reason_invalid_assignee() {
        let r = monitor_clear_reason_for_issue("in_progress", None, Some("u1"));
        assert_eq!(r, Some(IssueExecutionMonitorClearReason::Stale));
    }

    #[test]
    fn monitor_clear_reason_valid() {
        assert!(monitor_clear_reason_for_issue("in_progress", Some("a1"), None).is_none());
    }

    // ----- parseMonitorDate / exhaustedMonitorClearReason -----

    #[test]
    fn parse_monitor_date_valid() {
        let d = parse_monitor_date(Some("2025-01-01T00:00:00Z")).unwrap();
        assert_eq!(d.to_rfc3339(), "2025-01-01T00:00:00+00:00");
    }

    #[test]
    fn parse_monitor_date_invalid() {
        assert!(parse_monitor_date(Some("not a date")).is_none());
        assert!(parse_monitor_date(None).is_none());
    }

    #[test]
    fn exhausted_monitor_clear_reason_timeout() {
        let now = utc_dt(2025, 6, 1, 0, 0, 0);
        let m = IssueExecutionMonitorPolicy {
            next_check_at: "t".into(),
            notes: None,
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: Some("2025-05-01T00:00:00Z".into()),
            max_attempts: None,
            recovery_policy: None,
        };
        let input = ExhaustedMonitorClearReasonInput {
            monitor: &m,
            attempt_count: 0,
            now,
        };
        assert_eq!(
            exhausted_monitor_clear_reason(&input),
            Some(IssueExecutionMonitorClearReason::Expired)
        );
    }

    #[test]
    fn exhausted_monitor_clear_reason_max_attempts() {
        let now = utc_dt(2025, 1, 1, 0, 0, 0);
        let m = IssueExecutionMonitorPolicy {
            next_check_at: "t".into(),
            notes: None,
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: None,
            max_attempts: Some(3),
            recovery_policy: None,
        };
        let input = ExhaustedMonitorClearReasonInput {
            monitor: &m,
            attempt_count: 3,
            now,
        };
        assert_eq!(
            exhausted_monitor_clear_reason(&input),
            Some(IssueExecutionMonitorClearReason::Exhausted)
        );
    }

    #[test]
    fn exhausted_monitor_clear_reason_none() {
        let now = utc_dt(2025, 1, 1, 0, 0, 0);
        let m = IssueExecutionMonitorPolicy {
            next_check_at: "t".into(),
            notes: None,
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: Some("2026-01-01T00:00:00Z".into()),
            max_attempts: Some(10),
            recovery_policy: None,
        };
        let input = ExhaustedMonitorClearReasonInput {
            monitor: &m,
            attempt_count: 0,
            now,
        };
        assert!(exhausted_monitor_clear_reason(&input).is_none());
    }

    // ----- State builders -----

    #[test]
    fn build_completed_state_appends_stage() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let state = build_completed_state(None, &stage);
        assert_eq!(state.status, IssueExecutionStateStatus::Completed);
        assert_eq!(state.completed_stage_ids, vec!["s1".to_string()]);
        assert_eq!(state.last_decision_outcome.as_deref(), Some("approved"));
        assert_eq!(state.changes_requested_count, Some(0));
    }

    #[test]
    fn build_completed_state_dedupes_completed_stage_ids() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let prev = IssueExecutionState {
            completed_stage_ids: vec!["s1".into()],
            return_assignee: Some(IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: Some("returner".into()),
                user_id: None,
            }),
            ..Default::default()
        };
        let state = build_completed_state(Some(&prev), &stage);
        assert_eq!(state.completed_stage_ids, vec!["s1".to_string()]);
        assert_eq!(
            state
                .return_assignee
                .as_ref()
                .and_then(|p| p.agent_id.clone()),
            Some("returner".into())
        );
    }

    #[test]
    fn build_state_with_completed_stages_keeps_status() {
        let prev = IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("current".into()),
            ..Default::default()
        };
        let input = BuildStateWithCompletedStagesInput {
            previous: Some(&prev),
            completed_stage_ids: vec!["s1".into(), "s2".into()],
            return_assignee: None,
        };
        let state = build_state_with_completed_stages(&input);
        assert_eq!(state.status, IssueExecutionStateStatus::Pending);
        assert_eq!(state.current_stage_id.as_deref(), Some("current"));
        assert_eq!(
            state.completed_stage_ids,
            vec!["s1".to_string(), "s2".to_string()]
        );
    }

    #[test]
    fn build_skipped_stage_completed_state_status() {
        let prev = IssueExecutionState::default();
        let input = BuildStateWithCompletedStagesInput {
            previous: Some(&prev),
            completed_stage_ids: vec!["s1".into()],
            return_assignee: Some(IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: Some("r".into()),
                user_id: None,
            }),
        };
        let state = build_skipped_stage_completed_state(&input);
        assert_eq!(state.status, IssueExecutionStateStatus::Completed);
        assert_eq!(state.completed_stage_ids, vec!["s1".to_string()]);
        assert_eq!(state.changes_requested_count, Some(0));
    }

    #[test]
    fn build_pending_state_basic() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let participant = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let input = BuildPendingStateInput {
            previous: None,
            stage: &stage,
            stage_index: 0,
            participant: participant.clone(),
            return_assignee: None,
            review_request: None,
            changes_requested_count: None,
        };
        let state = build_pending_state(&input);
        assert_eq!(state.status, IssueExecutionStateStatus::Pending);
        assert_eq!(state.current_stage_id.as_deref(), Some("s1"));
        assert_eq!(state.current_stage_index, Some(0));
        assert_eq!(
            state.current_stage_type,
            Some(IssueExecutionStageType::Agent)
        );
        assert_eq!(state.current_participant, Some(participant));
        assert_eq!(state.changes_requested_count, Some(0));
    }

    #[test]
    fn build_pending_state_preserves_completed() {
        let prev = IssueExecutionState {
            completed_stage_ids: vec!["s0".into()],
            ..Default::default()
        };
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let participant = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let input = BuildPendingStateInput {
            previous: Some(&prev),
            stage: &stage,
            stage_index: 1,
            participant,
            return_assignee: None,
            review_request: None,
            changes_requested_count: None,
        };
        let state = build_pending_state(&input);
        assert_eq!(state.completed_stage_ids, vec!["s0".to_string()]);
        assert_eq!(state.current_stage_index, Some(1));
    }

    #[test]
    fn build_pending_state_uses_changes_requested_count() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let participant = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let input = BuildPendingStateInput {
            previous: None,
            stage: &stage,
            stage_index: 0,
            participant,
            return_assignee: None,
            review_request: None,
            changes_requested_count: Some(2),
        };
        let state = build_pending_state(&input);
        assert_eq!(state.changes_requested_count, Some(2));
    }

    #[test]
    fn build_changes_requested_state_overrides() {
        let prev = IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            ..Default::default()
        };
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let state = build_changes_requested_state(&prev, &stage, 2);
        assert_eq!(state.status, IssueExecutionStateStatus::ChangesRequested);
        assert_eq!(
            state.last_decision_outcome.as_deref(),
            Some("changes_requested")
        );
        assert_eq!(state.changes_requested_count, Some(2));
        assert!(state.review_request.is_none());
    }

    // ----- Patch builders -----

    #[test]
    fn build_pending_stage_patch_sets_status_and_assignee() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let participant = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let policy = make_policy(vec![stage.clone()]);
        let input = BuildPendingStagePatchInput {
            patch: Map::new(),
            previous: None,
            policy: &policy,
            stage: &stage,
            participant: &participant,
            return_assignee: None,
            review_request: None,
            changes_requested_count: None,
        };
        let patch = build_pending_stage_patch(&input);
        assert_eq!(
            patch.get("status"),
            Some(&Value::String("in_review".into()))
        );
        assert_eq!(
            patch.get("assigneeAgentId"),
            Some(&Value::String("a1".into()))
        );
        assert_eq!(patch.get("assigneeUserId"), Some(&Value::Null));
        let exec_state = patch.get("executionState").unwrap();
        assert_eq!(exec_state["status"], json!("pending"));
        assert_eq!(exec_state["currentStageType"], json!("agent"));
    }

    #[test]
    fn clear_execution_state_patch_keeps_state_when_status_change() {
        let return_assignee = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let input = ClearExecutionStatePatchInput {
            patch: Map::new(),
            issue_status: "in_review",
            requested_status: Some("done"),
            return_assignee: Some(return_assignee),
        };
        let patch = clear_execution_state_patch(&input);
        // requested_status is Some("done"), so no status change to in_progress
        assert_eq!(patch.get("executionState"), Some(&Value::Null));
        assert!(patch.get("status").is_none());
    }

    #[test]
    fn clear_execution_state_patch_reverts_to_in_progress() {
        let return_assignee = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let input = ClearExecutionStatePatchInput {
            patch: Map::new(),
            issue_status: "in_review",
            requested_status: None,
            return_assignee: Some(return_assignee),
        };
        let patch = clear_execution_state_patch(&input);
        assert_eq!(
            patch.get("status"),
            Some(&Value::String("in_progress".into()))
        );
        assert_eq!(
            patch.get("assigneeAgentId"),
            Some(&Value::String("a1".into()))
        );
    }

    #[test]
    fn can_auto_skip_pending_stage_basic() {
        // Single agent participant == return assignee
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let return_assignee = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let input = CanAutoSkipPendingStageInput {
            stage: &stage,
            return_assignee: Some(return_assignee),
            requested_status: Some("done"),
        };
        assert!(can_auto_skip_pending_stage(&input));
    }

    #[test]
    fn can_auto_skip_pending_stage_wrong_status() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let return_assignee = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let input = CanAutoSkipPendingStageInput {
            stage: &stage,
            return_assignee: Some(return_assignee),
            requested_status: Some("todo"),
        };
        assert!(!can_auto_skip_pending_stage(&input));
    }

    #[test]
    fn can_auto_skip_pending_stage_different_assignee() {
        let stage = make_stage("s1", IssueExecutionStageType::Agent, "a1");
        let return_assignee = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a2".into()),
            user_id: None,
        };
        let input = CanAutoSkipPendingStageInput {
            stage: &stage,
            return_assignee: Some(return_assignee),
            requested_status: Some("done"),
        };
        assert!(!can_auto_skip_pending_stage(&input));
    }

    // ----- Monitor state builders -----

    #[test]
    fn build_scheduled_monitor_state_basic() {
        let monitor = IssueExecutionMonitorPolicy {
            next_check_at: "2025-01-01T00:00:00Z".into(),
            notes: Some("hello".into()),
            scheduled_by: IssueMonitorScheduledBy::Board,
            kind: Some(IssueExecutionMonitorKind::ExternalService),
            service_name: Some("svc".into()),
            external_ref: Some("https://x".into()),
            timeout_at: None,
            max_attempts: None,
            recovery_policy: None,
        };
        let state = build_scheduled_monitor_state(None, &monitor);
        assert_eq!(state.status, IssueExecutionMonitorStateStatus::Scheduled);
        assert_eq!(state.next_check_at.as_deref(), Some("2025-01-01T00:00:00Z"));
        assert_eq!(state.scheduled_by, Some(IssueMonitorScheduledBy::Board));
        assert_eq!(state.kind, Some(IssueExecutionMonitorKind::ExternalService));
        assert_eq!(state.external_ref.as_deref(), Some("[redacted]"));
        assert_eq!(state.attempt_count, 0);
    }

    #[test]
    fn build_triggered_monitor_state_increments_attempt() {
        let prev = IssueExecutionMonitorState {
            attempt_count: 5,
            scheduled_by: Some(IssueMonitorScheduledBy::Assignee),
            notes: Some("note".into()),
            ..Default::default()
        };
        let triggered_at = utc_dt(2025, 1, 2, 0, 0, 0);
        let input = BuildTriggeredMonitorStateInput {
            previous: Some(&prev),
            triggered_at,
        };
        let state = build_triggered_monitor_state(&input);
        assert_eq!(state.status, IssueExecutionMonitorStateStatus::Running);
        assert_eq!(state.attempt_count, 6);
        assert_eq!(
            state.last_triggered_at.as_deref(),
            Some("2025-01-02T00:00:00.000Z")
        );
        assert_eq!(state.notes.as_deref(), Some("note"));
        assert_eq!(state.scheduled_by, Some(IssueMonitorScheduledBy::Assignee));
    }

    #[test]
    fn build_cleared_monitor_state_preserves_metadata() {
        let prev = IssueExecutionMonitorState {
            attempt_count: 2,
            last_triggered_at: Some("2025-01-01T00:00:00Z".into()),
            scheduled_by: Some(IssueMonitorScheduledBy::Board),
            notes: Some("n".into()),
            ..Default::default()
        };
        let cleared_at = utc_dt(2025, 2, 1, 0, 0, 0);
        let input = BuildClearedMonitorStateInput {
            previous: Some(&prev),
            clear_reason: IssueExecutionMonitorClearReason::Completed,
            cleared_at,
        };
        let state = build_cleared_monitor_state(&input);
        assert_eq!(state.status, IssueExecutionMonitorStateStatus::Cleared);
        assert_eq!(state.attempt_count, 2);
        assert_eq!(
            state.cleared_at.as_deref(),
            Some("2025-02-01T00:00:00.000Z")
        );
        assert_eq!(
            state.clear_reason,
            Some(IssueExecutionMonitorClearReason::Completed)
        );
    }

    #[test]
    fn derive_persisted_monitor_state_scheduled() {
        let monitor = IssueExecutionMonitorPolicy {
            next_check_at: "2025-03-01T00:00:00Z".into(),
            notes: Some("note".into()),
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: None,
            max_attempts: None,
            recovery_policy: None,
        };
        let issue = IssueLike {
            monitor_next_check_at: Some(utc_dt(2025, 3, 15, 0, 0, 0)),
            ..Default::default()
        };
        let policy = IssueExecutionPolicy {
            monitor: Some(monitor),
            ..Default::default()
        };
        let input = DerivePersistedMonitorStateInput {
            issue: &issue,
            state: None,
            policy: Some(&policy),
        };
        let state = derive_persisted_monitor_state(&input).unwrap();
        assert_eq!(state.status, IssueExecutionMonitorStateStatus::Scheduled);
        // issue.monitorNextCheckAt wins over policy
        assert_eq!(
            state.next_check_at.as_deref(),
            Some("2025-03-15T00:00:00.000Z")
        );
    }

    #[test]
    fn derive_persisted_monitor_state_cleared_preserved() {
        let cleared_state = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Cleared,
            attempt_count: 5,
            notes: Some("cleared".into()),
            cleared_at: Some("2025-01-01T00:00:00Z".into()),
            clear_reason: Some(IssueExecutionMonitorClearReason::Completed),
            ..Default::default()
        };
        let exec_state = IssueExecutionState {
            monitor: Some(cleared_state.clone()),
            ..Default::default()
        };
        let issue = IssueLike {
            monitor_notes: Some("override".into()),
            ..Default::default()
        };
        let input = DerivePersistedMonitorStateInput {
            issue: &issue,
            state: Some(&exec_state),
            policy: None,
        };
        let state = derive_persisted_monitor_state(&input).unwrap();
        assert_eq!(state.status, IssueExecutionMonitorStateStatus::Cleared);
        assert_eq!(state.attempt_count, 5);
        assert_eq!(state.notes.as_deref(), Some("override"));
        assert_eq!(
            state.clear_reason,
            Some(IssueExecutionMonitorClearReason::Completed)
        );
    }

    #[test]
    fn derive_persisted_monitor_state_triggered_fallback() {
        let issue = IssueLike {
            monitor_attempt_count: Some(3),
            ..Default::default()
        };
        let input = DerivePersistedMonitorStateInput {
            issue: &issue,
            state: None,
            policy: None,
        };
        let state = derive_persisted_monitor_state(&input).unwrap();
        assert_eq!(state.status, IssueExecutionMonitorStateStatus::Running);
        assert_eq!(state.attempt_count, 3);
    }

    #[test]
    fn derive_persisted_monitor_state_no_inputs() {
        let issue = IssueLike::default();
        let input = DerivePersistedMonitorStateInput {
            issue: &issue,
            state: None,
            policy: None,
        };
        assert!(derive_persisted_monitor_state(&input).is_none());
    }

    // ----- blank_execution_state -----

    #[test]
    fn blank_execution_state_idle() {
        let s = blank_execution_state();
        assert_eq!(s.status, IssueExecutionStateStatus::Idle);
        assert!(s.completed_stage_ids.is_empty());
        assert!(s.monitor.is_none());
    }

    // ----- integration: pending + completion flow -----

    #[test]
    fn integration_pending_to_completed() {
        let stages = vec![
            make_stage("s1", IssueExecutionStageType::Agent, "a1"),
            make_stage("s2", IssueExecutionStageType::User, "u1"),
        ];
        let policy = make_policy(stages.clone());
        // First, pending on s1
        let participant_a1 = IssueExecutionStagePrincipal {
            principal_type: "agent".to_string(),
            agent_id: Some("a1".into()),
            user_id: None,
        };
        let pending = build_pending_state(&BuildPendingStateInput {
            previous: None,
            stage: &stages[0],
            stage_index: 0,
            participant: participant_a1.clone(),
            return_assignee: None,
            review_request: None,
            changes_requested_count: None,
        });
        assert_eq!(pending.current_stage_id.as_deref(), Some("s1"));

        // Complete s1
        let completed = build_completed_state(Some(&pending), &stages[0]);
        assert_eq!(completed.completed_stage_ids, vec!["s1".to_string()]);
        assert_eq!(completed.status, IssueExecutionStateStatus::Completed);

        // Next pending should be s2
        let next = next_pending_stage(&policy, Some(&completed)).unwrap();
        assert_eq!(next.id.as_deref(), Some("s2"));
    }

    #[test]
    fn integration_select_participant_after_strip_monitor() {
        let mut policy = make_policy(vec![make_stage("s1", IssueExecutionStageType::Agent, "a1")]);
        policy.monitor = Some(IssueExecutionMonitorPolicy {
            next_check_at: "2025-01-01T00:00:00Z".into(),
            notes: None,
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: None,
            max_attempts: None,
            recovery_policy: None,
        });
        let stripped = strip_monitor_from_execution_policy(Some(&policy)).unwrap();
        assert!(stripped.monitor.is_none());
        // Stage should still have its participants
        let p = select_stage_participant(&stripped.stages[0], None).unwrap();
        assert_eq!(p.agent_id.as_deref(), Some("a1"));
    }
}
