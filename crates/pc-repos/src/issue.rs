//! `issue` 域：issues + comments + children + labels + read state + inbox archive.

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

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueCommentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<String>,
    pub body: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct LabelRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub color: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueReadStateRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub user_id: String,
    pub last_read_at: Timestamp,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueInboxArchiveRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub user_id: String,
    pub archived_at: Timestamp,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueApprovalRow {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub approval_id: Uuid,
    pub linked_by_agent_id: Option<Uuid>,
    pub linked_by_user_id: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueThreadInteractionRow {
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
    pub payload: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub resolved_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct FeedbackVoteRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub target_type: String,
    pub target_id: String,
    pub author_user_id: String,
    pub vote: String,
    pub reason: Option<String>,
    pub shared_with_labs: bool,
    pub shared_at: Option<Timestamp>,
    pub consent_version: Option<String>,
    pub redaction_summary: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AttachmentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub asset_id: Uuid,
    pub issue_comment_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AssetRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub provider: String,
    pub object_key: String,
    pub content_type: String,
    pub byte_size: i32,
    pub sha256: String,
    pub original_filename: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ExternalObjectMentionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub source_kind: String,
    pub source_record_id: Option<Uuid>,
    pub document_key: Option<String>,
    pub property_key: Option<String>,
    pub matched_text_redacted: Option<String>,
    pub sanitized_display_url: Option<String>,
    pub canonical_identity_hash: Option<String>,
    pub object_id: Option<Uuid>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ExternalObjectRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub provider_key: String,
    pub plugin_id: Option<Uuid>,
    pub object_type: String,
    pub external_id: String,
    pub display_title: Option<String>,
    pub status_key: Option<String>,
    pub status_label: Option<String>,
    pub status_category: String,
    pub status_tone: String,
    pub liveness: String,
    pub is_terminal: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueWatchdogRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub watchdog_agent_id: Uuid,
    pub instructions: Option<String>,
    pub status: String,
    pub watchdog_issue_id: Option<Uuid>,
    pub last_observed_fingerprint: Option<String>,
    pub last_reviewed_fingerprint: Option<String>,
    pub last_triggered_at: Option<Timestamp>,
    pub last_completed_at: Option<Timestamp>,
    pub trigger_count: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub updated_by_run_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueRecoveryActionRow {
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
    pub timeout_at: Option<Timestamp>,
    pub last_attempt_at: Option<Timestamp>,
    pub outcome: Option<String>,
    pub resolution_note: Option<String>,
    pub resolved_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueWorkProductRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub issue_id: Uuid,
    pub type_: String,
    pub provider: String,
    pub external_id: Option<String>,
    pub title: String,
    pub status: String,
    pub review_state: String,
    pub is_primary: bool,
    pub health_status: String,
    pub summary: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_by_run_id: Option<Uuid>,
    pub source_trust: Option<serde_json::Value>,
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

pub struct IssueRepo<'a> {
    pub db: &'a Db,
}

impl<'a> IssueRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---------- list / get ----------

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        status: Option<&str>,
    ) -> sqlx::Result<Vec<IssueRow>> {
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues WHERE company_id = $1 \
             AND ($2::text IS NULL OR status = $2) AND hidden_at IS NULL \
             ORDER BY created_at DESC LIMIT 200"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(company_id)
            .bind(status)
            .fetch_all(self.db.pool())
            .await
    }

    /// 列出全部（跨公司）；按 status 过滤；limit 默认 200。
    pub async fn list_all(&self, status: Option<&str>, limit: i64) -> sqlx::Result<Vec<IssueRow>> {
        let status_filter: Option<String> = status.map(str::to_owned);
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues              WHERE (CAST($1 AS text) IS NULL OR status = $1)              AND hidden_at IS NULL              ORDER BY created_at DESC LIMIT $2"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(status_filter)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!("SELECT {ISSUE_COLS} FROM issues WHERE id = $1");
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    // ---------- create / update / delete ----------

    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        description: Option<&str>,
        priority: &str,
        assignee_agent_id: Option<Uuid>,
    ) -> sqlx::Result<IssueRow> {
        let sql = format!(
            "INSERT INTO issues (company_id, title, description, priority, assignee_agent_id) \
             VALUES ($1,$2,$3,$4,$5) RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(company_id)
            .bind(title)
            .bind(description)
            .bind(priority)
            .bind(assignee_agent_id)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn update(
        &self,
        id: Uuid,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<&str>,
        assignee_agent_id: Option<Option<Uuid>>,
    ) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!(
            "UPDATE issues SET \
                title=COALESCE($2,title), description=COALESCE($3,description), \
                status=COALESCE($4,status), priority=COALESCE($5,priority), \
                assignee_agent_id=COALESCE($6,assignee_agent_id), updated_at=now() \
             WHERE id=$1 RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id)
            .bind(title)
            .bind(description)
            .bind(status)
            .bind(priority)
            .bind(assignee_agent_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM issues WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // ---------- children ----------

    /// 列出某 issue 的子 issue（按 created_at 升序）。
    pub async fn list_children(&self, parent_id: Uuid) -> sqlx::Result<Vec<IssueRow>> {
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues WHERE parent_id = $1 AND hidden_at IS NULL \
             ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(parent_id)
            .fetch_all(self.db.pool())
            .await
    }

    /// 创建子 issue：自动填充 parent_id。
    pub async fn create_child(
        &self,
        parent: &IssueRow,
        title: &str,
        description: Option<&str>,
        priority: &str,
        assignee_agent_id: Option<Uuid>,
    ) -> sqlx::Result<IssueRow> {
        let sql = format!(
            "INSERT INTO issues (company_id, parent_id, title, description, priority, assignee_agent_id, request_depth) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(parent.company_id)
            .bind(parent.id)
            .bind(title)
            .bind(description)
            .bind(priority)
            .bind(assignee_agent_id)
            .bind(parent.request_depth + 1)
            .fetch_one(self.db.pool())
            .await
    }

    // ---------- release / force-release ----------

    /// 释放 checkout 锁：清空 checkout_run_id 与 execution_locked_at。
    /// `run_id` 可选：若提供则仅当当前 checkout_run_id 匹配时才释放（所有权保护）。
    pub async fn release(&self, id: Uuid, run_id: Option<Uuid>) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!(
            "UPDATE issues SET \
                checkout_run_id = NULL, \
                execution_locked_at = NULL, \
                updated_at = now() \
             WHERE id = $1 AND ($2::uuid IS NULL OR checkout_run_id = $2) \
             RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 强制释放（admin 路径），忽略 run_id 匹配。
    pub async fn force_release(&self, id: Uuid) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!(
            "UPDATE issues SET checkout_run_id = NULL, execution_locked_at = NULL, updated_at = now() \
             WHERE id = $1 RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    // ---------- comments ----------

    pub async fn list_comments(&self, issue_id: Uuid) -> sqlx::Result<Vec<IssueCommentRow>> {
        sqlx::query_as::<_, IssueCommentRow>(
            "SELECT id, company_id, issue_id, author_agent_id, author_user_id, body, created_at, updated_at \
             FROM issue_comments WHERE issue_id = $1 ORDER BY created_at ASC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_comment(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        author_agent_id: Option<Uuid>,
        author_user_id: Option<&str>,
        body: &str,
    ) -> sqlx::Result<IssueCommentRow> {
        sqlx::query_as::<_, IssueCommentRow>(
            "INSERT INTO issue_comments (company_id, issue_id, author_agent_id, author_user_id, body) \
             VALUES ($1,$2,$3,$4,$5) \
             RETURNING id, company_id, issue_id, author_agent_id, author_user_id, body, created_at, updated_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(author_agent_id)
        .bind(author_user_id)
        .bind(body)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn update_comment(
        &self,
        issue_id: Uuid,
        comment_id: Uuid,
        body: &str,
    ) -> sqlx::Result<Option<IssueCommentRow>> {
        sqlx::query_as::<_, IssueCommentRow>(
            "UPDATE issue_comments SET body = $3, updated_at = now() \
             WHERE id = $1 AND issue_id = $2 \
             RETURNING id, company_id, issue_id, author_agent_id, author_user_id, body, created_at, updated_at",
        )
        .bind(comment_id)
        .bind(issue_id)
        .bind(body)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn delete_comment(&self, issue_id: Uuid, comment_id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM issue_comments WHERE id = $1 AND issue_id = $2")
            .bind(comment_id)
            .bind(issue_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // ---------- labels ----------

    pub async fn list_labels(&self, company_id: Uuid) -> sqlx::Result<Vec<LabelRow>> {
        sqlx::query_as::<_, LabelRow>(
            "SELECT id, company_id, name, color, created_at, updated_at \
             FROM labels WHERE company_id = $1 ORDER BY name ASC",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_label(
        &self,
        company_id: Uuid,
        name: &str,
        color: &str,
    ) -> sqlx::Result<LabelRow> {
        sqlx::query_as::<_, LabelRow>(
            "INSERT INTO labels (company_id, name, color) VALUES ($1,$2,$3) \
             RETURNING id, company_id, name, color, created_at, updated_at",
        )
        .bind(company_id)
        .bind(name)
        .bind(color)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn delete_label(&self, company_id: Uuid, label_id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM labels WHERE id = $1 AND company_id = $2")
            .bind(label_id)
            .bind(company_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn assign_label(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        label_id: Uuid,
    ) -> sqlx::Result<bool> {
        let r = sqlx::query(
            "INSERT INTO issue_labels (issue_id, label_id, company_id) \
             VALUES ($1,$2,$3) ON CONFLICT (issue_id, label_id) DO NOTHING",
        )
        .bind(issue_id)
        .bind(label_id)
        .bind(company_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn unassign_label(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        label_id: Uuid,
    ) -> sqlx::Result<bool> {
        let r = sqlx::query(
            "DELETE FROM issue_labels WHERE issue_id = $1 AND label_id = $2 AND company_id = $3",
        )
        .bind(issue_id)
        .bind(label_id)
        .bind(company_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn list_issue_label_ids(&self, issue_id: Uuid) -> sqlx::Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT label_id FROM issue_labels WHERE issue_id = $1")
                .bind(issue_id)
                .fetch_all(self.db.pool())
                .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    // ---------- read state ----------

    pub async fn get_read_state(
        &self,
        issue_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Option<IssueReadStateRow>> {
        sqlx::query_as::<_, IssueReadStateRow>(
            "SELECT id, company_id, issue_id, user_id, last_read_at, created_at, updated_at \
             FROM issue_read_states WHERE issue_id = $1 AND user_id = $2",
        )
        .bind(issue_id)
        .bind(user_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn upsert_read_state(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        user_id: &str,
        last_read_at: Option<Timestamp>,
    ) -> sqlx::Result<IssueReadStateRow> {
        sqlx::query_as::<_, IssueReadStateRow>(
            "INSERT INTO issue_read_states (company_id, issue_id, user_id, last_read_at) \
             VALUES ($1,$2,$3, COALESCE($4, now())) \
             ON CONFLICT (company_id, issue_id, user_id) DO UPDATE \
                SET last_read_at = EXCLUDED.last_read_at, updated_at = now() \
             RETURNING id, company_id, issue_id, user_id, last_read_at, created_at, updated_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(user_id)
        .bind(last_read_at)
        .fetch_one(self.db.pool())
        .await
    }

    // ---------- inbox archive ----------

    pub async fn list_inbox_archives(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Vec<IssueInboxArchiveRow>> {
        sqlx::query_as::<_, IssueInboxArchiveRow>(
            "SELECT id, company_id, issue_id, user_id, archived_at, created_at, updated_at \
             FROM issue_inbox_archives WHERE company_id = $1 AND user_id = $2 \
             ORDER BY archived_at DESC",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn archive_inbox(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<IssueInboxArchiveRow> {
        sqlx::query_as::<_, IssueInboxArchiveRow>(
            "INSERT INTO issue_inbox_archives (company_id, issue_id, user_id) \
             VALUES ($1,$2,$3) \
             ON CONFLICT (company_id, issue_id, user_id) DO UPDATE \
                SET archived_at = now(), updated_at = now() \
             RETURNING id, company_id, issue_id, user_id, archived_at, created_at, updated_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(user_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn unarchive_inbox(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<bool> {
        let r = sqlx::query(
            "DELETE FROM issue_inbox_archives \
             WHERE company_id = $1 AND issue_id = $2 AND user_id = $3",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(user_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Watchdog
    // =========================================================================

    pub async fn get_active_watchdog(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<IssueWatchdogRow>> {
        sqlx::query_as::<_, IssueWatchdogRow>(
            "SELECT id, company_id, issue_id, watchdog_agent_id, instructions, status, \
                    watchdog_issue_id, last_observed_fingerprint, last_reviewed_fingerprint, \
                    last_triggered_at, last_completed_at, trigger_count, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, \
                    updated_by_agent_id, updated_by_user_id, updated_by_run_id, \
                    created_at, updated_at \
             FROM issue_watchdogs WHERE issue_id = $1 AND status = 'active' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn upsert_watchdog(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        watchdog_agent_id: Uuid,
        instructions: Option<&str>,
        created_by_agent_id: Option<Uuid>,
        created_by_user_id: Option<&str>,
        created_by_run_id: Option<Uuid>,
    ) -> sqlx::Result<(IssueWatchdogRow, bool)> {
        // 先查找现有 active watchdog
        if let Some(existing) = self.get_active_watchdog(issue_id).await? {
            let row = sqlx::query_as::<_, IssueWatchdogRow>(
                "UPDATE issue_watchdogs SET \
                    watchdog_agent_id = $2, \
                    instructions = $3, \
                    updated_by_agent_id = $4, \
                    updated_by_user_id = $5, \
                    updated_by_run_id = $6, \
                    updated_at = now() \
                 WHERE id = $1 \
                 RETURNING id, company_id, issue_id, watchdog_agent_id, instructions, status, \
                    watchdog_issue_id, last_observed_fingerprint, last_reviewed_fingerprint, \
                    last_triggered_at, last_completed_at, trigger_count, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, \
                    updated_by_agent_id, updated_by_user_id, updated_by_run_id, \
                    created_at, updated_at",
            )
            .bind(existing.id)
            .bind(watchdog_agent_id)
            .bind(instructions)
            .bind(created_by_agent_id)
            .bind(created_by_user_id)
            .bind(created_by_run_id)
            .fetch_one(self.db.pool())
            .await?;
            Ok((row, false))
        } else {
            let row = sqlx::query_as::<_, IssueWatchdogRow>(
                "INSERT INTO issue_watchdogs \
                    (company_id, issue_id, watchdog_agent_id, instructions, \
                     created_by_agent_id, created_by_user_id, created_by_run_id) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7) \
                 RETURNING id, company_id, issue_id, watchdog_agent_id, instructions, status, \
                    watchdog_issue_id, last_observed_fingerprint, last_reviewed_fingerprint, \
                    last_triggered_at, last_completed_at, trigger_count, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, \
                    updated_by_agent_id, updated_by_user_id, updated_by_run_id, \
                    created_at, updated_at",
            )
            .bind(company_id)
            .bind(issue_id)
            .bind(watchdog_agent_id)
            .bind(instructions)
            .bind(created_by_agent_id)
            .bind(created_by_user_id)
            .bind(created_by_run_id)
            .fetch_one(self.db.pool())
            .await?;
            Ok((row, true))
        }
    }

    pub async fn disable_watchdog(&self, issue_id: Uuid) -> sqlx::Result<Option<IssueWatchdogRow>> {
        sqlx::query_as::<_, IssueWatchdogRow>(
            "UPDATE issue_watchdogs SET status = 'disabled', updated_at = now() \
             WHERE issue_id = $1 AND status = 'active' \
             RETURNING id, company_id, issue_id, watchdog_agent_id, instructions, status, \
                watchdog_issue_id, last_observed_fingerprint, last_reviewed_fingerprint, \
                last_triggered_at, last_completed_at, trigger_count, \
                created_by_agent_id, created_by_user_id, created_by_run_id, \
                updated_by_agent_id, updated_by_user_id, updated_by_run_id, \
                created_at, updated_at",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await
    }

    // =========================================================================
    // Recovery actions
    // =========================================================================

    pub async fn list_recovery_actions(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueRecoveryActionRow>> {
        sqlx::query_as::<_, IssueRecoveryActionRow>(
            "SELECT id, company_id, source_issue_id, recovery_issue_id, kind, status, \
                    owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                    return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                    wake_policy, monitor_policy, attempt_count, max_attempts, \
                    timeout_at, last_attempt_at, outcome, resolution_note, resolved_at, \
                    created_at, updated_at \
             FROM issue_recovery_actions WHERE source_issue_id = $1 \
             ORDER BY created_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_active_recovery_action(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<IssueRecoveryActionRow>> {
        sqlx::query_as::<_, IssueRecoveryActionRow>(
            "SELECT id, company_id, source_issue_id, recovery_issue_id, kind, status, \
                    owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                    return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                    wake_policy, monitor_policy, attempt_count, max_attempts, \
                    timeout_at, last_attempt_at, outcome, resolution_note, resolved_at, \
                    created_at, updated_at \
             FROM issue_recovery_actions WHERE source_issue_id = $1 AND status = 'active' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn resolve_recovery_action(
        &self,
        action_id: Uuid,
        resolution_note: Option<&str>,
        outcome: &str,
    ) -> sqlx::Result<Option<IssueRecoveryActionRow>> {
        sqlx::query_as::<_, IssueRecoveryActionRow>(
            "UPDATE issue_recovery_actions SET \
                status = 'resolved', resolution_note = $2, outcome = $3, \
                resolved_at = now(), updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, source_issue_id, recovery_issue_id, kind, status, \
                owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                wake_policy, monitor_policy, attempt_count, max_attempts, \
                timeout_at, last_attempt_at, outcome, resolution_note, resolved_at, \
                created_at, updated_at",
        )
        .bind(action_id)
        .bind(resolution_note)
        .bind(outcome)
        .fetch_optional(self.db.pool())
        .await
    }

    // =========================================================================
    // Work products
    // =========================================================================

    pub async fn list_work_products(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueWorkProductRow>> {
        sqlx::query_as::<_, IssueWorkProductRow>(
            "SELECT id, company_id, project_id, issue_id, type as type_, provider, \
                    external_id, title, status, review_state, is_primary, health_status, \
                    summary, metadata, created_by_run_id, source_trust, \
                    created_at, updated_at \
             FROM issue_work_products WHERE issue_id = $1 \
             ORDER BY created_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_work_product(&self, id: Uuid) -> sqlx::Result<Option<IssueWorkProductRow>> {
        sqlx::query_as::<_, IssueWorkProductRow>(
            "SELECT id, company_id, project_id, issue_id, type as type_, provider, \
                    external_id, title, status, review_state, is_primary, health_status, \
                    summary, metadata, created_by_run_id, source_trust, \
                    created_at, updated_at \
             FROM issue_work_products WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_work_product(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        project_id: Option<Uuid>,
        product_type: &str,
        provider: &str,
        external_id: Option<&str>,
        title: &str,
        status: &str,
        review_state: &str,
        is_primary: bool,
        health_status: &str,
        summary: Option<&str>,
        metadata: Option<&serde_json::Value>,
        created_by_run_id: Option<Uuid>,
    ) -> sqlx::Result<IssueWorkProductRow> {
        sqlx::query_as::<_, IssueWorkProductRow>(
            "INSERT INTO issue_work_products \
                (company_id, project_id, issue_id, type, provider, external_id, title, \
                 status, review_state, is_primary, health_status, summary, metadata, \
                 created_by_run_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) \
             RETURNING id, company_id, project_id, issue_id, type as type_, provider, \
                external_id, title, status, review_state, is_primary, health_status, \
                summary, metadata, created_by_run_id, source_trust, \
                created_at, updated_at",
        )
        .bind(company_id)
        .bind(project_id)
        .bind(issue_id)
        .bind(product_type)
        .bind(provider)
        .bind(external_id)
        .bind(title)
        .bind(status)
        .bind(review_state)
        .bind(is_primary)
        .bind(health_status)
        .bind(summary)
        .bind(metadata)
        .bind(created_by_run_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn update_work_product(
        &self,
        id: Uuid,
        title: Option<&str>,
        status: Option<&str>,
        review_state: Option<&str>,
        is_primary: Option<bool>,
        health_status: Option<&str>,
        summary: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> sqlx::Result<Option<IssueWorkProductRow>> {
        sqlx::query_as::<_, IssueWorkProductRow>(
            "UPDATE issue_work_products SET \
                title = COALESCE($2, title), \
                status = COALESCE($3, status), \
                review_state = COALESCE($4, review_state), \
                is_primary = COALESCE($5, is_primary), \
                health_status = COALESCE($6, health_status), \
                summary = COALESCE($7, summary), \
                metadata = COALESCE($8, metadata), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, project_id, issue_id, type as type_, provider, \
                external_id, title, status, review_state, is_primary, health_status, \
                summary, metadata, created_by_run_id, source_trust, \
                created_at, updated_at",
        )
        .bind(id)
        .bind(title)
        .bind(status)
        .bind(review_state)
        .bind(is_primary)
        .bind(health_status)
        .bind(summary)
        .bind(metadata)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn delete_work_product(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM issue_work_products WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Issue approvals
    // =========================================================================

    pub async fn list_issue_approvals(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueApprovalRow>> {
        sqlx::query_as::<_, IssueApprovalRow>(
            "SELECT company_id, issue_id, approval_id, linked_by_agent_id, linked_by_user_id, created_at \
             FROM issue_approvals WHERE issue_id = $1 ORDER BY created_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn link_approval(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        approval_id: Uuid,
        linked_by_agent_id: Option<Uuid>,
        linked_by_user_id: Option<&str>,
    ) -> sqlx::Result<IssueApprovalRow> {
        sqlx::query_as::<_, IssueApprovalRow>(
            "INSERT INTO issue_approvals (company_id, issue_id, approval_id, linked_by_agent_id, linked_by_user_id) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (issue_id, approval_id) DO UPDATE SET linked_by_agent_id = EXCLUDED.linked_by_agent_id, \
                linked_by_user_id = EXCLUDED.linked_by_user_id \
             RETURNING company_id, issue_id, approval_id, linked_by_agent_id, linked_by_user_id, created_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(approval_id)
        .bind(linked_by_agent_id)
        .bind(linked_by_user_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn unlink_approval(&self, issue_id: Uuid, approval_id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM issue_approvals WHERE issue_id = $1 AND approval_id = $2")
            .bind(issue_id)
            .bind(approval_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Issue thread interactions
    // =========================================================================

    pub async fn list_interactions(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueThreadInteractionRow>> {
        sqlx::query_as::<_, IssueThreadInteractionRow>(
            "SELECT id, company_id, issue_id, kind, status, continuation_policy, \
                    source_comment_id, source_run_id, title, summary, \
                    created_by_agent_id, created_by_user_id, \
                    resolved_by_agent_id, resolved_by_user_id, \
                    payload, result, resolved_at, created_at, updated_at \
             FROM issue_thread_interactions WHERE issue_id = $1 ORDER BY created_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_interaction(
        &self,
        id: Uuid,
    ) -> sqlx::Result<Option<IssueThreadInteractionRow>> {
        sqlx::query_as::<_, IssueThreadInteractionRow>(
            "SELECT id, company_id, issue_id, kind, status, continuation_policy, \
                    source_comment_id, source_run_id, title, summary, \
                    created_by_agent_id, created_by_user_id, \
                    resolved_by_agent_id, resolved_by_user_id, \
                    payload, result, resolved_at, created_at, updated_at \
             FROM issue_thread_interactions WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_interaction(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        kind: &str,
        continuation_policy: &str,
        title: Option<&str>,
        summary: Option<&str>,
        payload: &serde_json::Value,
        created_by_agent_id: Option<Uuid>,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<IssueThreadInteractionRow> {
        sqlx::query_as::<_, IssueThreadInteractionRow>(
            "INSERT INTO issue_thread_interactions \
                (company_id, issue_id, kind, continuation_policy, title, summary, \
                 payload, created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
             RETURNING id, company_id, issue_id, kind, status, continuation_policy, \
                    source_comment_id, source_run_id, title, summary, \
                    created_by_agent_id, created_by_user_id, \
                    resolved_by_agent_id, resolved_by_user_id, \
                    payload, result, resolved_at, created_at, updated_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(kind)
        .bind(continuation_policy)
        .bind(title)
        .bind(summary)
        .bind(payload)
        .bind(created_by_agent_id)
        .bind(created_by_user_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn resolve_interaction(
        &self,
        id: Uuid,
        new_status: &str,
        result: Option<&serde_json::Value>,
        resolved_by_user_id: Option<&str>,
    ) -> sqlx::Result<Option<IssueThreadInteractionRow>> {
        sqlx::query_as::<_, IssueThreadInteractionRow>(
            "UPDATE issue_thread_interactions SET \
                status = $2, result = COALESCE($3, result), \
                resolved_by_user_id = COALESCE($4, resolved_by_user_id), \
                resolved_at = CASE WHEN $2 IN ('accepted','rejected','cancelled','withdrawn','responded') THEN now() ELSE resolved_at END, \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, issue_id, kind, status, continuation_policy, \
                    source_comment_id, source_run_id, title, summary, \
                    created_by_agent_id, created_by_user_id, \
                    resolved_by_agent_id, resolved_by_user_id, \
                    payload, result, resolved_at, created_at, updated_at",
        )
        .bind(id)
        .bind(new_status)
        .bind(result)
        .bind(resolved_by_user_id)
        .fetch_optional(self.db.pool())
        .await
    }

    // =========================================================================
    // Feedback votes
    // =========================================================================

    pub async fn list_feedback_votes(&self, issue_id: Uuid) -> sqlx::Result<Vec<FeedbackVoteRow>> {
        sqlx::query_as::<_, FeedbackVoteRow>(
            "SELECT id, company_id, issue_id, target_type, target_id, author_user_id, \
                    vote, reason, shared_with_labs, shared_at, consent_version, \
                    redaction_summary, created_at, updated_at \
             FROM feedback_votes WHERE issue_id = $1 ORDER BY created_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_feedback_vote(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        target_type: &str,
        target_id: &str,
        author_user_id: &str,
        vote: &str,
        reason: Option<&str>,
    ) -> sqlx::Result<FeedbackVoteRow> {
        sqlx::query_as::<_, FeedbackVoteRow>(
            "INSERT INTO feedback_votes \
                (company_id, issue_id, target_type, target_id, author_user_id, vote, reason) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             RETURNING id, company_id, issue_id, target_type, target_id, author_user_id, \
                    vote, reason, shared_with_labs, shared_at, consent_version, \
                    redaction_summary, created_at, updated_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(target_type)
        .bind(target_id)
        .bind(author_user_id)
        .bind(vote)
        .bind(reason)
        .fetch_one(self.db.pool())
        .await
    }

    // =========================================================================
    // Attachments (via assets)
    // =========================================================================

    pub async fn list_issue_attachments(&self, issue_id: Uuid) -> sqlx::Result<Vec<AttachmentRow>> {
        sqlx::query_as::<_, AttachmentRow>(
            "SELECT id, company_id, issue_id, asset_id, issue_comment_id, created_at, updated_at \
             FROM issue_attachments WHERE issue_id = $1 ORDER BY created_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_attachment(&self, id: Uuid) -> sqlx::Result<Option<AttachmentRow>> {
        sqlx::query_as::<_, AttachmentRow>(
            "SELECT id, company_id, issue_id, asset_id, issue_comment_id, created_at, updated_at \
             FROM issue_attachments WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn get_asset(&self, id: Uuid) -> sqlx::Result<Option<AssetRow>> {
        sqlx::query_as::<_, AssetRow>(
            "SELECT id, company_id, provider, object_key, content_type, byte_size, sha256, \
                    original_filename, created_by_agent_id, created_by_user_id, \
                    created_at, updated_at \
             FROM assets WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_attachment(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        provider: &str,
        object_key: &str,
        content_type: &str,
        byte_size: i32,
        sha256: &str,
        original_filename: Option<&str>,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<(AttachmentRow, AssetRow)> {
        let mut tx = self.db.pool().begin().await?;
        let asset_id = Uuid::new_v4();
        let asset: AssetRow = sqlx::query_as::<_, AssetRow>(
            "INSERT INTO assets (id, company_id, provider, object_key, content_type, byte_size, sha256, original_filename, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
             RETURNING id, company_id, provider, object_key, content_type, byte_size, sha256, \
                    original_filename, created_by_agent_id, created_by_user_id, \
                    created_at, updated_at",
        )
        .bind(asset_id)
        .bind(company_id)
        .bind(provider)
        .bind(object_key)
        .bind(content_type)
        .bind(byte_size)
        .bind(sha256)
        .bind(original_filename)
        .bind(created_by_user_id)
        .fetch_one(&mut *tx)
        .await?;
        let attach: AttachmentRow = sqlx::query_as::<_, AttachmentRow>(
            "INSERT INTO issue_attachments (company_id, issue_id, asset_id) \
             VALUES ($1,$2,$3) \
             RETURNING id, company_id, issue_id, asset_id, issue_comment_id, created_at, updated_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(asset_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((attach, asset))
    }

    pub async fn delete_attachment(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM issue_attachments WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // External objects
    // =========================================================================

    pub async fn list_external_object_mentions(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<ExternalObjectMentionRow>> {
        sqlx::query_as::<_, ExternalObjectMentionRow>(
            "SELECT id, company_id, source_issue_id, source_kind, source_record_id, \
                    document_key, property_key, matched_text_redacted, sanitized_display_url, \
                    canonical_identity_hash, object_id, created_at \
             FROM external_object_mentions WHERE source_issue_id = $1 \
             ORDER BY created_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_external_object(&self, id: Uuid) -> sqlx::Result<Option<ExternalObjectRow>> {
        sqlx::query_as::<_, ExternalObjectRow>(
            "SELECT id, company_id, provider_key, plugin_id, object_type, external_id, \
                    display_title, status_key, status_label, status_category, status_tone, \
                    liveness, is_terminal, created_at, updated_at \
             FROM external_objects WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// 聚合 external object 摘要：按 status_category 统计 + 终端状态判断
    pub async fn external_object_summary(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<ExternalObjectSummary> {
        let mentions = self.list_external_object_mentions(issue_id).await?;
        let mut object_ids: Vec<Uuid> = mentions
            .iter()
            .filter_map(|m| m.object_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        object_ids.sort();
        let mut objects: Vec<ExternalObjectRow> = Vec::new();
        for oid in object_ids {
            if let Some(o) = self.get_external_object(oid).await? {
                objects.push(o);
            }
        }
        let total = objects.len();
        let terminal = objects.iter().filter(|o| o.is_terminal).count();
        let open = total - terminal;
        let mut by_category: std::collections::BTreeMap<String, i64> = Default::default();
        for o in &objects {
            *by_category.entry(o.status_category.clone()).or_insert(0) += 1;
        }
        Ok(ExternalObjectSummary {
            issue_id,
            total_objects: total as i64,
            open_objects: open as i64,
            terminal_objects: terminal as i64,
            by_category,
            objects,
        })
    }

    // =========================================================================
    // Diagnostics
    // =========================================================================

    /// 返回阻塞此 issue 的问题列表（parent_id = this.id 且 status != done）
    pub async fn list_blockers(&self, issue_id: Uuid) -> sqlx::Result<Vec<IssueRow>> {
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues WHERE parent_id = $1 \
             AND status NOT IN ('done', 'cancelled', 'completed') AND hidden_at IS NULL \
             ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(issue_id)
            .fetch_all(self.db.pool())
            .await
    }

    /// 返回最近 wakes（agent_wakeup_requests 关联到 issue）
    pub async fn list_wakes(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<serde_json::Value>> {
        let rows: Vec<(Uuid, String, String, Option<serde_json::Value>, Timestamp)> =
            sqlx::query_as(
                "SELECT id, source, status, payload, created_at \
                 FROM agent_wakeup_requests \
                 WHERE payload->>'issueId' = $1::text \
                 ORDER BY created_at DESC LIMIT $2",
            )
            .bind(issue_id.to_string())
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, source, status, payload, created_at)| {
                serde_json::json!({
                    "id": id,
                    "source": source,
                    "status": status,
                    "payload": payload,
                    "created_at": created_at,
                })
            })
            .collect())
    }

    /// 返回 issue 的子 issue 树（递归）
    pub async fn subtree_diagnostics(&self, issue_id: Uuid) -> sqlx::Result<IssueSubtree> {
        let root = self.get(issue_id).await?;
        let children = self.list_children(issue_id).await?;
        let mut subtree_children = Vec::with_capacity(children.len());
        for c in children {
            // 限制深度：仅展开一层
            let grandchildren = self.list_children(c.id).await?;
            subtree_children.push(IssueSubtreeNode {
                issue: c,
                children: grandchildren
                    .into_iter()
                    .map(|g| IssueSubtreeNode {
                        issue: g,
                        children: Vec::new(),
                    })
                    .collect(),
            });
        }
        Ok(IssueSubtree {
            root,
            children: subtree_children,
        })
    }

    // =========================================================================
    // Count + search
    // =========================================================================

    pub async fn count_company_issues(
        &self,
        company_id: Uuid,
        status: Option<&str>,
    ) -> sqlx::Result<i64> {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM issues \
             WHERE company_id = $1 AND hidden_at IS NULL \
             AND ($2::text IS NULL OR status = $2)",
        )
        .bind(company_id)
        .bind(status)
        .fetch_one(self.db.pool())
        .await?;
        Ok(count.0)
    }

    /// 简单全文搜索：在 title/description 中 ILIKE 匹配
    pub async fn search_company_issues(
        &self,
        company_id: Uuid,
        query: &str,
        limit: i64,
    ) -> sqlx::Result<Vec<IssueRow>> {
        let pat = format!("%{}%", query);
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues \
             WHERE company_id = $1 AND hidden_at IS NULL \
             AND (title ILIKE $2 OR description ILIKE $2) \
             ORDER BY created_at DESC LIMIT $3"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(company_id)
            .bind(pat)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    // =========================================================================
    // Tree control
    // =========================================================================

    /// 标记 monitor check-now：把 monitor_next_check_at 设为 now()
    pub async fn trigger_monitor_check_now(&self, id: Uuid) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!(
            "UPDATE issues SET monitor_next_check_at = now(), monitor_wake_requested_at = now(), \
                updated_at = now() \
             WHERE id = $1 RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 触发 scheduled-retry：把 monitor_next_check_at 设为 now() + 1s
    pub async fn trigger_scheduled_retry_now(&self, id: Uuid) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!(
            "UPDATE issues SET monitor_next_check_at = now() + interval '1 second', \
                monitor_wake_requested_at = now(), monitor_attempt_count = monitor_attempt_count + 1, \
                updated_at = now() \
             WHERE id = $1 RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalObjectSummary {
    pub issue_id: Uuid,
    pub total_objects: i64,
    pub open_objects: i64,
    pub terminal_objects: i64,
    pub by_category: std::collections::BTreeMap<String, i64>,
    pub objects: Vec<ExternalObjectRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSubtree {
    pub root: Option<IssueRow>,
    pub children: Vec<IssueSubtreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSubtreeNode {
    pub issue: IssueRow,
    pub children: Vec<IssueSubtreeNode>,
}
