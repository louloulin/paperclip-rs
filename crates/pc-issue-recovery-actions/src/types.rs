//! Service DTOs — 与 Node `services/issue-recovery-actions.ts` 1:1 对齐。
//!
//! 设计：
//! - `IssueRecoveryActionInfo`：service 暴露的 DTO（`toReadModel` 输出）
//! - `UpsertIssueRecoveryActionRequest`：upsert 输入
//! - `ResolveIssueRecoveryActionRequest`：resolve 输入
//! - `IssueRecoveryActionError`：service 错误

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_repos::issue::{
    IssueRecoveryActionRow, UpsertRecoveryAction,
};

// -----------------------------------------------------------------------------
// Constants (与 Node ACTIVE_RECOVERY_ACTION_STATUSES / MAX_UPSERT_RETRIES 对齐)
// -----------------------------------------------------------------------------

/// `ACTIVE_RECOVERY_ACTION_STATUSES` — 与 Node 1:1 对齐。
pub const ACTIVE_RECOVERY_ACTION_STATUSES: &[&str] = &["active", "escalated"];

/// `MAX_UPSERT_RETRIES` — 与 Node 1:1 对齐。
pub const MAX_UPSERT_RETRIES: u32 = 3;

/// Recovery action 合法 status 集合（用于校验）。
pub const VALID_RECOVERY_ACTION_STATUSES: &[&str] = &[
    "active",
    "escalated",
    "resolved",
    "cancelled",
    "expired",
    "stale",
];

/// Recovery action 合法 outcome 集合。
pub const VALID_RECOVERY_ACTION_OUTCOMES: &[&str] = &[
    "fixed",
    "superseded",
    "no_longer_needed",
    "manual_resolution",
    "timeout",
    "exhausted",
];

/// Recovery action 合法 owner_type 集合。
pub const VALID_RECOVERY_ACTION_OWNER_TYPES: &[&str] = &[
    "agent",
    "user",
    "system",
    "board",
];

/// Recovery action 合法 kind 集合。
pub const VALID_RECOVERY_ACTION_KINDS: &[&str] = &[
    "stranded_issue_recovery",
    "stale_active_run_recovery",
    "review_round_exhausted",
    "blocked_dependency_recovery",
    "monitor_cleared_recovery",
    "manual",
    "approval_required_recovery",
    "escalation",
];

// -----------------------------------------------------------------------------
// Service DTO
// -----------------------------------------------------------------------------

/// Service 暴露的 recovery action DTO（与 Node `IssueRecoveryAction` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRecoveryActionInfo {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub recovery_issue_id: Option<Uuid>,
    pub kind: String,
    pub status: String,
    pub owner_type: String,
    pub owner_agent_id: Option<Uuid>,
    pub owner_user_id: Option<String>,
    pub previous_owner_agent_id: Option<Uuid>,
    pub return_owner_agent_id: Option<Uuid>,
    pub cause: String,
    pub fingerprint: String,
    pub evidence: serde_json::Value,
    pub next_action: String,
    pub wake_policy: Option<serde_json::Value>,
    pub monitor_policy: Option<serde_json::Value>,
    pub attempt_count: i32,
    pub max_attempts: Option<i32>,
    pub timeout_at: Option<pc_core::Timestamp>,
    pub last_attempt_at: Option<pc_core::Timestamp>,
    pub outcome: Option<String>,
    pub resolution_note: Option<String>,
    pub resolved_at: Option<pc_core::Timestamp>,
    pub created_at: pc_core::Timestamp,
    pub updated_at: pc_core::Timestamp,
}

impl IssueRecoveryActionInfo {
    /// 从 DB 行转换为 service DTO（与 Node `toReadModel` 1:1 对齐）。
    pub fn from_row(row: IssueRecoveryActionRow) -> Self {
        Self {
            id: row.id,
            company_id: row.company_id,
            source_issue_id: row.source_issue_id,
            recovery_issue_id: row.recovery_issue_id,
            kind: row.kind,
            status: row.status,
            owner_type: row.owner_type,
            owner_agent_id: row.owner_agent_id,
            owner_user_id: row.owner_user_id,
            previous_owner_agent_id: row.previous_owner_agent_id,
            return_owner_agent_id: row.return_owner_agent_id,
            cause: row.cause,
            fingerprint: row.fingerprint,
            evidence: row.evidence,
            next_action: row.next_action,
            wake_policy: row.wake_policy,
            monitor_policy: row.monitor_policy,
            attempt_count: row.attempt_count,
            max_attempts: row.max_attempts,
            timeout_at: row.timeout_at,
            last_attempt_at: row.last_attempt_at,
            outcome: row.outcome,
            resolution_note: row.resolution_note,
            resolved_at: row.resolved_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }

    pub fn is_active(&self) -> bool {
        ACTIVE_RECOVERY_ACTION_STATUSES.contains(&self.status.as_str())
    }

    pub fn is_resolved(&self) -> bool {
        matches!(self.status.as_str(), "resolved" | "cancelled" | "expired" | "stale")
    }
}

/// Upsert 输入（与 Node `UpsertIssueRecoveryActionInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertIssueRecoveryActionRequest {
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub recovery_issue_id: Option<Uuid>,
    pub kind: String,
    pub owner_type: Option<String>,
    pub owner_agent_id: Option<Uuid>,
    pub owner_user_id: Option<String>,
    pub previous_owner_agent_id: Option<Uuid>,
    pub return_owner_agent_id: Option<Uuid>,
    pub cause: String,
    pub fingerprint: String,
    pub evidence: Option<serde_json::Value>,
    pub next_action: String,
    pub wake_policy: Option<serde_json::Value>,
    pub monitor_policy: Option<serde_json::Value>,
    pub max_attempts: Option<i32>,
    pub timeout_at: Option<pc_core::Timestamp>,
    pub last_attempt_at: Option<pc_core::Timestamp>,
}

impl UpsertIssueRecoveryActionRequest {
    /// 校验必填字段。
    pub fn validate(&self) -> Result<(), String> {
        if !VALID_RECOVERY_ACTION_KINDS.contains(&self.kind.as_str()) {
            return Err(format!("invalid kind: {}", self.kind));
        }
        if let Some(ref owner_type) = self.owner_type {
            if !VALID_RECOVERY_ACTION_OWNER_TYPES.contains(&owner_type.as_str()) {
                return Err(format!("invalid owner_type: {owner_type}"));
            }
        }
        if self.cause.is_empty() {
            return Err("cause is required".to_string());
        }
        if self.fingerprint.is_empty() {
            return Err("fingerprint is required".to_string());
        }
        if self.next_action.is_empty() {
            return Err("next_action is required".to_string());
        }
        Ok(())
    }

    /// 转换为 pc-repos `UpsertRecoveryAction`。
    pub fn to_repo_input(&self) -> UpsertRecoveryAction {
        UpsertRecoveryAction {
            company_id: self.company_id,
            source_issue_id: self.source_issue_id,
            recovery_issue_id: self.recovery_issue_id,
            kind: self.kind.clone(),
            owner_type: self.owner_type.clone(),
            owner_agent_id: self.owner_agent_id,
            owner_user_id: self.owner_user_id.clone(),
            previous_owner_agent_id: self.previous_owner_agent_id,
            return_owner_agent_id: self.return_owner_agent_id,
            cause: self.cause.clone(),
            fingerprint: self.fingerprint.clone(),
            evidence: self.evidence.clone(),
            next_action: self.next_action.clone(),
            wake_policy: self.wake_policy.clone(),
            monitor_policy: self.monitor_policy.clone(),
            max_attempts: self.max_attempts,
            timeout_at: self.timeout_at,
            last_attempt_at: self.last_attempt_at,
        }
    }
}

/// Resolve 输入（与 Node `ResolveIssueRecoveryActionInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveIssueRecoveryActionRequest {
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub action_id: Option<Uuid>,
    pub kind: Option<String>,
    pub cause: Option<String>,
    pub fingerprint: Option<String>,
    pub status: String,
    pub outcome: String,
    pub resolution_note: Option<String>,
}

impl ResolveIssueRecoveryActionRequest {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.status.as_str(), "resolved" | "cancelled") {
            return Err(format!(
                "invalid status for resolve: {} (must be resolved | cancelled)",
                self.status
            ));
        }
        if !VALID_RECOVERY_ACTION_OUTCOMES.contains(&self.outcome.as_str()) {
            return Err(format!("invalid outcome: {}", self.outcome));
        }
        Ok(())
    }
}

/// 错误类型。
#[derive(Debug, thiserror::Error)]
pub enum IssueRecoveryActionError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] pc_errors::Error),
}

pub type IssueRecoveryActionResult<T> = std::result::Result<T, IssueRecoveryActionError>;

// -----------------------------------------------------------------------------
// Multi-source DTO（用于 list_active_for_issues）
// -----------------------------------------------------------------------------

/// `list_active_for_issues` 返回的 map 别名。
pub type ActiveRecoveryActionsByIssue = HashMap<Uuid, IssueRecoveryActionInfo>;

