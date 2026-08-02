//! `issue` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub work_mode: String,
    pub harness_kind: Option<String>,
    pub priority: String,
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
    pub checkout_run_id: Option<Uuid>,
    pub execution_run_id: Option<Uuid>,
    pub execution_agent_name_key: Option<String>,
    pub execution_locked_at: Option<Timestamp>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub issue_number: Option<i32>,
    pub identifier: Option<String>,
    pub origin_kind: String,
    pub origin_id: Option<String>,
    pub origin_run_id: Option<String>,
    pub origin_fingerprint: String,
    pub request_depth: i32,
    pub billing_code: Option<String>,
    pub assignee_adapter_overrides: Option<serde_json::Value>,
    pub execution_policy: Option<serde_json::Value>,
    pub execution_state: Option<serde_json::Value>,
    pub monitor_next_check_at: Option<Timestamp>,
    pub monitor_wake_requested_at: Option<Timestamp>,
    pub monitor_last_triggered_at: Option<Timestamp>,
    pub monitor_attempt_count: i32,
    pub monitor_notes: Option<String>,
    pub monitor_scheduled_by: Option<String>,
    pub execution_workspace_id: Option<Uuid>,
    pub execution_workspace_preference: Option<String>,
    pub execution_workspace_settings: Option<serde_json::Value>,
    pub source_trust: Option<serde_json::Value>,
    pub unblock_descriptor: Option<serde_json::Value>,
    pub blocked_transition_at: Option<Timestamp>,
    pub blocked_owner_notified_at: Option<Timestamp>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub cancelled_at: Option<Timestamp>,
    pub hidden_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const ISSUE_COLS: &str = "id, company_id, project_id, project_workspace_id, goal_id, parent_id, \
    title, description, status, work_mode, harness_kind, priority, \
    assignee_agent_id, assignee_user_id, checkout_run_id, execution_run_id, \
    execution_agent_name_key, execution_locked_at, created_by_agent_id, created_by_user_id, \
    responsible_user_id, issue_number, identifier, origin_kind, origin_id, origin_run_id, \
    origin_fingerprint, request_depth, billing_code, assignee_adapter_overrides, \
    execution_policy, execution_state, monitor_next_check_at, monitor_wake_requested_at, \
    monitor_last_triggered_at, monitor_attempt_count, monitor_notes, monitor_scheduled_by, \
    execution_workspace_id, execution_workspace_preference, execution_workspace_settings, \
    source_trust, unblock_descriptor, blocked_transition_at, blocked_owner_notified_at, \
    started_at, completed_at, cancelled_at, hidden_at, created_at, updated_at";

pub struct IssueRepo<'a> { pub db: &'a Db }

impl<'a> IssueRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }

    pub async fn list_by_company(&self, company_id: Uuid, status: Option<&str>) -> sqlx::Result<Vec<IssueRow>> {
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues WHERE company_id = $1 \
             AND ($2::text IS NULL OR status = $2) AND hidden_at IS NULL \
             ORDER BY created_at DESC LIMIT 200"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(company_id).bind(status)
            .fetch_all(self.db.pool()).await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!("SELECT {ISSUE_COLS} FROM issues WHERE id = $1");
        sqlx::query_as::<_, IssueRow>(&sql).bind(id)
            .fetch_optional(self.db.pool()).await
    }

    pub async fn create(
        &self, company_id: Uuid, title: &str, description: Option<&str>,
        priority: &str, assignee_agent_id: Option<Uuid>,
    ) -> sqlx::Result<IssueRow> {
        let sql = format!(
            "INSERT INTO issues (company_id, title, description, priority, assignee_agent_id) \
             VALUES ($1,$2,$3,$4,$5) RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(company_id).bind(title).bind(description).bind(priority).bind(assignee_agent_id)
            .fetch_one(self.db.pool()).await
    }

    pub async fn update(
        &self, id: Uuid, title: Option<&str>, description: Option<&str>,
        status: Option<&str>, priority: Option<&str>, assignee_agent_id: Option<Option<Uuid>>,
    ) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!(
            "UPDATE issues SET \
                title=COALESCE($2,title), description=COALESCE($3,description), \
                status=COALESCE($4,status), priority=COALESCE($5,priority), \
                assignee_agent_id=COALESCE($6,assignee_agent_id), updated_at=now() \
             WHERE id=$1 RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id).bind(title).bind(description).bind(status).bind(priority).bind(assignee_agent_id)
            .fetch_optional(self.db.pool()).await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM issues WHERE id=$1").bind(id)
            .execute(self.db.pool()).await?;
        Ok(r.rows_affected() > 0)
    }
}
