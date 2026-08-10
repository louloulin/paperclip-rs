//! Types —— Issue thread interaction DTOs and constants.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use pc_repos::issue::IssueThreadInteractionRow;

/// 所有合法 interaction kinds（与 Node 5 类交互 1:1 对齐）。
pub const INTERACTION_KINDS: &[&str] = &[
    "ask_user_questions",
    "request_confirmation",
    "request_checkbox_confirmation",
    "request_item_verdicts",
    "suggest_tasks",
];

/// 所有合法 interaction statuses（与 Node 9 类状态 1:1 对齐）。
pub const INTERACTION_STATUSES: &[&str] = &[
    "pending",
    "accepted",
    "rejected",
    "cancelled",
    "withdrawn",
    "answered",
    "responded",
    "blocked",
    "done",
];

/// 终态 statuses（不能再次 resolve）。
pub const INTERACTION_TERMINAL_STATUSES: &[&str] = &[
    "accepted",
    "rejected",
    "cancelled",
    "withdrawn",
    "answered",
    "responded",
    "done",
];

/// Continuation policy（与 Node 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationPolicy {
    None,
    WakeAssignee,
    WakeAssigneeOnAccept,
}

impl ContinuationPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::WakeAssignee => "wake_assignee",
            Self::WakeAssigneeOnAccept => "wake_assignee_on_accept",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "wake_assignee" => Some(Self::WakeAssignee),
            "wake_assignee_on_accept" => Some(Self::WakeAssigneeOnAccept),
            _ => None,
        }
    }
}

/// Interaction status enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    Pending,
    Accepted,
    Rejected,
    Cancelled,
    Withdrawn,
    Answered,
    Responded,
    Blocked,
    Done,
}

impl InteractionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Withdrawn => "withdrawn",
            Self::Answered => "answered",
            Self::Responded => "responded",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "cancelled" => Some(Self::Cancelled),
            "withdrawn" => Some(Self::Withdrawn),
            "answered" => Some(Self::Answered),
            "responded" => Some(Self::Responded),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted
                | Self::Rejected
                | Self::Cancelled
                | Self::Withdrawn
                | Self::Answered
                | Self::Responded
                | Self::Done
        )
    }
}

/// Interaction actor (用于 resolve 操作)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionActor {
    pub actor_type: String, // "user" | "agent" | "system"
    pub actor_id: Option<String>,
}

/// Create input（与 Node `CreateIssueThreadInteraction` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIssueThreadInteractionInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub kind: String,
    pub continuation_policy: ContinuationPolicy,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub payload: Value,
    pub source_comment_id: Option<Uuid>,
    pub source_run_id: Option<Uuid>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub idempotency_key: Option<String>,
}

/// Resolve input（与 Node `resolveInteraction` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveInteractionInput {
    pub interaction_id: Uuid,
    pub new_status: InteractionStatus,
    pub result: Option<Value>,
    pub resolved_by_actor: InteractionActor,
}

/// Submit verdicts input（用于 request_item_verdicts 类型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitVerdictsInput {
    pub interaction_id: Uuid,
    pub verdicts: Value, // Item verdicts JSON
    pub resolved_by_actor: InteractionActor,
}

/// Resolution outcome (与 Node service 返回类型对齐)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionResolution {
    pub interaction: IssueThreadInteractionRow,
    pub continuation_issue_id: Option<Uuid>,
}

/// Service 暴露的 DTO (从 IssueThreadInteractionRow 转换)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueThreadInteractionInfo {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub kind: String,
    pub status: String,
    pub continuation_policy: String,
    pub source_comment_id: Option<Uuid>,
    pub source_run_id: Option<Uuid>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub resolved_by_agent_id: Option<Uuid>,
    pub resolved_by_user_id: Option<String>,
    pub payload: Value,
    pub result: Option<Value>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<IssueThreadInteractionRow> for IssueThreadInteractionInfo {
    fn from(row: IssueThreadInteractionRow) -> Self {
        Self {
            id: row.id,
            company_id: row.company_id,
            issue_id: row.issue_id,
            kind: row.kind,
            status: row.status,
            continuation_policy: row.continuation_policy,
            source_comment_id: row.source_comment_id,
            source_run_id: row.source_run_id,
            title: row.title,
            summary: row.summary,
            created_by_agent_id: row.created_by_agent_id,
            created_by_user_id: row.created_by_user_id,
            resolved_by_agent_id: row.resolved_by_agent_id,
            resolved_by_user_id: row.resolved_by_user_id,
            payload: row.payload,
            result: row.result,
            resolved_at: row.resolved_at.map(|t| t.as_datetime()),
            created_at: row.created_at.as_datetime(),
            updated_at: row.updated_at.as_datetime(),
        }
    }
}

/// List filter（用于 list operations）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListIssueThreadInteractionsFilter {
    pub kind: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

/// Issue thread interaction service 错误。
#[derive(Debug, Error)]
pub enum IssueThreadInteractionError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("repository error: {0}")]
    Repo(String),
}

pub type IssueThreadInteractionResult<T> = Result<T, IssueThreadInteractionError>;

impl From<sqlx::Error> for IssueThreadInteractionError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(format!("sqlx: {e}"))
    }
}

impl From<pc_repos::RepoError> for IssueThreadInteractionError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Repo(e.to_string())
    }
}
