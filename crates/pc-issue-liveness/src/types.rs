//! 输入输出类型 — 与 Node `services/recovery/issue-graph-liveness.ts` 1:1 对齐。
//!
//! 所有类型保持字段命名（snake_case → camelCase）与 Node 兼容，便于上层
//! HTTP/CLI 直接复用。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_core::Timestamp;

// -----------------------------------------------------------------------------
// Liveness state enum + severity
// -----------------------------------------------------------------------------

/// Liveness 发现严重程度（与 Node `IssueLivenessSeverity` 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueLivenessSeverity {
    Warning,
    Critical,
}

impl IssueLivenessSeverity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// Issue graph liveness 状态（与 Node `IssueLivenessState` 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueLivenessState {
    BlockedByUnassignedIssue,
    BlockedByAssignedBacklogIssue,
    BlockedByUninvokableAssignee,
    BlockedByCancelledIssue,
    InvalidReviewParticipant,
    InReviewWithoutActionPath,
}

impl IssueLivenessState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlockedByUnassignedIssue => "blocked_by_unassigned_issue",
            Self::BlockedByAssignedBacklogIssue => "blocked_by_assigned_backlog_issue",
            Self::BlockedByUninvokableAssignee => "blocked_by_uninvokable_assignee",
            Self::BlockedByCancelledIssue => "blocked_by_cancelled_issue",
            Self::InvalidReviewParticipant => "invalid_review_participant",
            Self::InReviewWithoutActionPath => "in_review_without_action_path",
        }
    }
}

// -----------------------------------------------------------------------------
// Input types
// -----------------------------------------------------------------------------

/// Issue 输入投影（与 Node `IssueLivenessIssueInput` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessIssueInput {
    pub id: Uuid,
    pub company_id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_agent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_agent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_policy: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_next_check_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_attempt_count: Option<i32>,
}

/// Blocker/blocked 关系输入（与 Node `IssueLivenessRelationInput` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessRelationInput {
    pub company_id: Uuid,
    pub blocker_issue_id: Uuid,
    pub blocked_issue_id: Uuid,
}

/// Agent 输入投影（与 Node `IssueLivenessAgentInput` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessAgentInput {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reports_to: Option<Uuid>,
}

/// Active run / queued wake 路径输入（与 Node `IssueLivenessExecutionPathInput` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessExecutionPathInput {
    pub company_id: Uuid,
    pub issue_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    pub status: String,
}

/// Pending interaction/approval/recovery-issue 等待路径输入（与 Node `IssueLivenessWaitingPathInput` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessWaitingPathInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub status: String,
}

/// 依赖路径条目（与 Node `IssueLivenessDependencyPathEntry` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessDependencyPathEntry {
    pub issue_id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
}

/// Owner candidate reason（与 Node `IssueLivenessOwnerCandidateReason` 1:1）。
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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StalledBlockerAssignee => "stalled_blocker_assignee",
            Self::AssigneeReportingChain => "assignee_reporting_chain",
            Self::CreatorReportingChain => "creator_reporting_chain",
            Self::RootAgent => "root_agent",
            Self::OrderedInvokableFallback => "ordered_invokable_fallback",
        }
    }
}

/// Owner candidate（与 Node `IssueLivenessOwnerCandidate` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessOwnerCandidate {
    pub agent_id: Uuid,
    pub reason: IssueLivenessOwnerCandidateReason,
    pub source_issue_id: Uuid,
}

/// Liveness 发现结果（与 Node `IssueLivenessFinding` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessFinding {
    pub issue_id: Uuid,
    pub company_id: Uuid,
    pub identifier: Option<String>,
    pub state: IssueLivenessState,
    pub severity: IssueLivenessSeverity,
    pub reason: String,
    pub dependency_path: Vec<IssueLivenessDependencyPathEntry>,
    pub recovery_issue_id: Uuid,
    pub recommended_owner_agent_id: Option<Uuid>,
    pub recommended_owner_candidate_agent_ids: Vec<Uuid>,
    pub recommended_owner_candidates: Vec<IssueLivenessOwnerCandidate>,
    pub recommended_action: String,
    pub incident_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_agent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker_issue_id: Option<Uuid>,
}

/// 整体分类输入（与 Node `IssueGraphLivenessInput` 1:1）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueGraphLivenessInput {
    pub issues: Vec<IssueLivenessIssueInput>,
    pub relations: Vec<IssueLivenessRelationInput>,
    pub agents: Vec<IssueLivenessAgentInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_runs: Option<Vec<IssueLivenessExecutionPathInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_wake_requests: Option<Vec<IssueLivenessExecutionPathInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_interactions: Option<Vec<IssueLivenessWaitingPathInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_approvals: Option<Vec<IssueLivenessWaitingPathInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_recovery_issues: Option<Vec<IssueLivenessWaitingPathInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<Timestamp>,
}
