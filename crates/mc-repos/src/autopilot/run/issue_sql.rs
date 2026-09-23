//! `create_issue` 线的 SQL —— `autopilot/run.rs` 的 `run` 子模块（R7 800 行硬上限拆分）。
//!
//! 与 `run.rs` 同属一条链：编号分配 → 重复守卫 → issue 插入 → 订阅者扇出，全部跑在**调用方
//! 的事务**上（为什么要 tx、以及两套来源表示，见 `run.rs` 模块头）。条目由 `run.rs` 重导出，
//! 外部路径仍是 `mc_repos::autopilot::run::*`。
//!
//! 子模块落在 `run/` 目录而不是新增顶层文件：`autopilot/mod.rs` 是 M5-1 的写集（本切片只读），
//! 不能在它里面加 `pub mod`。

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::Result;

// ---------------------------------------------------------------------------
// create_issue 线：编号 / 重复守卫 / issue 插入 / 订阅者扇出
// ---------------------------------------------------------------------------

/// 一段 issue（create_issue 线要用到的列）。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotIssueRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub number: i32,
    pub identifier: String,
    pub title: String,
    pub status: String,
    pub assignee_type: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub origin_type: Option<String>,
    pub origin_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// 重复守卫命中的既有 issue（`errDispatchSkipped{ReasonAlreadyActive}` 的载荷）。
#[derive(Debug, Clone, FromRow)]
pub struct DuplicateAutopilotIssue {
    pub id: Uuid,
    pub identifier: String,
    pub title: String,
    pub status: String,
}

/// `AllocateIssueNumber` 的本地形态：**tx 内** `MAX(number)+1`（唯一性由
/// `UNIQUE(workspace_id, number)` + 外层冲突重试保证）。
pub async fn next_issue_number(conn: &mut PgConnection, workspace_id: Uuid) -> Result<i32> {
    sqlx::query_scalar("SELECT COALESCE(MAX(number), 0) + 1 FROM issue WHERE workspace_id = $1")
        .bind(workspace_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_sqlx_err)
}

/// `issueposition.NextTopPosition`：autopilot 建的 issue 落在 `todo` 列**顶部**。
pub async fn next_top_position(
    conn: &mut PgConnection,
    workspace_id: Uuid,
    status: &str,
) -> Result<f64> {
    let min: f64 = sqlx::query_scalar(
        "SELECT COALESCE(MIN(position), 0) FROM issue WHERE workspace_id = $1 AND status = $2",
    )
    .bind(workspace_id)
    .bind(status)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(min - 1.0)
}

/// `LockIssueDuplicateKey`：按**归一化标题**取事务级 advisory lock。
///
/// 同一 (workspace, project, 标题) 的并发派发因此串行：先到者插入，后到者在
/// [`find_recent_duplicate_issue`] 里看见它并被判 `already_active`。
pub async fn lock_duplicate_key(conn: &mut PgConnection, key: &str) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(key)
        .execute(&mut *conn)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

/// `FindRecentAutopilotDuplicateIssue`：同源、同项目、同归一化标题、窗口期内且**已有在飞 run**
/// 的 issue。
///
/// `project_id` 用 `IS NOT DISTINCT FROM` —— 两边都 NULL 也算同项目（上游逐字）。
pub async fn find_recent_duplicate_issue(
    conn: &mut PgConnection,
    workspace_id: Uuid,
    autopilot_id: Uuid,
    project_id: Option<Uuid>,
    normalized_title: &str,
    created_after: DateTime<Utc>,
) -> Result<Option<DuplicateAutopilotIssue>> {
    sqlx::query_as::<_, DuplicateAutopilotIssue>(
        "SELECT i.id, i.identifier, i.title, i.status FROM issue i \
         WHERE i.workspace_id = $1 \
           AND NOT EXISTS (SELECT 1 FROM issue_status s WHERE s.workspace_id = $1 \
                           AND s.category = 'closed' AND s.key = i.status) \
           AND i.status NOT IN ('done', 'cancelled') \
           AND i.triage_state IS NULL \
           AND i.origin_type = 'autopilot' \
           AND i.origin_id = $2 \
           AND i.project_id IS NOT DISTINCT FROM $3 \
           AND lower(btrim(regexp_replace(i.title, '[[:space:]]+', ' ', 'g'))) = $4 \
           AND i.created_at >= $5 \
           AND EXISTS (SELECT 1 FROM autopilot_run r \
                       WHERE r.issue_id = i.id AND r.autopilot_id = i.origin_id \
                         AND r.status IN ('issue_created', 'running', 'completed')) \
         ORDER BY i.created_at ASC LIMIT 1",
    )
    .bind(workspace_id)
    .bind(autopilot_id)
    .bind(project_id)
    .bind(normalized_title)
    .bind(created_after)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `CreateIssueWithOrigin` + 本地 `identifier` / `origin` 列（两套来源表示，见模块头）。
pub async fn insert_issue(
    conn: &mut PgConnection,
    new: &NewAutopilotIssue,
) -> Result<AutopilotIssueRow> {
    sqlx::query_as::<_, AutopilotIssueRow>(
        "INSERT INTO issue (id, workspace_id, number, identifier, title, description, status, \
             priority, assignee_type, assignee_id, creator_type, creator_id, position, \
             project_id, origin, origin_type, origin_id, last_activity_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'todo', 'none', $7, $8, 'agent', $9, $10, \
             $11, $12, 'autopilot', $13, now()) \
         RETURNING id, workspace_id, number, identifier, title, status, assignee_type, \
             assignee_id, project_id, origin_type, origin_id, created_at",
    )
    .bind(new.id)
    .bind(new.workspace_id)
    .bind(new.number)
    .bind(new.identifier.as_str())
    .bind(new.title.as_str())
    .bind(new.description.as_deref())
    .bind(new.assignee_type.as_str())
    .bind(new.assignee_id)
    .bind(new.creator_id)
    .bind(new.position)
    .bind(new.project_id)
    .bind(new.origin.as_str())
    .bind(new.autopilot_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// [`insert_issue`] 的入参。
#[derive(Debug, Clone)]
pub struct NewAutopilotIssue {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub number: i32,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub assignee_type: String,
    pub assignee_id: Uuid,
    /// issue 的 creator 是**执行 agent**（派发目标 / squad leader），不是配置 autopilot 的人
    /// —— 上游 `dispatchCreateIssue` 注释逐字（MUL-6680 的 `creator_type='agent'`）。
    pub creator_id: Uuid,
    pub position: f64,
    pub project_id: Option<Uuid>,
    /// 本仓 local-only `issue.origin` 取值（`IssueOrigin::AutopilotRun` ⇒ `"autopilot_run"`）。
    pub origin: String,
    /// 上游 `origin_id`（= autopilot id）。
    pub autopilot_id: Uuid,
}

/// `ListAutopilotSubscribers` + `AddIssueSubscriber(reason='autopilot')` 一条语句的形态。
///
/// 上游在 issue 插入的**同一个 tx** 里扇出订阅者模板，好让 `EventIssueCreated` 的监听者第一次
/// 就看到完整订阅集合（否则会和「补齐模板」的监听器抢跑）。
pub async fn insert_issue_subscribers(
    conn: &mut PgConnection,
    issue_id: Uuid,
    autopilot_id: Uuid,
) -> Result<Vec<(String, Uuid)>> {
    sqlx::query_as::<_, (String, Uuid)>(
        "INSERT INTO issue_subscriber (issue_id, user_type, user_id, reason) \
         SELECT $1, s.user_type, s.user_id, 'autopilot' FROM autopilot_subscriber s \
         WHERE s.autopilot_id = $2 \
         ON CONFLICT (issue_id, user_type, user_id) DO NOTHING \
         RETURNING user_type, user_id",
    )
    .bind(issue_id)
    .bind(autopilot_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}
