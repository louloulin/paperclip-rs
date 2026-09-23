//! `issue_wakeup` 本体的读写 + 锁 + issue 级列表（上游 `db/queries/wakeup.sql`
//! 的 47-162 段，除证据收据见 [`super::receipt`]）。
//!
//! 上游 query 名 → 本文件函数的对应关系（实现时逐条对照，行号为 `wakeup.sql`）：
//!
//! | 上游 query | 行 | 本文件 |
//! | --- | ---: | --- |
//! | `CreateIssueWakeup` | 1 | [`create`] |
//! | `ListIssueWakeups` | 4 | [`list_issue_wakeups`] |
//! | `GetIssueWakeup` | 47 | [`get_in_workspace`] |
//! | `LockIssueWakeup` | 49 | [`lock`] |
//! | `LockWakeupIssue` | 51 | [`lock_issue`] |
//! | `LockWakeupSourceTask` | 53 | [`lock_source_task`]（`FOR UPDATE NOWAIT` → 409 `wakeup_source_busy`） |
//! | `CancelUnstartedWakeupTasks` | 55 | [`cancel_unstarted_wakeup_tasks`] |
//! | `DisableIssueWakeups` | 58 | [`disable_issue_wakeups`] |
//! | `CancelUnstartedIssueWakeupTasks` | 62 | [`cancel_unstarted_issue_wakeup_tasks`] |
//! | `ListReadyWakeups` | 65 | [`ready_wakeups`] |
//! | `AdvanceIssueWakeup` | 94 | [`advance`] |
//! | `FindPendingWakeupTask` | 96 | [`find_pending_wakeup_task`] |
//! | `LocklessWakeup` | 152 | [`lockless`] |
//! | `NoteWakeupFailure` | 157 | [`note_failure`] |
//! | `TouchWakeupDispatch` | 159 | [`touch_dispatch`] |
//!
//! `CreateWakeupTask`（99）**不在本文件**：那是 `agent_task_queue` 的写入面，
//! 归 M5-8（`jobs_issue_wakeup`）用 task 侧的仓储落库；本片只提供它需要的
//! `handoff_note` / `context.wakeup_evidence` 产物（见 `mc-autopilot::wakeup::service::plan_dispatch`）。
//!
//! # 锁顺序（契约，勿改）
//!
//! `workspace(FOR KEY SHARE) → issue(FOR NO KEY UPDATE) → issue_wakeup(FOR UPDATE)`，
//! 派发路径在最前面还要拿 `lock_task_owner_rows(agent, issue, runtime)`（上游 `dispatch`）。
//! 顺序与 `chat_task::send` 一致，两路因此不会死锁。
//!
//! # `revision` 只由两条 UPDATE 递增
//!
//! [`replace`]（upsert / enable：`revision = revision + 1` + `enabled=true` + 清 `last_*`）与
//! [`edit_instruction`]（**不**动 revision，只改正文）——上游把这两件事分开是为了让改文案不影响
//! 已捕获的事件与已排队的 run。

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    map_wakeup_err, map_wakeup_write_err, new_id, IssueWakeupView, WakeupRepoError, WakeupRow,
};
use crate::{RepoError, Result};

/// 上游 `ListReadyWakeups` 的批大小（`LIMIT 100`）。
pub const READY_WAKEUPS_BATCH: i64 = 100;

/// `CreateIssueWakeup` 的 18 个参数（`id` 由 [`create`] 现场生成）。
#[derive(Debug, Clone)]
pub struct NewWakeup {
    /// 所属 workspace。
    pub workspace_id: Uuid,
    /// 所属 issue。
    pub issue_id: Uuid,
    /// 被唤醒的 agent。
    pub agent_id: Uuid,
    /// 注册者（人类成员）。
    pub created_by: Uuid,
    /// 注册来源 task（自触发抑制）。
    pub source_task_id: Option<Uuid>,
    /// 注册来源评论。
    pub parent_comment_id: Option<Uuid>,
    /// 指令正文。
    pub instruction: String,
    /// `event | at | every | cron`。
    pub kind: String,
    /// `once | continuous`。
    pub mode: String,
    /// 订阅事件集合。
    pub event_types: Vec<String>,
    /// agent 过滤。
    pub filter_agent_id: Option<Uuid>,
    /// task 过滤。
    pub filter_task_id: Option<Uuid>,
    /// 主体过滤类型。
    pub filter_actor_type: Option<String>,
    /// 主体过滤 id。
    pub filter_actor_id: Option<Uuid>,
    /// `every` 间隔秒。
    pub interval_seconds: Option<i64>,
    /// `cron` 表达式。
    pub cron_expression: Option<String>,
    /// 时区。
    pub timezone: String,
    /// 首次触发时间。
    pub next_fire_at: Option<DateTime<Utc>>,
}

/// `LockWakeupIssue` 的最小投影（上游返回整行，本仓只读 4 列）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WakeupIssueRow {
    /// issue 主键。
    pub id: Uuid,
    /// 所属 workspace。
    pub workspace_id: Uuid,
    /// 当前 status key。
    pub status: String,
    /// 优先级（建 task 时映射成 int）。
    pub priority: String,
}

/// `LockWakeupSourceTask` 的最小投影（供注册时的终端事件快照与过滤校验用）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WakeupSourceTaskRow {
    /// task 主键。
    pub id: Uuid,
    /// 该 task 的 agent。
    pub agent_id: Uuid,
    /// 状态。
    pub status: String,
    /// 完成时间。
    pub completed_at: Option<DateTime<Utc>>,
    /// 重试链来源。
    pub retry_of_task_id: Option<Uuid>,
    /// 重跑来源。
    pub rerun_of_task_id: Option<Uuid>,
}

/// `FindPendingWakeupTask` 的行（M5-8 用它决定「合并进已有 task」还是「新建」）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WakeupTaskRow {
    /// task 主键。
    pub id: Uuid,
    /// 状态（`queued` / `dispatched`）。
    pub status: String,
    /// 所属 issue。
    pub issue_id: Uuid,
    /// 目标 agent。
    pub agent_id: Uuid,
    /// 目标 runtime。
    pub runtime_id: Option<Uuid>,
    /// 派发时间（`dispatched` 等待判定的起点）。
    pub dispatched_at: Option<DateTime<Utc>>,
    /// prepare 租约到期时间。
    pub prepare_lease_expires_at: Option<DateTime<Utc>>,
    /// 任务上下文（`wakeup_id` / `wakeup_revision` / `wakeup_evidence` 都在这里）。
    pub context: Option<JsonValue>,
    /// 注册者（`CheckClaim` 要它与 `issue_wakeup.created_by` 相等）。
    pub originator_user_id: Option<Uuid>,
    /// handoff note（合并证据时覆写）。
    pub handoff_note: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// 读（`&PgPool`）
// ---------------------------------------------------------------------------

/// `GetIssueWakeup`：按 (`id`, `workspace_id`) 读一行（handler 的 404 判定）。
pub async fn get_in_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<Option<WakeupRow>> {
    sqlx::query_as::<_, WakeupRow>("SELECT * FROM issue_wakeup WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(workspace_id)
        .fetch_optional(pool)
        .await
        .map_err(map_wakeup_err)
}

/// `LocklessWakeup`：不加锁读一行（`CheckClaim` 用；派发路径**不**用它）。
pub async fn lockless(pool: &PgPool, id: Uuid) -> Result<Option<WakeupRow>> {
    sqlx::query_as::<_, WakeupRow>("SELECT * FROM issue_wakeup WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(map_wakeup_err)
}

/// `ListIssueWakeups`：issue 级列表，含掩码后的过滤字段与展示名。
///
/// `agent_ids` 是调用者**可见**的 agent 集合（上游 `accessibleAgentIDs`）；空集合表示
/// 「除了公开信息一律掩码」，与上游传空数组时等价（`= ANY('{}')` 恒 false）。
pub async fn list_issue_wakeups(
    pool: &PgPool,
    workspace_id: Uuid,
    issue_id: Uuid,
    agent_ids: &[Uuid],
) -> Result<Vec<IssueWakeupView>> {
    sqlx::query_as::<_, IssueWakeupView>(
        "SELECT w.id,w.workspace_id,w.issue_id,w.agent_id,w.created_by,w.source_task_id,w.parent_comment_id,w.instruction, \
                w.kind,w.mode,w.event_types,w.filter_actor_type, \
                (CASE WHEN actor_agent.id IS NOT NULL OR actor_member.user_id IS NOT NULL THEN w.filter_actor_id END)::uuid AS filter_actor_id, \
                COALESCE(actor_agent.name,actor_user.name,'')::text AS filter_actor_name, \
                (CASE WHEN source.id IS NOT NULL THEN w.filter_agent_id END)::uuid AS filter_agent_id, \
                (CASE WHEN EXISTS(SELECT 1 FROM agent_task_queue ft JOIN agent fa ON fa.id=ft.agent_id AND fa.workspace_id=w.workspace_id \
                 WHERE ft.id=w.filter_task_id AND ft.issue_id=w.issue_id AND fa.id=ANY($3::uuid[])) THEN w.filter_task_id END)::uuid AS filter_task_id, \
                w.interval_seconds,w.cron_expression,w.timezone,w.next_fire_at,w.enabled,w.disabled_at,w.revision, \
                w.last_task_id,w.last_error,w.created_at,w.updated_at,a.name AS agent_name,source.name AS filter_agent_name,t.status AS last_task_status \
         FROM issue_wakeup w JOIN agent a ON a.id=w.agent_id AND a.workspace_id=w.workspace_id \
         LEFT JOIN agent actor_agent ON w.filter_actor_type='agent' AND actor_agent.id=w.filter_actor_id AND actor_agent.workspace_id=w.workspace_id AND actor_agent.id=ANY($3::uuid[]) \
         LEFT JOIN member actor_member ON w.filter_actor_type='member' AND actor_member.user_id=w.filter_actor_id AND actor_member.workspace_id=w.workspace_id \
         LEFT JOIN \"user\" actor_user ON actor_user.id=actor_member.user_id \
         LEFT JOIN agent source ON source.id=w.filter_agent_id AND source.workspace_id=w.workspace_id AND source.id=ANY($3::uuid[]) \
         LEFT JOIN agent_task_queue t ON t.id=w.last_task_id AND t.issue_id=w.issue_id AND t.agent_id=w.agent_id \
         WHERE w.workspace_id=$1 AND w.issue_id=$2 ORDER BY w.created_at,w.id",
    )
    .bind(workspace_id)
    .bind(issue_id)
    .bind(agent_ids)
    .fetch_all(pool)
    .await
    .map_err(map_wakeup_err)
}

/// `ListReadyWakeups`：到点的定时规则 ∪ 有待处理收据的规则（**含**事件驱动的）。
pub async fn ready_wakeups(pool: &PgPool) -> Result<Vec<WakeupRow>> {
    sqlx::query_as::<_, WakeupRow>(
        "WITH candidates AS ( \
             SELECT id FROM issue_wakeup WHERE enabled AND kind<>'event' AND next_fire_at<=now() \
             UNION \
             SELECT wakeup_id FROM issue_wakeup_receipt WHERE processed_at IS NULL \
         ) \
         SELECT w.* FROM candidates c JOIN issue_wakeup w ON w.id=c.id \
         ORDER BY w.updated_at,w.id LIMIT $1",
    )
    .bind(READY_WAKEUPS_BATCH)
    .fetch_all(pool)
    .await
    .map_err(map_wakeup_err)
}

/// `FindPendingWakeupTask`：该 wakeup 的待处理 task（`queued` / `dispatched`，取最早一条）。
pub async fn find_pending_wakeup_task(
    pool: &PgPool,
    wakeup_id: Uuid,
) -> Result<Option<WakeupTaskRow>> {
    sqlx::query_as::<_, WakeupTaskRow>(
        "SELECT id, status, issue_id, agent_id, runtime_id, dispatched_at, prepare_lease_expires_at, \
                context, originator_user_id, handoff_note, created_at \
         FROM agent_task_queue \
         WHERE context->>'wakeup_id' = $1 AND status IN ('queued','dispatched') \
         ORDER BY created_at LIMIT 1",
    )
    .bind(wakeup_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(map_wakeup_err)
}

/// `once` 重新武装前的活跃 run 判定（上游 `save` 的 enable 分支内联 SQL）：
/// `status IN ('queued','deferred','dispatched','running','waiting_local_directory')`。
pub async fn active_run_exists(pool: &PgPool, issue_id: Uuid, wakeup_id: Uuid) -> Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM agent_task_queue WHERE issue_id=$1 AND context->>'wakeup_id'=$2 \
         AND status IN ('queued','deferred','dispatched','running','waiting_local_directory'))",
    )
    .bind(issue_id)
    .bind(wakeup_id.to_string())
    .fetch_one(pool)
    .await
    .map_err(map_wakeup_err)
}

/// 上游 `wakeupIssueActive` = `issuestatus.CategoryWithError(...)` ∈ {open}。
///
/// 规范 key 直接由 `mc_core::status` 判定；自定义 key 查 `issue_status` 表。
/// **找不到 key** ⇒ [`RepoError::NotFound`]（上游同样把未知 status 当错误，不是「视为活跃」）。
pub async fn issue_is_active(pool: &PgPool, workspace_id: Uuid, status: &str) -> Result<bool> {
    if let Some(known) = mc_core::status::IssueStatus::from_key(status) {
        return Ok(known.category() == mc_core::status::StatusCategory::Open);
    }
    let row: Option<(String,)> =
        sqlx::query_as("SELECT category FROM issue_status WHERE workspace_id=$1 AND key=$2")
            .bind(workspace_id)
            .bind(status)
            .fetch_optional(pool)
            .await
            .map_err(map_wakeup_err)?;
    match row {
        Some((category,)) => Ok(category != "done" && category != "closed"),
        None => Err(RepoError::NotFound),
    }
}

// ---------------------------------------------------------------------------
// 锁 + 写（`&mut PgConnection`，调用方持有事务）
// ---------------------------------------------------------------------------

/// `SELECT w.id … FOR KEY SHARE OF w`：锁住 issue 背后的 workspace 行。
///
/// 与 workspace 删除路径的 `FOR UPDATE` 冲突 ⇒ 删除进行中时这里会等（或超时）。
pub async fn lock_workspace_for_issue(conn: &mut PgConnection, issue_id: Uuid) -> Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT w.id FROM workspace w JOIN issue i ON i.workspace_id=w.id WHERE i.id=$1 FOR KEY SHARE OF w",
    )
    .bind(issue_id)
    .fetch_one(conn)
    .await
    .map_err(map_wakeup_err)
}

/// `LockWakeupIssue`。
pub async fn lock_issue(conn: &mut PgConnection, issue_id: Uuid) -> Result<WakeupIssueRow> {
    sqlx::query_as::<_, WakeupIssueRow>(
        "SELECT id, workspace_id, status, priority FROM issue WHERE id=$1 FOR NO KEY UPDATE",
    )
    .bind(issue_id)
    .fetch_one(conn)
    .await
    .map_err(map_wakeup_err)
}

/// `LockIssueWakeup`。
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<WakeupRow> {
    sqlx::query_as::<_, WakeupRow>("SELECT * FROM issue_wakeup WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(conn)
        .await
        .map_err(map_wakeup_err)
}

/// `LockWakeupSourceTask`：`FOR UPDATE NOWAIT` ⇒ 拿不到锁报 `55P03`（上层映射 409）。
pub async fn lock_source_task(
    conn: &mut PgConnection,
    task_id: Uuid,
    issue_id: Uuid,
) -> std::result::Result<Option<WakeupSourceTaskRow>, WakeupRepoError> {
    sqlx::query_as::<_, WakeupSourceTaskRow>(
        "SELECT id, agent_id, status, completed_at, retry_of_task_id, rerun_of_task_id \
         FROM agent_task_queue WHERE id=$1 AND issue_id=$2 FOR UPDATE NOWAIT",
    )
    .bind(task_id)
    .bind(issue_id)
    .fetch_optional(conn)
    .await
    .map_err(map_wakeup_write_err)
}

/// `CreateIssueWakeup`：`id` 用 UUIDv7（与上游 `dbid.NewV7()` 同口径）。
///
/// 唯一会抛 `23514 + issue_wakeup_active_limit` 的写路径（`530` 的容量触发器）⇒ 专用错误类型。
pub async fn create(
    conn: &mut PgConnection,
    new: &NewWakeup,
) -> std::result::Result<WakeupRow, WakeupRepoError> {
    sqlx::query_as::<_, WakeupRow>(
        "INSERT INTO issue_wakeup(id,workspace_id,issue_id,agent_id,created_by,source_task_id,parent_comment_id,instruction,kind,mode,event_types,filter_agent_id,filter_task_id,filter_actor_type,filter_actor_id,interval_seconds,cron_expression,timezone,next_fire_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19) RETURNING *",
    )
    .bind(new_id())
    .bind(new.workspace_id)
    .bind(new.issue_id)
    .bind(new.agent_id)
    .bind(new.created_by)
    .bind(new.source_task_id)
    .bind(new.parent_comment_id)
    .bind(&new.instruction)
    .bind(&new.kind)
    .bind(&new.mode)
    .bind(&new.event_types)
    .bind(new.filter_agent_id)
    .bind(new.filter_task_id)
    .bind(&new.filter_actor_type)
    .bind(new.filter_actor_id)
    .bind(new.interval_seconds)
    .bind(&new.cron_expression)
    .bind(&new.timezone)
    .bind(new.next_fire_at)
    .fetch_one(conn)
    .await
    .map_err(map_wakeup_write_err)
}

/// 上游 `save` 的 upsert UPDATE（`wakeup.sql` 里没有独立 query 名：它内联在 Go 里）。
///
/// 语义逐字保留：`enabled=true` + 清 `disabled_at` + `revision=revision+1` +
/// **清 `last_task_id`/`last_error`**（重订阅后不再显示上一次的派发痕迹）。
pub async fn replace(
    conn: &mut PgConnection,
    id: Uuid,
    new: &NewWakeup,
) -> std::result::Result<WakeupRow, WakeupRepoError> {
    sqlx::query(
        "UPDATE issue_wakeup SET agent_id=$2,created_by=$3,source_task_id=$4,parent_comment_id=$5,instruction=$6,kind=$7,mode=$8,event_types=$9,filter_agent_id=$10,filter_task_id=$11,interval_seconds=$12,cron_expression=$13,timezone=$14,next_fire_at=$15,filter_actor_type=$16,filter_actor_id=$17,enabled=true,disabled_at=NULL,revision=revision+1,last_task_id=NULL,last_error=NULL,updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .bind(new.agent_id)
    .bind(new.created_by)
    .bind(new.source_task_id)
    .bind(new.parent_comment_id)
    .bind(&new.instruction)
    .bind(&new.kind)
    .bind(&new.mode)
    .bind(&new.event_types)
    .bind(new.filter_agent_id)
    .bind(new.filter_task_id)
    .bind(new.interval_seconds)
    .bind(&new.cron_expression)
    .bind(&new.timezone)
    .bind(new.next_fire_at)
    .bind(&new.filter_actor_type)
    .bind(new.filter_actor_id)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_write_err)?;
    lock(conn, id).await.map_err(WakeupRepoError::from)
}

/// `EditInstruction` 的 UPDATE：**不 bump revision**，`workspace_id` 也进 WHERE（上游口径）。
pub async fn edit_instruction(
    conn: &mut PgConnection,
    id: Uuid,
    workspace_id: Uuid,
    instruction: &str,
) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup SET instruction=$2,updated_at=now() WHERE id=$1 AND workspace_id=$3",
    )
    .bind(id)
    .bind(instruction)
    .bind(workspace_id)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// `Disable` 的 UPDATE（`disabled_at` 只补空，保留最早停用时间）。
pub async fn disable(conn: &mut PgConnection, id: Uuid) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup SET enabled=false,disabled_at=COALESCE(disabled_at,now()),updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// 派发路径的自动停用（上游 `dispatch` 内联 SQL）：带 `last_error` 原因。
pub async fn disable_with_reason(conn: &mut PgConnection, id: Uuid, reason: &str) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup SET enabled=false,disabled_at=COALESCE(disabled_at,now()),last_error=$2 WHERE id=$1",
    )
    .bind(id)
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// `DisableIssueWakeups`（issue 关闭时批量停用；重开**不**自动重启）。
pub async fn disable_issue_wakeups(conn: &mut PgConnection, issue_id: Uuid) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup SET enabled=false,disabled_at=clock_timestamp(),updated_at=clock_timestamp() \
         WHERE issue_id=$1 AND disabled_at IS NULL",
    )
    .bind(issue_id)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// `CancelUnstartedWakeupTasks`：只取消**未启动**的 wakeup run，返回被取消的行。
pub async fn cancel_unstarted_wakeup_tasks(
    conn: &mut PgConnection,
    wakeup_id: Uuid,
) -> Result<Vec<WakeupTaskRow>> {
    sqlx::query_as::<_, WakeupTaskRow>(
        "UPDATE agent_task_queue SET status='cancelled',completed_at=now(),error='Wakeup disabled or updated' \
         WHERE context->>'wakeup_id'=$1 AND status IN ('queued','deferred') AND started_at IS NULL \
         RETURNING id, status, issue_id, agent_id, runtime_id, dispatched_at, prepare_lease_expires_at, context, originator_user_id, handoff_note, created_at",
    )
    .bind(wakeup_id.to_string())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_wakeup_err)
}

/// `CancelUnstartedIssueWakeupTasks`：issue 关闭时的批量取消。
pub async fn cancel_unstarted_issue_wakeup_tasks(
    conn: &mut PgConnection,
    issue_id: Uuid,
) -> Result<Vec<WakeupTaskRow>> {
    sqlx::query_as::<_, WakeupTaskRow>(
        "UPDATE agent_task_queue SET status='cancelled',completed_at=now(),error='Issue closed; wakeup disabled' \
         WHERE issue_id=$1 AND context->>'wakeup_id' IS NOT NULL AND status IN ('queued','deferred') AND started_at IS NULL \
         RETURNING id, status, issue_id, agent_id, runtime_id, dispatched_at, prepare_lease_expires_at, context, originator_user_id, handoff_note, created_at",
    )
    .bind(issue_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_wakeup_err)
}

/// `AdvanceIssueWakeup`：`last_task_id` 用 `COALESCE`（不传就保留旧值），
/// `next_fire_at` / `last_error` 是**显式覆盖**（传 `None` 就写成 NULL）。
pub async fn advance(
    conn: &mut PgConnection,
    id: Uuid,
    enabled: bool,
    next_fire_at: Option<DateTime<Utc>>,
    last_task_id: Option<Uuid>,
    last_error: Option<&str>,
) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup SET enabled=$2,next_fire_at=$3,last_task_id=COALESCE($4,last_task_id),last_error=$5,updated_at=clock_timestamp() WHERE id=$1",
    )
    .bind(id)
    .bind(enabled)
    .bind(next_fire_at)
    .bind(last_task_id)
    .bind(last_error)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// `NoteWakeupFailure`（只写 `last_error`，不动 `enabled`）。
pub async fn note_failure(conn: &mut PgConnection, id: Uuid, message: &str) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup SET last_error=$2,updated_at=clock_timestamp() WHERE id=$1",
    )
    .bind(id)
    .bind(message)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// `TouchWakeupDispatch`：把卡住的配置挪到扫描批次的队尾。
pub async fn touch_dispatch(conn: &mut PgConnection, id: Uuid) -> Result<u64> {
    let done = sqlx::query("UPDATE issue_wakeup SET updated_at=clock_timestamp() WHERE id=$1")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}
