//! 业务服务层类型 — 与 Node `services/issue-execution-policy.ts` 1:1 对齐。
//!
//! 设计：
//! - `ApplyTransitionRequest`：service 输入（issue / policy / previous policy / actor / patches）
//! - `ApplyTransitionOutcome`：service 输出（pc-core TransitionResult + metadata）
//! - `InitialMonitorRequest` / `TriggerMonitorRequest` / `ClearMonitorRequest`：
//!   monitor 三类 patch builder 的输入
//! - `IssueExecutionPolicyError`：service 错误类型

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_core::{IssueExecutionDecision, IssueExecutionPolicy, ReviewRequest};
use pc_repos::issue::IssueRow;

// -----------------------------------------------------------------------------
// Error
// -----------------------------------------------------------------------------

/// Service 错误（与 Node `unprocessable(...)` 1:1）。
#[derive(Debug, thiserror::Error)]
pub enum IssueExecutionPolicyError {
    /// pc-core 拒绝（policy 校验失败 / monitor 越界 / review rounds 用完）
    #[error("policy transition rejected: {message}")]
    Transition {
        message: String,
        clear_reason: Option<String>,
    },
    /// DB 错误
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// 其它 pc-errors
    #[error(transparent)]
    Pc(#[from] pc_errors::Error),
}

impl IssueExecutionPolicyError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Transition {
            message: message.into(),
            clear_reason: None,
        }
    }
}

impl From<pc_core::PolicyTransitionError> for IssueExecutionPolicyError {
    fn from(e: pc_core::PolicyTransitionError) -> Self {
        Self::Transition {
            message: e.message,
            clear_reason: e
                .clear_reason
                .map(|r| serde_json::to_string(&r).unwrap_or_default()),
        }
    }
}

pub type IssueExecutionPolicyResult<T> = std::result::Result<T, IssueExecutionPolicyError>;

// -----------------------------------------------------------------------------
// Actor
// -----------------------------------------------------------------------------

/// 调用方 actor 信息 — 与 Node 端 `ActorLike` 字段对齐。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPolicyActor {
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
}

impl ExecutionPolicyActor {
    pub fn system() -> Self {
        Self::default()
    }
    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            agent_id: None,
            user_id: Some(user_id.into()),
        }
    }
    pub fn agent(agent_id: Uuid) -> Self {
        Self {
            agent_id: Some(agent_id),
            user_id: None,
        }
    }
}

// -----------------------------------------------------------------------------
// Transition request / outcome
// -----------------------------------------------------------------------------

/// Apply transition 请求（与 Node `TransitionInput` 1:1 对齐字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyTransitionRequest {
    /// 当前 issue 行（必须已从 DB 加载）
    pub issue: IssueRow,
    /// 新的 execution policy（可空 → 清除 policy）
    pub policy: Option<IssueExecutionPolicy>,
    /// 上一次的 execution policy（用于 diff）
    pub previous_policy: Option<IssueExecutionPolicy>,
    /// 用户请求的新 status
    pub requested_status: Option<String>,
    /// 用户请求的 assignee 变更
    #[serde(default)]
    pub requested_assignee_patch: RequestedAssigneePatchDto,
    /// 调用方 actor
    pub actor: ExecutionPolicyActor,
    /// 是否允许 board 覆盖 policy
    #[serde(default)]
    pub allow_board_override: bool,
    /// 评论内容
    pub comment_body: Option<String>,
    /// Review request 信息
    pub review_request: Option<ReviewRequest>,
    /// Monitor 是否被显式更新
    #[serde(default)]
    pub monitor_explicitly_updated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestedAssigneePatchDto {
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
}

impl RequestedAssigneePatchDto {
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.assignee_agent_id.is_none() && self.assignee_user_id.is_none()
    }
}

/// Apply transition outcome。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyTransitionOutcome {
    /// pc-core 计算出的 patch（键 → Value），可应用到 IssueRow
    #[serde(skip)]
    pub patch: serde_json::Map<String, serde_json::Value>,
    /// pc-core 计算出的 decision（如有）
    pub decision: Option<IssueExecutionDecision>,
    /// 是否由 workflow 控制了 assignee 赋值
    pub workflow_controlled_assignment: bool,
    /// 输入是否为 monitor-only transition
    pub monitor_only: bool,
}

impl ApplyTransitionOutcome {
    pub fn has_patch(&self) -> bool {
        !self.patch.is_empty()
    }
    /// Apply this outcome's patch to a clone of the input IssueRow.
    pub fn apply_to_row(&self, row: &IssueRow) -> IssueRow {
        let mut row = row.clone();
        for (k, v) in &self.patch {
            apply_field(&mut row, k, v);
        }
        row
    }
}

fn apply_field(row: &mut IssueRow, key: &str, value: &serde_json::Value) {
    match key {
        "status" => {
            if let Some(s) = value.as_str() {
                row.status = s.to_string();
            }
        }
        "assigneeAgentId" => {
            row.assignee_agent_id = value.as_str().and_then(|s| Uuid::parse_str(s).ok());
        }
        "assigneeUserId" => {
            row.assignee_user_id = value.as_str().map(|s| s.to_string());
        }
        "executionPolicy" => {
            row.execution_policy = Some(value.clone());
        }
        "executionState" => {
            row.execution_state = Some(value.clone());
        }
        "monitorNextCheckAt" => {
            row.monitor_next_check_at = value
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| pc_core::Timestamp::from_dt(dt.with_timezone(&chrono::Utc)));
        }
        "monitorWakeRequestedAt" => {
            row.monitor_wake_requested_at = value
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| pc_core::Timestamp::from_dt(dt.with_timezone(&chrono::Utc)));
        }
        "monitorLastTriggeredAt" => {
            row.monitor_last_triggered_at = value
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| pc_core::Timestamp::from_dt(dt.with_timezone(&chrono::Utc)));
        }
        "monitorAttemptCount" => {
            row.monitor_attempt_count = value.as_i64().map(|n| n as i32).unwrap_or(0);
        }
        "monitorNotes" => {
            row.monitor_notes = value.as_str().map(|s| s.to_string());
        }
        "monitorScheduledBy" => {
            row.monitor_scheduled_by = value.as_str().map(|s| s.to_string());
        }
        _ => {}
    }
}

// -----------------------------------------------------------------------------
// Monitor-only requests
// -----------------------------------------------------------------------------

/// `build_initial_issue_monitor_fields` 的 service 输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialMonitorRequest {
    pub policy: Option<IssueExecutionPolicy>,
    pub status: String,
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
}

/// `build_issue_monitor_triggered_patch` 的 service 输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerMonitorRequest {
    pub issue: IssueRow,
    pub policy: Option<IssueExecutionPolicy>,
    pub triggered_at: chrono::DateTime<chrono::Utc>,
}

/// `build_issue_monitor_cleared_patch` 的 service 输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearMonitorRequest {
    pub issue: IssueRow,
    pub policy: Option<IssueExecutionPolicy>,
    pub clear_reason: String,
    pub cleared_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Outcome for monitor-only patches。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorPatchOutcome {
    #[serde(skip)]
    pub patch: serde_json::Map<String, serde_json::Value>,
}

impl MonitorPatchOutcome {
    pub fn has_patch(&self) -> bool {
        !self.patch.is_empty()
    }
    pub fn apply_to_row(&self, row: &IssueRow) -> IssueRow {
        let mut row = row.clone();
        for (k, v) in &self.patch {
            apply_field(&mut row, k, v);
        }
        row
    }
}

#[cfg(test)]
mod apply_to_row_tests {
    use super::*;
    use pc_core::Timestamp;
    use serde_json::json;
    use uuid::Uuid;

    fn fixture_issue() -> IssueRow {
        IssueRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            parent_id: None,
            title: "apply-to-row fixture".to_string(),
            description: None,
            status: "todo".to_string(),
            work_mode: "standard".to_string(),
            harness_kind: None,
            priority: "normal".to_string(),
            assignee_agent_id: None,
            assignee_user_id: None,
            checkout_run_id: None,
            execution_run_id: None,
            execution_agent_name_key: None,
            execution_locked_at: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            issue_number: None,
            identifier: Some("R-1".to_string()),
            origin_kind: "manual".to_string(),
            origin_id: None,
            origin_run_id: None,
            origin_fingerprint: "r753".to_string(),
            request_depth: 0,
            billing_code: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_state: None,
            monitor_next_check_at: None,
            monitor_wake_requested_at: None,
            monitor_last_triggered_at: None,
            monitor_attempt_count: 0,
            monitor_notes: None,
            monitor_scheduled_by: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            source_trust: None,
            unblock_descriptor: None,
            blocked_transition_at: None,
            blocked_owner_notified_at: None,
            started_at: None,
            completed_at: None,
            cancelled_at: None,
            hidden_at: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn r753_apply_to_row_status_and_assignee_round_trip() {
        let issue = fixture_issue();
        let agent_id = Uuid::new_v4();
        let mut outcome = ApplyTransitionOutcome::default();
        outcome.patch.insert("status".into(), json!("in_progress"));
        outcome
            .patch
            .insert("assigneeAgentId".into(), json!(agent_id.to_string()));
        outcome
            .patch
            .insert("assigneeUserId".into(), json!("user-42"));

        let updated = outcome.apply_to_row(&issue);
        assert_eq!(updated.status, "in_progress");
        assert_eq!(updated.assignee_agent_id, Some(agent_id));
        assert_eq!(updated.assignee_user_id.as_deref(), Some("user-42"));
        // 未触达的字段保持原值
        assert_eq!(updated.title, issue.title);
        assert_eq!(updated.priority, issue.priority);
    }

    #[test]
    fn r753_apply_to_row_monitor_next_check_parses_iso_string() {
        let issue = fixture_issue();
        let iso = "2026-08-17T09:30:00Z";
        let mut outcome = ApplyTransitionOutcome::default();
        outcome
            .patch
            .insert("monitorNextCheckAt".into(), json!(iso));
        outcome.patch.insert("monitorNotes".into(), json!("probe"));
        outcome
            .patch
            .insert("monitorAttemptCount".into(), json!(3_i64));

        let updated = outcome.apply_to_row(&issue);
        assert_eq!(
            updated.monitor_next_check_at.map(|t| t.to_string()),
            Some("2026-08-17T09:30:00+00:00".to_string())
        );
        assert_eq!(updated.monitor_notes.as_deref(), Some("probe"));
        assert_eq!(updated.monitor_attempt_count, 3);
    }

    #[test]
    fn r753_apply_to_row_unknown_keys_are_ignored() {
        let issue = fixture_issue();
        let mut outcome = ApplyTransitionOutcome::default();
        outcome.patch.insert("status".into(), json!("blocked"));
        outcome
            .patch
            .insert("nonsenseField".into(), json!("ignored"));

        let updated = outcome.apply_to_row(&issue);
        assert_eq!(updated.status, "blocked");
        assert_eq!(updated.assignee_agent_id, issue.assignee_agent_id);
        assert_eq!(updated.execution_policy, issue.execution_policy);
    }
}
