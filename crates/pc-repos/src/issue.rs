//! `issue` 域：issues + comments + children + labels + read state + inbox archive.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::issue_change_receipt::{build_issue_changes, IssueChanges, IssueRelationChanges};
use crate::issue_terminal_effects::{
    apply_issue_terminal_effects, TerminalEffectActor, TerminalEffectIssue,
};
use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueTitleRow {
    pub id: Uuid,
    pub title: String,
    pub status: String,
}

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

/// `upsert_recovery_action` 的输入。
///
/// 字段语义与 Node `UpsertIssueRecoveryActionInput` 对齐：
/// - `owner_type` 不填则由 `owner_agent_id` / `owner_user_id` 推导
/// - `attempt_count` 在 update 路径上由数据库自增（`existing.attempt_count + 1`）
/// - fingerprint 用于 active 唯一索引（同一 source 同一 fingerprint 只保留一条 active）
#[derive(Debug, Clone)]
pub struct UpsertRecoveryAction {
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
    pub timeout_at: Option<Timestamp>,
    pub last_attempt_at: Option<Timestamp>,
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

const ISSUE_STATUSES: [&str; 7] = [
    "backlog", "todo", "in_progress", "in_review", "done", "blocked", "cancelled",
];

fn valid_issue_status(status: &str) -> bool {
    ISSUE_STATUSES.contains(&status)
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct BlockedAttentionRow {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub priority: String,
    pub updated_at: pc_core::Timestamp,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct IssueRunLinkRow {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub kind: String,
}

pub struct IssueRepo<'a> {
    pub db: &'a Db,
}

#[derive(Debug, Clone)]
pub struct IssueUpdateReceipt {
    pub issue: IssueRow,
    pub changes: IssueChanges,
}

#[derive(Debug, Clone, Default)]
pub struct IssueRelationUpdate {
    pub label_ids: Option<Vec<Uuid>>,
    pub blocked_by_issue_ids: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, Default)]
pub struct IssueUpdateActor {
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
    pub run_id: Option<Uuid>,
}

impl<'a> IssueRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn claim_due_monitors(&self, limit: i64) -> sqlx::Result<Vec<IssueRow>> {
        let query = format!(
            "WITH due AS (\
                SELECT id FROM issues \
                WHERE monitor_next_check_at IS NOT NULL \
                  AND monitor_next_check_at <= now() \
                  AND assignee_agent_id IS NOT NULL AND assignee_user_id IS NULL \
                  AND status IN ('in_progress','in_review') \
                  AND (monitor_wake_requested_at IS NULL \
                       OR monitor_wake_requested_at < now() - interval '5 minutes') \
                ORDER BY monitor_next_check_at ASC, updated_at ASC \
                LIMIT $1 FOR UPDATE SKIP LOCKED\
            ) \
            UPDATE issues AS i SET monitor_wake_requested_at=now(), updated_at=now() \
            FROM due WHERE i.id=due.id RETURNING {ISSUE_COLS}"
        );
        sqlx::query_as::<_, IssueRow>(&query)
            .bind(limit.clamp(1, 50))
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn complete_monitor_dispatch(&self, id: Uuid) -> sqlx::Result<()> {
        sqlx::query(
            "UPDATE issues SET monitor_next_check_at=NULL, monitor_last_triggered_at=now(), \
             monitor_attempt_count=monitor_attempt_count+1, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
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

    /// 公司内 issue 标题模糊搜索（`ILIKE %query%`），返回 `(id, title, status)` 投影。
    /// 用于 `POST /api/companies/:id/search/extract` 的快速标题级抽取。
    pub async fn search_titles(
        &self,
        company_id: Uuid,
        query: &str,
        limit: i64,
    ) -> sqlx::Result<Vec<IssueTitleRow>> {
        let limit = limit.clamp(1, 100);
        sqlx::query_as::<_, IssueTitleRow>(
            "SELECT id, title, status FROM issues \
             WHERE company_id = $1 AND title ILIKE $2 LIMIT $3",
        )
        .bind(company_id)
        .bind(format!("%{query}%"))
        .bind(limit)
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

/// Round 107: 列出指派给 agent 的活跃 issues (status in todo/in_progress/blocked
    /// 且未被 hidden)。专门用于 `GET /agents/me/inbox/lite` 这种轻量自查询端点。
    pub async fn list_assigned_active(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<IssueRow>> {
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues              WHERE company_id=$1 AND assignee_agent_id=$2              AND status IN ('todo','in_progress','blocked') AND hidden_at IS NULL              ORDER BY updated_at DESC LIMIT $3"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(company_id)
            .bind(agent_id)
            .bind(limit.clamp(1, 1000))
            .fetch_all(self.db.pool())
            .await
    }
/// Round 108: 列某个 agent 被指派的 issues，按 status 多值 + responsible_user_id 过滤。
    /// `statuses_csv` 例如 "todo,in_progress,blocked"，会被 `string_to_array` 拆分。
    /// `responsible_user_id` 为 Some("") 时不过滤；为 None 时也不过滤；为 Some(other) 时按精确匹配。
    pub async fn list_assigned_filtered(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
        statuses_csv: &str,
        responsible_user_id: Option<&str>,
        limit: i64,
    ) -> sqlx::Result<Vec<IssueRow>> {
        let sql = format!(
            "SELECT {ISSUE_COLS} FROM issues              WHERE company_id=$1 AND assignee_agent_id=$2              AND status = ANY(string_to_array($3, ','))              AND hidden_at IS NULL              AND ($4 = '' OR responsible_user_id = $4)              ORDER BY updated_at DESC LIMIT $5"
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(company_id)
            .bind(agent_id)
            .bind(statuses_csv)
            .bind(responsible_user_id.unwrap_or(""))
            .bind(limit.clamp(1, 1000))
            .fetch_all(self.db.pool())
            .await
    }



        /// Round 126: 统计 company 的 issue 总数。
    pub async fn count_for_company(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM issues WHERE company_id=$1")
            .bind(company_id)
            .fetch_one(self.db.pool())
            .await?;
        Ok(count)
    }

    /// Round 174: 实例统计用 —— 统计某公司"未隐藏"的 issue 数（hidden_at IS NULL）。
    pub async fn count_visible_for_company(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM issues WHERE company_id=$1 AND hidden_at IS NULL",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(count)
    }

    /// Round 177: 注意力队列用 —— 列出某公司的 blocked issues（非 harness、未隐藏）。
    pub async fn list_blocked_attention(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<BlockedAttentionRow>> {
        sqlx::query_as::<_, BlockedAttentionRow>(
            "SELECT id, identifier, title, priority, updated_at FROM issues \
             WHERE company_id = $1 AND status = 'blocked' AND hidden_at IS NULL \
               AND harness_kind IS NULL \
             ORDER BY updated_at DESC LIMIT 100",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 178: activity 心跳关联接口 —— 列出某 run 关联的 issues（execution_run_id OR checkout_run_id）。
    pub async fn list_for_run(
        &self,
        company_id: Uuid,
        run_id: Uuid,
    ) -> sqlx::Result<Vec<IssueRunLinkRow>> {
        sqlx::query_as::<_, IssueRunLinkRow>(
            "SELECT i.id, i.identifier, i.title, i.status::text, i.priority::text, \
                    COALESCE(i.kind::text,'issue') \
             FROM issues i \
             WHERE i.company_id = $1 \
               AND (i.execution_run_id = $2 OR i.checkout_run_id = $2) \
             ORDER BY i.updated_at DESC LIMIT 200",
        )
        .bind(company_id)
        .bind(run_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 178: activity 心跳关联接口 —— 按 id 取单个 issue 的运行关联摘要。
    pub async fn get_run_link_summary(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<IssueRunLinkRow>> {
        sqlx::query_as::<_, IssueRunLinkRow>(
            "SELECT i.id, i.identifier, i.title, i.status::text, i.priority::text, \
                    COALESCE(i.kind::text,'issue') \
             FROM issues i WHERE i.company_id = $1 AND i.id = $2",
        )
        .bind(company_id)
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 180: file_resources 列表 —— 通过 issue_id JOIN project_artifacts 取文件元数据。
    pub async fn list_project_files(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<(String, String, Option<i64>)>> {
        sqlx::query_as(
            "SELECT a.path, a.mime_type, a.size_bytes \
             FROM project_artifacts a \
             JOIN issues i ON i.project_id = a.project_id \
             WHERE i.id = $1 ORDER BY a.created_at DESC LIMIT 50",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 180: file_resources resolve —— 仅用于检查 issue 是否存在（接口语义保留：返回 unresolved 占位）。
    pub async fn exists_for_resolution(&self, issue_id: Uuid) -> sqlx::Result<bool> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM issues WHERE id = $1 LIMIT 1")
                .bind(issue_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.is_some())
    }

    /// Round 180: file_resources content —— 取单个 artifact 的 content / mime / size。
    pub async fn get_project_file_content(
        &self,
        issue_id: Uuid,
        path: &str,
    ) -> sqlx::Result<Option<(String, Option<String>, Option<i64>)>> {
        sqlx::query_as(
            "SELECT a.content, a.mime_type, a.size_bytes \
             FROM project_artifacts a \
             JOIN issues i ON i.project_id = a.project_id \
             WHERE i.id = $1 AND a.path = $2 LIMIT 1",
        )
        .bind(issue_id)
        .bind(path)
        .fetch_optional(self.db.pool())
        .await
    }
    /// Round 185: extensions /api/companies/:id/issues/count -- group by status for visible issues.
    pub async fn count_by_status_visible(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<(String, i64)>> {
        sqlx::query_as(
            "SELECT status, COUNT(*)::bigint AS count FROM issues \
             WHERE company_id = $1 AND hidden_at IS NULL \
             GROUP BY status",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }
    /// Round 190: board_chat -- idempotent comment insert (caller provides id).
    pub async fn insert_comment_idempotent(
        &self,
        id: Uuid,
        issue_id: Uuid,
        author_user_id: &str,
        body: &str,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "INSERT INTO issue_comments (id, issue_id, author_user_id, body, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now()) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .bind(issue_id)
        .bind(author_user_id)
        .bind(body)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }



    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<IssueRow>> {
        let sql = format!("SELECT {ISSUE_COLS} FROM issues WHERE id = $1");
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    // ---------- create / update / delete ----------

    /// Round 163: 给 skill test run 创建 harness issue（专用 status='todo'，固定 title）。
    pub async fn create_harness_issue(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<()> {
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO issues (id, company_id, title, status, created_at, updated_at)
             VALUES ($1, $2, 'Skill test run', 'todo', $3, $3)",
        )
        .bind(issue_id)
        .bind(company_id)
        .bind(now)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

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

    pub async fn update_with_receipt(
        &self,
        id: Uuid,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<&str>,
        assignee_agent_id: Option<Option<Uuid>>,
        relation_changes: &IssueRelationChanges,
    ) -> sqlx::Result<Option<IssueUpdateReceipt>> {
        self.update_with_relations(
            id,
            title,
            description,
            status,
            priority,
            assignee_agent_id,
            IssueRelationUpdate::default(),
            relation_changes,
            None,
        )
        .await
    }

    pub async fn update_with_relations(
        &self,
        id: Uuid,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<&str>,
        assignee_agent_id: Option<Option<Uuid>>,
        relations: IssueRelationUpdate,
        relation_changes: &IssueRelationChanges,
        actor: Option<IssueUpdateActor>,
    ) -> sqlx::Result<Option<IssueUpdateReceipt>> {
        let mut tx = self.db.pool().begin().await?;
        let Some(existing) = sqlx::query_as::<_, IssueRow>(&format!(
            "SELECT {ISSUE_COLS} FROM issues WHERE id = $1 FOR UPDATE"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };

        if let Some(status) = status {
            if !valid_issue_status(status) {
                return Err(sqlx::Error::Protocol(format!("unknown issue status: {status}")));
            }
            let next_assignee = assignee_agent_id.unwrap_or(existing.assignee_agent_id);
            if status == "in_progress" && next_assignee.is_none() && existing.assignee_user_id.is_none() {
                return Err(sqlx::Error::Protocol(
                    "in_progress issues require an assignee".into(),
                ));
            }
            if status == "in_progress" {
                let unresolved_count: i64 = if let Some(blocker_ids) = relations.blocked_by_issue_ids.as_deref() {
                    sqlx::query_scalar(
                        "SELECT COUNT(*) FROM issues WHERE company_id=$1 AND id=ANY($2) AND status NOT IN ('done','cancelled') AND hidden_at IS NULL",
                    )
                    .bind(existing.company_id)
                    .bind(blocker_ids)
                    .fetch_one(&mut *tx)
                    .await?
                } else {
                    sqlx::query_scalar(
                        "SELECT COUNT(*) FROM issue_relations ir INNER JOIN issues blocker ON blocker.id=ir.issue_id AND blocker.company_id=ir.company_id WHERE ir.company_id=$1 AND ir.related_issue_id=$2 AND ir.type='blocks' AND blocker.status NOT IN ('done','cancelled') AND blocker.hidden_at IS NULL",
                    )
                    .bind(existing.company_id)
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?
                };
                if unresolved_count > 0 {
                    return Err(sqlx::Error::Protocol(
                        "issue is blocked by unresolved blockers".into(),
                    ));
                }
            }
        }
        if let Some(actor) = &actor {
            if let Some(agent_id) = actor.agent_id {
                let agent_exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM agents WHERE id=$1 AND company_id=$2)",
                )
                .bind(agent_id)
                .bind(existing.company_id)
                .fetch_one(&mut *tx)
                .await?;
                if !agent_exists {
                    return Err(sqlx::Error::Protocol(
                        "actor agent does not belong to the issue company".into(),
                    ));
                }
                if let Some(run_id) = actor.run_id {
                    let run_exists: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM heartbeat_runs WHERE id=$1 AND company_id=$2 AND agent_id=$3)",
                    )
                    .bind(run_id)
                    .bind(existing.company_id)
                    .bind(agent_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    if !run_exists {
                        return Err(sqlx::Error::Protocol(
                            "actor run does not belong to the actor agent".into(),
                        ));
                    }
                }
            }
        }

        let previous_label_ids: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT label_id FROM issue_labels WHERE issue_id = $1 ORDER BY label_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        let previous_blocker_ids: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT issue_id FROM issue_relations WHERE related_issue_id = $1 AND type = 'blocks' ORDER BY issue_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        if let Some(label_ids) = relations.label_ids.as_deref() {
            let unique: std::collections::BTreeSet<Uuid> = label_ids.iter().copied().collect();
            let unique_vec: Vec<Uuid> = unique.iter().copied().collect();
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM labels WHERE company_id = $1 AND id = ANY($2)",
            )
            .bind(existing.company_id)
            .bind(&unique_vec)
            .fetch_one(&mut *tx)
            .await?;
            if count != unique_vec.len() as i64 {
                return Err(sqlx::Error::Protocol(
                    "one or more labels do not belong to the issue company".into(),
                ));
            }
            sqlx::query("DELETE FROM issue_labels WHERE issue_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            for label_id in unique_vec {
                sqlx::query(
                    "INSERT INTO issue_labels (issue_id, label_id, company_id) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
                )
                .bind(id)
                .bind(label_id)
                .bind(existing.company_id)
                .execute(&mut *tx)
                .await?;
            }
        }

        if let Some(blocker_ids) = relations.blocked_by_issue_ids.as_deref() {
            let unique: std::collections::BTreeSet<Uuid> = blocker_ids.iter().copied().collect();
            if unique.contains(&id) {
                return Err(sqlx::Error::Protocol(
                    "an issue cannot be blocked by itself".into(),
                ));
            }
            let unique_vec: Vec<Uuid> = unique.iter().copied().collect();
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM issues WHERE company_id = $1 AND id = ANY($2)",
            )
            .bind(existing.company_id)
            .bind(&unique_vec)
            .fetch_one(&mut *tx)
            .await?;
            if count != unique_vec.len() as i64 {
                return Err(sqlx::Error::Protocol(
                    "blocked-by issues must belong to the same company".into(),
                ));
            }
            let edges: Vec<(Uuid, Uuid)> = sqlx::query_as(
                "SELECT issue_id, related_issue_id FROM issue_relations WHERE company_id = $1 AND type = 'blocks'",
            )
            .bind(existing.company_id)
            .fetch_all(&mut *tx)
            .await?;
            let mut graph: std::collections::HashMap<Uuid, Vec<Uuid>> = Default::default();
            for (from, to) in edges {
                graph.entry(from).or_default().push(to);
            }
            for candidate in &unique_vec {
                let mut queue = vec![id];
                let mut visited = std::collections::HashSet::from([id]);
                while let Some(current) = queue.pop() {
                    if current == *candidate {
                        return Err(sqlx::Error::Protocol(
                            "blocking relations cannot contain cycles".into(),
                        ));
                    }
                    for next in graph.get(&current).into_iter().flatten() {
                        if visited.insert(*next) {
                            queue.push(*next);
                        }
                    }
                }
            }
            sqlx::query(
                "DELETE FROM issue_relations WHERE company_id = $1 AND related_issue_id = $2 AND type = 'blocks'",
            )
            .bind(existing.company_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            for blocker_id in unique_vec {
                sqlx::query(
                    "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type, created_by_agent_id, created_by_user_id) VALUES ($1,$2,$3,'blocks',$4,$5) ON CONFLICT DO NOTHING",
                )
                .bind(existing.company_id)
                .bind(blocker_id)
                .bind(id)
                .bind(actor.as_ref().and_then(|value| value.agent_id))
                .bind(actor.as_ref().and_then(|value| value.user_id.as_deref()))
                .execute(&mut *tx)
                .await?;
            }
        }

        let issue = sqlx::query_as::<_, IssueRow>(&format!(
            "UPDATE issues SET title=COALESCE($2,title), description=COALESCE($3,description), \
             status=COALESCE($4,status), priority=COALESCE($5,priority), \
             assignee_agent_id=COALESCE($6,assignee_agent_id), \
             started_at=CASE WHEN $4='in_progress' AND started_at IS NULL THEN now() ELSE started_at END, \
             completed_at=CASE WHEN $4='done' THEN now() WHEN $4 IS NOT NULL THEN NULL ELSE completed_at END, \
             cancelled_at=CASE WHEN $4='cancelled' THEN now() WHEN $4 IS NOT NULL THEN NULL ELSE cancelled_at END, \
             blocked_transition_at=CASE WHEN $4='blocked' AND status<>'blocked' THEN now() WHEN $4 IS NOT NULL AND $4<>'blocked' THEN NULL ELSE blocked_transition_at END, \
             blocked_owner_notified_at=CASE WHEN $4='blocked' AND status<>'blocked' THEN NULL WHEN $4 IS NOT NULL AND $4<>'blocked' THEN NULL ELSE blocked_owner_notified_at END, \
             unblock_descriptor=CASE WHEN $4 IS NOT NULL AND $4<>'blocked' THEN NULL ELSE unblock_descriptor END, \
             checkout_run_id=CASE WHEN $4 IS NOT NULL AND $4<>'in_progress' THEN NULL ELSE checkout_run_id END, \
             execution_run_id=CASE WHEN $4 IS NOT NULL AND $4<>'in_progress' THEN NULL ELSE execution_run_id END, \
             execution_agent_name_key=CASE WHEN $4 IS NOT NULL AND $4<>'in_progress' THEN NULL ELSE execution_agent_name_key END, \
             execution_locked_at=CASE WHEN $4 IS NOT NULL AND $4<>'in_progress' THEN NULL ELSE execution_locked_at END, \
             updated_at=now() \
             WHERE id=$1 RETURNING {ISSUE_COLS}"
        ))
        .bind(id)
        .bind(title)
        .bind(description)
        .bind(status)
        .bind(priority)
        .bind(assignee_agent_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(issue) = issue else {
            return Ok(None);
        };
        let existing_value = serde_json::to_value(&existing).unwrap_or_default();
        let updated_value = serde_json::to_value(&issue).unwrap_or_default();
        let mut effective_relations = relation_changes.clone();
        if relations.label_ids.is_some() {
            effective_relations.label_ids = Some((
                previous_label_ids.iter().map(|(value,)| value.to_string()).collect(),
                relations.label_ids.unwrap_or_default().iter().map(Uuid::to_string).collect(),
            ));
        }
        if relations.blocked_by_issue_ids.is_some() {
            effective_relations.blocked_by_issue_ids = Some((
                previous_blocker_ids.iter().map(|(value,)| value.to_string()).collect(),
                relations
                    .blocked_by_issue_ids
                    .unwrap_or_default()
                    .iter()
                    .map(Uuid::to_string)
                    .collect(),
            ));
        }
        let empty = serde_json::Map::new();
        let changes = build_issue_changes(
            existing_value.as_object().unwrap_or(&empty),
            updated_value.as_object().unwrap_or(&empty),
            &effective_relations,
        );
        if existing.status != issue.status
            && matches!(issue.status.as_str(), "done" | "cancelled" | "blocked")
        {
            let effect_actor = TerminalEffectActor {
                agent_id: actor.as_ref().and_then(|value| value.agent_id),
                user_id: actor.as_ref().and_then(|value| value.user_id.as_deref()),
                run_id: actor.as_ref().and_then(|value| value.run_id),
            };
            apply_issue_terminal_effects(
                &mut tx,
                &TerminalEffectIssue {
                    id: issue.id,
                    company_id: issue.company_id,
                    identifier: issue.identifier.as_deref(),
                    title: &issue.title,
                    status: &issue.status,
                },
                &effect_actor,
            )
            .await?;
        }
        if let Some(actor) = &actor {
            let (actor_type, actor_id) = if let Some(agent_id) = actor.agent_id {
                ("agent", agent_id.to_string())
            } else if let Some(user_id) = actor.user_id.as_deref() {
                ("user", user_id.to_owned())
            } else {
                ("system", "issue_service".to_owned())
            };
            sqlx::query(
                "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type, entity_id, agent_id, run_id, details) VALUES ($1,$2,$3,'issue.updated','issue',$4,$5,$6,$7)",
            )
            .bind(existing.company_id)
            .bind(actor_type)
            .bind(actor_id)
            .bind(id.to_string())
            .bind(actor.agent_id)
            .bind(actor.run_id)
            .bind(serde_json::json!({ "changes": changes }))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Some(IssueUpdateReceipt { issue, changes }))
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

    // ---------- checkout / release ----------

    /// Round 126: checkout issue（UPDATE assignee_agent_id + checkout_run_id）。
    /// 原子操作：单 SQL UPDATE ... RETURNING，返回 (company_id, status) 二元组。
    pub async fn checkout(
        &self,
        id: Uuid,
        agent_id: Uuid,
        run_id: Option<Uuid>,
    ) -> sqlx::Result<Option<(Uuid, String)>> {
        let sql = format!(
            "UPDATE issues SET assignee_agent_id = $1, checkout_run_id = $2, updated_at = now()              WHERE id = $3 RETURNING company_id, status"
        );
        sqlx::query_as::<_, (Uuid, String)>(&sql)
            .bind(agent_id)
            .bind(run_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

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
    /// Upsert active recovery action for a source issue.
    ///
    /// 对齐 Node `upsertSourceScopedUnlocked`：
    /// - 已有 active 行：增量更新 `attempt_count` 并保留历史 owner / evidence
    /// - 无 active 行：插入新行，`attempt_count = 1`
    /// - 在 `issue_recovery_actions_active_source_uq` /
    ///   `issue_recovery_actions_active_fingerprint_uq` 上 retry 最多 3 次
    ///   （处理并发首次插入竞争）
    ///
    /// 调用方需要在更外层使用 advisory lock 来按 (company_id, source_issue_id)
    /// 串行化，本函数只负责单次原子 upsert。
    pub async fn upsert_recovery_action(
        &self,
        input: &UpsertRecoveryAction,
    ) -> sqlx::Result<IssueRecoveryActionRow> {
        const MAX_RETRIES: u32 = 3;
        let owner_type = input
            .owner_type
            .clone()
            .or_else(|| {
                if input.owner_agent_id.is_some() {
                    Some("agent".to_string())
                } else {
                    Some("board".to_string())
                }
            })
            .unwrap_or_else(|| "board".to_string());
        let mut last_err: Option<sqlx::Error> = None;
        for _attempt in 0..MAX_RETRIES {
            // 1) 尝试更新现有 active 行
            let existing: Option<IssueRecoveryActionRow> = sqlx::query_as(
                "UPDATE issue_recovery_actions SET \
                    recovery_issue_id = $2, kind = $3, status = 'active', \
                    owner_type = $4, owner_agent_id = $5, owner_user_id = $6, \
                    previous_owner_agent_id = $7, return_owner_agent_id = $8, \
                    cause = $9, fingerprint = $10, evidence = $11, next_action = $12, \
                    wake_policy = $13, monitor_policy = $14, \
                    attempt_count = attempt_count + 1, max_attempts = $15, \
                    timeout_at = $16, last_attempt_at = COALESCE($17, now()), \
                    outcome = NULL, resolution_note = NULL, resolved_at = NULL, \
                    updated_at = now() \
                 WHERE source_issue_id = $1 AND status = 'active' \
                 RETURNING id, company_id, source_issue_id, recovery_issue_id, kind, status, \
                    owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                    return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                    wake_policy, monitor_policy, attempt_count, max_attempts, \
                    timeout_at, last_attempt_at, outcome, resolution_note, resolved_at, \
                    created_at, updated_at",
            )
            .bind(input.source_issue_id)
            .bind(input.recovery_issue_id)
            .bind(&input.kind)
            .bind(&owner_type)
            .bind(input.owner_agent_id)
            .bind(input.owner_user_id.as_deref())
            .bind(input.previous_owner_agent_id)
            .bind(input.return_owner_agent_id)
            .bind(&input.cause)
            .bind(&input.fingerprint)
            .bind(input.evidence.clone().unwrap_or(serde_json::Value::Null))
            .bind(&input.next_action)
            .bind(input.wake_policy.clone())
            .bind(input.monitor_policy.clone())
            .bind(input.max_attempts)
            .bind(input.timeout_at)
            .bind(input.last_attempt_at)
            .fetch_optional(self.db.pool())
            .await?;
            if let Some(row) = existing {
                return Ok(row);
            }
            // 2) 没有 active 行 → 插入新行
            let inserted: Result<IssueRecoveryActionRow, sqlx::Error> = sqlx::query_as(
                "INSERT INTO issue_recovery_actions ( \
                    company_id, source_issue_id, recovery_issue_id, kind, status, \
                    owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                    return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                    wake_policy, monitor_policy, attempt_count, max_attempts, \
                    timeout_at, last_attempt_at \
                 ) VALUES ( \
                    $1, $2, $3, $4, 'active', $5, $6, $7, $8, $9, $10, $11, $12, $13, \
                    $14, $15, 1, $16, $17, $18 \
                 ) RETURNING id, company_id, source_issue_id, recovery_issue_id, kind, status, \
                    owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                    return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                    wake_policy, monitor_policy, attempt_count, max_attempts, \
                    timeout_at, last_attempt_at, outcome, resolution_note, resolved_at, \
                    created_at, updated_at",
            )
            .bind(input.company_id)
            .bind(input.source_issue_id)
            .bind(input.recovery_issue_id)
            .bind(&input.kind)
            .bind(&owner_type)
            .bind(input.owner_agent_id)
            .bind(input.owner_user_id.as_deref())
            .bind(input.previous_owner_agent_id)
            .bind(input.return_owner_agent_id)
            .bind(&input.cause)
            .bind(&input.fingerprint)
            .bind(input.evidence.clone().unwrap_or(serde_json::Value::Null))
            .bind(&input.next_action)
            .bind(input.wake_policy.clone())
            .bind(input.monitor_policy.clone())
            .bind(input.max_attempts)
            .bind(input.timeout_at)
            .bind(input.last_attempt_at)
            .fetch_one(self.db.pool())
            .await;
            match inserted {
                Ok(row) => return Ok(row),
                Err(sqlx::Error::Database(db_err)) if is_unique_recovery_conflict(db_err.as_ref()) => {
                    // 并发竞争：另一个事务抢先插入 → 下次循环走 update 路径
                    last_err = Some(sqlx::Error::Database(db_err));
                    continue;
                }
                Err(other) => return Err(other),
            }
        }
        Err(last_err.unwrap_or_else(|| sqlx::Error::RowNotFound))
    }

    /// Bulk 拉取一组 source issues 的「最近一条 active」recovery action。
    ///
    /// 对齐 Node `listActiveForIssues`：按 `source_issue_id` 分组，每个 issue 只
    /// 保留 `updated_at` 最新的一条 active 行（status IN ('active','escalated')）。
    /// 空输入 → 空 Map。
    pub async fn list_active_recovery_actions_for_issues(
        &self,
        company_id: Uuid,
        source_issue_ids: &[Uuid],
    ) -> sqlx::Result<std::collections::HashMap<Uuid, IssueRecoveryActionRow>> {
        use std::collections::HashMap;
        let mut out: HashMap<Uuid, IssueRecoveryActionRow> = HashMap::new();
        if source_issue_ids.is_empty() {
            return Ok(out);
        }
        let rows: Vec<IssueRecoveryActionRow> = sqlx::query_as(
            "SELECT id, company_id, source_issue_id, recovery_issue_id, kind, status, \
                    owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                    return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                    wake_policy, monitor_policy, attempt_count, max_attempts, \
                    timeout_at, last_attempt_at, outcome, resolution_note, resolved_at, \
                    created_at, updated_at \
             FROM issue_recovery_actions \
             WHERE company_id = $1 \
               AND source_issue_id = ANY($2::uuid[]) \
               AND status IN ('active', 'escalated') \
             ORDER BY source_issue_id, updated_at DESC",
        )
        .bind(company_id)
        .bind(source_issue_ids)
        .fetch_all(self.db.pool())
        .await?;
        for row in rows {
            // 只保留每个 source 的第一条（已按 updated_at DESC 排序）
            out.entry(row.source_issue_id).or_insert(row);
        }
        Ok(out)
    }

    /// 按 source_issue_id + 可选过滤条件 resolve active recovery action。
    ///
    /// 对齐 Node `resolveActiveForIssue`：
    /// - 必须匹配 company_id + source_issue_id + status IN ('active','escalated')
    /// - 可选 action_id / kind / cause / fingerprint 进一步过滤
    /// - 写 status / outcome / resolution_note / resolved_at / updated_at
    ///
    /// 返回被更新的最新行（可能 None：未匹配到任何 active 行）。
    pub async fn resolve_active_recovery_for_issue(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
        action_id: Option<Uuid>,
        kind: Option<&str>,
        cause: Option<&str>,
        fingerprint: Option<&str>,
        status: &str,
        outcome: &str,
        resolution_note: Option<&str>,
    ) -> sqlx::Result<Option<IssueRecoveryActionRow>> {
        let mut sql = String::from(
            "UPDATE issue_recovery_actions SET \
                status = $3, outcome = $4, resolution_note = $5, \
                resolved_at = now(), updated_at = now() \
             WHERE company_id = $1 AND source_issue_id = $2 \
               AND status IN ('active', 'escalated')",
        );
        let mut bind_idx = 6;
        if action_id.is_some() {
            sql.push_str(&format!(" AND id = ${}", bind_idx));
            bind_idx += 1;
        }
        if kind.is_some() {
            sql.push_str(&format!(" AND kind = ${}", bind_idx));
            bind_idx += 1;
        }
        if cause.is_some() {
            sql.push_str(&format!(" AND cause = ${}", bind_idx));
            bind_idx += 1;
        }
        if fingerprint.is_some() {
            sql.push_str(&format!(" AND fingerprint = ${}", bind_idx));
        }
        sql.push_str(
            " RETURNING id, company_id, source_issue_id, recovery_issue_id, kind, status, \
                    owner_type, owner_agent_id, owner_user_id, previous_owner_agent_id, \
                    return_owner_agent_id, cause, fingerprint, evidence, next_action, \
                    wake_policy, monitor_policy, attempt_count, max_attempts, \
                    timeout_at, last_attempt_at, outcome, resolution_note, resolved_at, \
                    created_at, updated_at",
        );
        let mut q = sqlx::query_as::<_, IssueRecoveryActionRow>(&sql)
            .bind(company_id)
            .bind(source_issue_id)
            .bind(status)
            .bind(outcome)
            .bind(resolution_note);
        if let Some(id) = action_id {
            q = q.bind(id);
        }
        if let Some(k) = kind {
            q = q.bind(k);
        }
        if let Some(c) = cause {
            q = q.bind(c);
        }
        if let Some(f) = fingerprint {
            q = q.bind(f);
        }
        q.fetch_optional(self.db.pool())
            .await
    }

    /// 把超时未结的 active / escalated recovery action 标记为 cancelled。
    ///
    /// 对齐 Node `expireRecoveryActions`（background cleanup）：
    /// - 限定 `timeout_at IS NOT NULL AND timeout_at < now()`
    /// - 写 `status='cancelled'`、`outcome='timed_out'`、`resolved_at=now()`、`updated_at=now()`
    /// - 返回被取消的行数
    pub async fn expire_timed_out_recovery_actions(
        &self,
        company_id: Option<Uuid>,
    ) -> sqlx::Result<u64> {
        let result = if let Some(cid) = company_id {
            sqlx::query(
                "UPDATE issue_recovery_actions SET \
                    status = 'cancelled', outcome = 'timed_out', \
                    resolved_at = now(), updated_at = now() \
                 WHERE company_id = $1 \
                   AND status IN ('active', 'escalated') \
                   AND timeout_at IS NOT NULL AND timeout_at < now()",
            )
            .bind(cid)
            .execute(self.db.pool())
            .await?
        } else {
            sqlx::query(
                "UPDATE issue_recovery_actions SET \
                    status = 'cancelled', outcome = 'timed_out', \
                    resolved_at = now(), updated_at = now() \
                 WHERE status IN ('active', 'escalated') \
                   AND timeout_at IS NOT NULL AND timeout_at < now()",
            )
            .execute(self.db.pool())
            .await?
        };
        Ok(result.rows_affected())
    }



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

    /// 事务化「设为 primary」：在同一 issue + type 下先把所有 work_product 的
    /// `is_primary` 清空，再把目标行设为 primary。返回目标行最新状态。
    ///
    /// 对齐 Node `workProductService.setPrimary` 的事务语义：
    /// - 同一 issue + type 至多一条 `is_primary = true`
    /// - 跨 type 不互相影响
    pub async fn set_as_primary_work_product(
        &self,
        id: Uuid,
    ) -> sqlx::Result<Option<IssueWorkProductRow>> {
        let mut tx = self.db.pool().begin().await?;
        // 取出目标的 (issue_id, type) 用于限定同 type 清空
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT issue_id, type FROM issue_work_products WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((issue_id, kind)) = row else {
            tx.rollback().await.ok();
            return Ok(None);
        };
        // 清空同 issue + type 的其他 primary
        sqlx::query(
            "UPDATE issue_work_products SET is_primary = false, updated_at = now() \
             WHERE issue_id = $1 AND type = $2 AND id != $3 AND is_primary = true",
        )
        .bind(issue_id)
        .bind(&kind)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        // 设置目标为 primary
        let updated: Option<IssueWorkProductRow> = sqlx::query_as(
            "UPDATE issue_work_products SET is_primary = true, updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, project_id, issue_id, type as type_, provider, \
                external_id, title, status, review_state, is_primary, health_status, \
                summary, metadata, created_by_run_id, source_trust, \
                created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(updated)
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

    pub async fn has_actionable_timer_work(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
    ) -> sqlx::Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM issues \
             WHERE company_id=$1 AND assignee_agent_id=$2 AND assignee_user_id IS NULL \
               AND hidden_at IS NULL AND status IN ('todo','in_progress'))",
        )
        .bind(company_id)
        .bind(agent_id)
        .fetch_one(self.db.pool())
        .await
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

    /// Returns the IDs of unresolved blockers for an issue. A blocker is
    /// unresolved when it is not in `done` or `cancelled` status and is not
    /// hidden. Mirrors Node `evaluateIssueExecutionReadiness` in
    /// `services/heartbeat.ts`.
    pub async fn unresolved_blocker_ids(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<Uuid>> {
        sqlx::query_scalar(
            "SELECT ir.issue_id FROM issue_relations ir              INNER JOIN issues blocker ON blocker.id = ir.issue_id                 AND blocker.company_id = ir.company_id              WHERE ir.company_id = $1 AND ir.related_issue_id = $2                AND ir.type = 'blocks'                AND blocker.status NOT IN ('done', 'cancelled')                AND blocker.hidden_at IS NULL",
        )
        .bind(company_id)
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Convenience helper for dependency readiness: returns the list of
    /// unresolved blocker IDs for an issue, or an empty list when the issue
    /// has no blockers or the issue itself is missing.
    pub async fn unresolved_blockers_for(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<Uuid>> {
        let issue = match self.get(issue_id).await? {
            Some(issue) => issue,
            None => return Ok(Vec::new()),
        };
        self.unresolved_blocker_ids(issue.company_id, issue_id).await
    }

    // =========================================================================
    // Round 161: issues.rs route 仓储化新增方法
    // =========================================================================

    /// Round 161: issue_heartbeat_context — 取 6-tuple (company, assignee, project, project_workspace, status, work_mode)。
    pub async fn heartbeat_context_inputs(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<(Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>, String, String)>> {
        let row: Option<(Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>, String, String)> = sqlx::query_as(
            "SELECT company_id, assignee_agent_id, project_id, project_workspace_id, status, work_mode              FROM issues WHERE id = $1",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 161: list_company_issues — 基本 5-tuple (id, identifier, title, status, priority) + limit。
    pub async fn list_company_basic(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<(Uuid, String, String, String, Option<String>)>> {
        let rows: Vec<(Uuid, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT id, identifier, title, status, priority FROM issues              WHERE company_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(company_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .unwrap_or_default();
        Ok(rows)
    }

    /// Round 161: start_run_inputs — (company_id, project_id, assignee_agent_id)。
    pub async fn start_run_inputs(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<(Uuid, Uuid, Option<Uuid>)>> {
        let row: Option<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
            "SELECT company_id, project_id, assignee_agent_id FROM issues WHERE id = $1",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 161: 查单条 issue_comment（限定 issue_id + 删除过滤）。
    pub async fn find_one_comment(
        &self,
        issue_id: Uuid,
        comment_id: Uuid,
    ) -> sqlx::Result<
        Option<(Uuid, Uuid, Option<String>, Option<Uuid>, String, pc_core::Timestamp)>,
    > {
        let row: Option<(Uuid, Uuid, Option<String>, Option<Uuid>, String, pc_core::Timestamp)> = sqlx::query_as(
            "SELECT id, issue_id, author_user_id, author_agent_id, body, created_at              FROM issue_comments WHERE issue_id=$1 AND id=$2 AND deleted_at IS NULL",
        )
        .bind(issue_id)
        .bind(comment_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 161: issue_doc_exists — key 是否已存在。
    pub async fn issue_doc_exists(&self, issue_id: Uuid, key: &str) -> sqlx::Result<bool> {
        let v: Option<(bool,)> = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM issue_documents WHERE issue_id=$1 AND key=$2)",
        )
        .bind(issue_id)
        .bind(key)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(v.map(|(b,)| b).unwrap_or(false))
    }

    /// Round 161: UPDATE issue_documents content (存在时)。
    pub async fn update_issue_doc_content(
        &self,
        issue_id: Uuid,
        key: &str,
        content: &serde_json::Value,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE issue_documents SET content=$1, updated_at=now()              WHERE issue_id=$2 AND key=$3",
        )
        .bind(content)
        .bind(issue_id)
        .bind(key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 161: INSERT issue_documents (从 issues 取 company_id)。
    pub async fn insert_issue_doc(
        &self,
        issue_id: Uuid,
        key: &str,
        content: &serde_json::Value,
        title: Option<&str>,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "INSERT INTO issue_documents (id, issue_id, key, content, title)              SELECT gen_random_uuid(), $1, $2, $3, $4 FROM issues WHERE id=$1",
        )
        .bind(issue_id)
        .bind(key)
        .bind(content)
        .bind(title)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 161: 软删除 issue_document (UPDATE deleted_at)。
    pub async fn soft_delete_issue_doc(
        &self,
        issue_id: Uuid,
        key: &str,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE issue_documents SET deleted_at=now()              WHERE issue_id=$1 AND key=$2 AND deleted_at IS NULL",
        )
        .bind(issue_id)
        .bind(key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 161: 设置 issue_document current_revision_id。
    pub async fn set_issue_doc_current_revision(
        &self,
        issue_id: Uuid,
        key: &str,
        revision_id: Uuid,
    ) -> sqlx::Result<u64> {
        let n = sqlx::query(
            "UPDATE issue_documents SET current_revision_id=$1, updated_at=now()              WHERE issue_id=$2 AND key=$3",
        )
        .bind(revision_id)
        .bind(issue_id)
        .bind(key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Round 161: attachment_content_meta — JOIN issue_attachments + assets。
    pub async fn attachment_content_meta(
        &self,
        attachment_id: Uuid,
    ) -> sqlx::Result<
        Option<(Uuid, String, String, String, i32, Option<String>)>,
    > {
        let row: Option<(Uuid, String, String, String, i32, Option<String>)> = sqlx::query_as(
            "SELECT a.company_id, a.provider, a.object_key, a.content_type, a.byte_size, a.original_filename              FROM issue_attachments ia              INNER JOIN assets a ON a.id = ia.asset_id              WHERE ia.id = $1",
        )
        .bind(attachment_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 168: 统计 company 的 visible issues（hidden_at IS NULL AND harness_kind IS NULL）按 status 分组。
    pub async fn count_visible_by_status(&self, company_id: Uuid) -> sqlx::Result<Vec<(String, i64)>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT status, COUNT(*)::bigint FROM issues \
             WHERE company_id = $1 AND hidden_at IS NULL AND harness_kind IS NULL \
             GROUP BY status",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// Round 171: 按 status 拆分 issues 计数（blocked/in_progress/needs_review）。
    pub async fn status_breakdown_visible(&self, company_id: Uuid) -> sqlx::Result<(i64, i64, i64)> {
        let row: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
                COUNT(*) FILTER (WHERE status = 'blocked')::bigint, \
                COUNT(*) FILTER (WHERE status = 'in_progress')::bigint, \
                COUNT(*) FILTER (WHERE status = 'needs_review')::bigint \
             FROM issues WHERE company_id = $1 AND hidden_at IS NULL",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await
        .unwrap_or((0, 0, 0));
        Ok(row)
    }

    /// Round 171: 统计未读 issues（assignee_user_id IS NULL 且 7 天内创建）。
    pub async fn count_unread_visible(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM issues WHERE company_id = $1 AND hidden_at IS NULL \
             AND (assignee_user_id IS NULL OR assignee_user_id = '') \
             AND created_at > now() - interval '7 days'",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await
        .unwrap_or(0);
        Ok(n)
    }

    /// Round 172: 取 issue 的 checkout 摘要（assignee + prev run）。
    pub async fn get_checkout_snapshot(&self, issue_id: Uuid) -> sqlx::Result<Option<(Uuid, Option<Uuid>, Option<Uuid>)>> {
        let row: Option<(Uuid, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, assignee_agent_id, checkout_run_id FROM issues WHERE id = $1",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 172: 设置 issue 的 checkout_run_id + execution_locked_at。
    pub async fn set_checkout_run(&self, issue_id: Uuid, run_id: Uuid) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE issues SET checkout_run_id = $1, execution_locked_at = now(), updated_at = now() \
             WHERE id = $2",
        )
        .bind(run_id)
        .bind(issue_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 172: 写入 issue_checkout_locks（幂等）。
    pub async fn insert_checkout_lock(
        &self,
        issue_id: Uuid,
        run_id: Uuid,
        actor_type: &str,
        actor_id: &str,
        strategy: &str,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "INSERT INTO issue_checkout_locks \
                (issue_id, run_id, actor_type, actor_id, strategy, status, created_at) \
             VALUES ($1, $2, $3, $4, $5, 'active', now()) \
             ON CONFLICT (issue_id, run_id) DO NOTHING",
        )
        .bind(issue_id)
        .bind(run_id)
        .bind(actor_type)
        .bind(actor_id)
        .bind(strategy)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 172: 取 issue 的 (id, assignee_agent_id)。
    pub async fn get_id_and_assignee(&self, issue_id: Uuid) -> sqlx::Result<Option<(Uuid, Option<Uuid>)>> {
        let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, assignee_agent_id FROM issues WHERE id = $1",
        )
        .bind(issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 172: 写入 agent_wakeup_requests。
    pub async fn enqueue_agent_wakeup(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
        source: &str,
        reason: &str,
        payload: &Value,
        actor_type: &str,
        actor_id: &str,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "INSERT INTO agent_wakeup_requests \
                (company_id, agent_id, source, reason, payload, status, requested_by_actor_type, requested_by_actor_id) \
             VALUES ($1, $2, $3, $4, $5, 'queued', $6, $7)",
        )
        .bind(company_id)
        .bind(agent_id)
        .bind(source)
        .bind(reason)
        .bind(payload)
        .bind(actor_type)
        .bind(actor_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 172: 取 agent 的 company_id。
    pub async fn get_agent_company_id(&self, agent_id: Uuid) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT company_id FROM agents WHERE id = $1",
        )
        .bind(agent_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(c,)| c))
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

#[cfg(test)]
mod issue_update_tests {
    use super::valid_issue_status;

    #[test]
    fn accepts_all_node_issue_statuses() {
        for status in [
            "backlog",
            "todo",
            "in_progress",
            "in_review",
            "done",
            "blocked",
            "cancelled",
        ] {
            assert!(valid_issue_status(status), "{status}");
        }
    }

    #[test]
    fn rejects_unknown_issue_statuses() {
        assert!(!valid_issue_status("completed"));
        assert!(!valid_issue_status("open"));
        assert!(!valid_issue_status(""));
    }
}


/// Detect Postgres unique constraint conflict on the active-recovery indexes.
/// 对齐 Node `isUniqueRecoveryActionConflict`。
fn is_unique_recovery_conflict(err: &dyn sqlx::error::DatabaseError) -> bool {
    if err.code().as_deref() != Some("23505") {
        return false;
    }
    let constraint = err.constraint().unwrap_or("");
    if constraint.contains("issue_recovery_actions_active_source_uq")
        || constraint.contains("issue_recovery_actions_active_fingerprint_uq")
    {
        return true;
    }
    let msg = err.message();
    msg.contains("issue_recovery_actions_active_source_uq")
        || msg.contains("issue_recovery_actions_active_fingerprint_uq")
}
