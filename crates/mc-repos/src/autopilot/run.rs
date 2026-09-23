//! `autopilot_run` 的写边，以及派发路径需要的几张邻表 SQL（M5-4，`docs/44` §3.2 的 W 面）。
//!
//! # 为什么这个文件里不只有 `autopilot_run`
//!
//! 上游 `dispatchCreateIssue` 把「分配编号 → 建 issue → 链接 run → 订阅者扇出 → 建 task」
//! 放在**同一个事务**里（`internal/service/autopilot.go:681-900`）。本仓的 repo 层惯例是
//! 「SQL 在 `mc-repos`，事务编排在服务层」，而这条链的每一步都必须在**调用方的 tx** 上执行
//! ⇒ 这里提供的是**收 `&mut PgConnection` 的写函数**，由 `mc-autopilot::dispatch` 打开事务
//! 串起来。`issue` 的插入因此没有走 [`crate::issue::IssueRepo::create`]（它自己拿 pool，
//! 无法加入我的 tx），而是照上游 `CreateIssueWithOrigin` 的形状单写一条。
//!
//! # 本地与上游的两处形状差异（都不是「顺手改一下」，是必须双写）
//!
//! 1. `issue` 的来源有**两套表示**：上游 `origin_type` / `origin_id`（`042_autopilot`），
//!    以及本仓 local-only 的 `issue.origin`（`537_local_only_columns`，无 CHECK，语义自成一系）。
//!    派发建出来的 issue **两套都写**：`origin_type='autopilot'` + `origin_id=<autopilot>` 是
//!    重复守卫与 `SyncRunFromIssue` 读的那套（上游契约），`origin='autopilot_run'` 是本地读面
//!    （issue 列表 / 详情）显示来源用的那套。只写一套就会有一侧看不见来源。
//! 2. 编号分配：上游走 `workspace` 计数器（`IncrementIssueCounter`），本仓没有这张计数器表，
//!    `IssueRepo` 用的是 `MAX(number)+1` + `UNIQUE(workspace_id, number)` 冲突重试
//!    （`NUMBER_ALLOC_RETRIES`）。这里照抄**本仓**的口径（在 tx 内算 `MAX+1`，冲突则整个
//!    派发重试），因为编号唯一性由同一个唯一索引保证。
//!
//! # 归属（attribution）
//!
//! `CreateAutopilotTask` 的 `originator_*` / `accountable_*` / `rule_version_id` 三件套由
//! `mc-autopilot::dispatch` 判定后传进来；本文件只负责绑参。见 `docs/44` §8 的偏差登记：
//! 本仓**没有** `autopilot_rule_version` 的发布链（MUL-4302 §3.4），因此 `rule_version_id`
//! 恒为 `NULL`，`originator_source` 取值 `direct_human` / `trigger_owner` / `rule_owner` /
//! `unattributed`。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::Result;

/// `autopilot_run` 的 18 列（与 `mc_core::autopilot::AutopilotRun` 一一对应，`SELECT *` 序）。
pub(crate) const AUTOPILOT_RUN_COLUMNS: &str = "id, autopilot_id, trigger_id, status, source, \
     issue_id, squad_id, task_id, quota_reservation_id, webhook_delivery_id, trigger_payload, \
     planned_at, triggered_at, completed_at, result, failure_reason, reason_code, created_at";

/// 带 `r.` 限定的同一份列清单（`get_in_workspace` 里 `JOIN autopilot` ⇒ 列必须限定）。
const AUTOPILOT_RUN_COLUMNS_QUALIFIED: &str = "r.id, r.autopilot_id, r.trigger_id, r.status, \
     r.source, r.issue_id, r.squad_id, r.task_id, r.quota_reservation_id, \
     r.webhook_delivery_id, r.trigger_payload, r.planned_at, r.triggered_at, r.completed_at, \
     r.result, r.failure_reason, r.reason_code, r.created_at";

/// `run_only` 任务完成后要写回 run 的 `result`（上游 `task.Result`）。
pub const RUN_STATUS_ISSUE_CREATED: &str = "issue_created";
/// 见 [`RUN_STATUS_ISSUE_CREATED`]。
pub const RUN_STATUS_RUNNING: &str = "running";
/// 见 [`RUN_STATUS_ISSUE_CREATED`]。
pub const RUN_STATUS_COMPLETED: &str = "completed";
/// 见 [`RUN_STATUS_ISSUE_CREATED`]。
pub const RUN_STATUS_FAILED: &str = "failed";
/// 见 [`RUN_STATUS_ISSUE_CREATED`]。
pub const RUN_STATUS_SKIPPED: &str = "skipped";

/// 「还在飞」的 run status（重复守卫与 `SyncRunFrom*` 用的集合）。
///
/// 与上游 `GetAutopilotRunByIssue` 的 `status IN ('issue_created','running')` 一致：`pending`
/// 在本地 CHECK 里已被 `043` 删掉、`079` 只把 `skipped` 加回来，所以这里就是两个。
pub const ACTIVE_RUN_STATUSES: &str = "'issue_created', 'running'";

/// `autopilot_run` 行（18 列，裸 `Uuid` / `Option<...>`，与 `mod.rs` 的行结构同惯例）。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotRunRow {
    pub id: Uuid,
    pub autopilot_id: Uuid,
    pub trigger_id: Option<Uuid>,
    pub status: String,
    pub source: String,
    pub issue_id: Option<Uuid>,
    pub squad_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub quota_reservation_id: Option<Uuid>,
    pub webhook_delivery_id: Option<Uuid>,
    pub trigger_payload: Option<Value>,
    pub planned_at: Option<DateTime<Utc>>,
    pub triggered_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<Value>,
    pub failure_reason: Option<String>,
    pub reason_code: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl AutopilotRunRow {
    /// 终态（`completed` / `failed` / `skipped`）。
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed" | "skipped")
    }
}

/// `CreateAutopilotRun` 的入参（`id` 由调用方生成 —— 与上游 `dbid.NewV7()` 同位）。
#[derive(Debug, Clone)]
pub struct NewAutopilotRun {
    pub id: Uuid,
    pub autopilot_id: Uuid,
    pub trigger_id: Option<Uuid>,
    /// `schedule` / `manual` / `webhook` / `api`（`autopilot_run.source` 的 CHECK）。
    pub source: String,
    /// `issue_created` / `running` / `completed` / `failed` / `skipped`。
    ///
    /// `pending` **不在此列**：迁移 `043` 把 CHECK 收窄成 `(issue_created, running,
    /// completed, failed)`、`079` 只把 `skipped` 加回来（`043` 的注释就是「修孤儿 pending
    /// 行」，而不是保留它）。所以新 run 只有两种起始态：create_issue 线 `issue_created`、
    /// run_only 线 `running`。
    pub status: String,
    pub trigger_payload: Option<Value>,
    pub squad_id: Option<Uuid>,
    pub planned_at: Option<DateTime<Utc>>,
    pub webhook_delivery_id: Option<Uuid>,
    pub quota_reservation_id: Option<Uuid>,
    pub reason_code: Option<String>,
}

/// `CreateAutopilotRun`：插一行 run。
pub async fn create_run(
    conn: &mut PgConnection,
    new: &NewAutopilotRun,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "INSERT INTO autopilot_run (id, autopilot_id, trigger_id, source, status, \
             trigger_payload, squad_id, planned_at, webhook_delivery_id, quota_reservation_id, \
             reason_code) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         RETURNING {AUTOPILOT_RUN_COLUMNS}"
    ))
    .bind(new.id)
    .bind(new.autopilot_id)
    .bind(new.trigger_id)
    .bind(new.source.as_str())
    .bind(new.status.as_str())
    .bind(new.trigger_payload.clone())
    .bind(new.squad_id)
    .bind(new.planned_at)
    .bind(new.webhook_delivery_id)
    .bind(new.quota_reservation_id)
    .bind(new.reason_code.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `GetAutopilotRunByTriggerAndPlanned`：调度线幂等快路径（`uq_autopilot_run_trigger_planned`）。
pub async fn find_by_trigger_and_planned(
    conn: &mut PgConnection,
    trigger_id: Uuid,
    planned_at: DateTime<Utc>,
) -> Result<Option<AutopilotRunRow>> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "SELECT {AUTOPILOT_RUN_COLUMNS} FROM autopilot_run \
         WHERE trigger_id = $1 AND planned_at = $2 LIMIT 1"
    ))
    .bind(trigger_id)
    .bind(planned_at)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `GetAutopilotRunByWebhookDelivery`（webhook 线幂等）。
pub async fn find_by_webhook_delivery(
    conn: &mut PgConnection,
    delivery_id: Uuid,
) -> Result<Option<AutopilotRunRow>> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "SELECT {AUTOPILOT_RUN_COLUMNS} FROM autopilot_run WHERE webhook_delivery_id = $1 LIMIT 1"
    ))
    .bind(delivery_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `GetAutopilotRunByQuotaReservation`（quota 线幂等：同 `Idempotency-Key` 复用同一个 run）。
pub async fn find_by_quota_reservation(
    conn: &mut PgConnection,
    reservation_id: Uuid,
) -> Result<Option<AutopilotRunRow>> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "SELECT {AUTOPILOT_RUN_COLUMNS} FROM autopilot_run \
         WHERE quota_reservation_id = $1 LIMIT 1"
    ))
    .bind(reservation_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `RecoverPartialAutopilotRun`：把「写了 run 但 downstream 没建出来」的半成品标成 failed，
/// 并**清空 `planned_at`**（腾出 `uq_autopilot_run_trigger_planned` 那个槽位）同时释放预留。
///
/// 返回 `true` = 确实回收了（调用方据此走「全新 run」路径）。
pub async fn recover_partial_run(conn: &mut PgConnection, run_id: Uuid) -> Result<bool> {
    let recovered: i64 = sqlx::query_scalar(
        "WITH updated_run AS ( \
             UPDATE autopilot_run AS ar \
             SET status = 'failed', completed_at = now(), \
                 failure_reason = 'recovered partial dispatch (crashed before downstream creation)', \
                 reason_code = 'internal_error', planned_at = NULL \
             WHERE ar.id = $1 \
               AND (ar.status = 'pending' \
                    OR (ar.status = 'issue_created' AND ar.issue_id IS NULL) \
                    OR (ar.status = 'running' AND ar.task_id IS NULL)) \
               AND NOT EXISTS (SELECT 1 FROM agent_task_queue task \
                               WHERE task.autopilot_run_id = ar.id) \
             RETURNING ar.quota_reservation_id \
         ), locked_reservation AS MATERIALIZED ( \
             SELECT qr.id FROM autopilot_quota_reservation qr \
             JOIN updated_run ar ON ar.quota_reservation_id = qr.id \
             WHERE qr.state = 'reserved' FOR UPDATE \
         ), released_reservation AS ( \
             UPDATE autopilot_quota_reservation AS qr \
             SET state = 'released', finalized_at = now() \
             FROM locked_reservation AS locked WHERE qr.id = locked.id \
             RETURNING qr.workspace_id, qr.period_start, qr.period_end \
         ), settled_period AS ( \
             UPDATE autopilot_quota_period AS p \
             SET reserved_count = reserved_count - 1, updated_at = now() \
             FROM released_reservation AS released \
             WHERE p.workspace_id = released.workspace_id \
               AND p.period_start = released.period_start \
               AND p.period_end = released.period_end \
             RETURNING p.workspace_id \
         ) \
         SELECT count(*)::bigint FROM updated_run",
    )
    .bind(run_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(recovered > 0)
}

/// `GetAutopilotRun`（无 workspace 限定；仅内部路径用，读面用 [`get_in_workspace`]）。
pub async fn get(pool: &PgPool, run_id: Uuid) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "SELECT {AUTOPILOT_RUN_COLUMNS} FROM autopilot_run WHERE id = $1"
    ))
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_err)
}

/// 读面：run + workspace 双重限定（跨 workspace 的 runId 一律 `NotFound`）。
pub async fn get_in_workspace(
    pool: &PgPool,
    run_id: Uuid,
    workspace_id: Uuid,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "SELECT {AUTOPILOT_RUN_COLUMNS_QUALIFIED} FROM autopilot_run r \
         JOIN autopilot a ON a.id = r.autopilot_id \
         WHERE r.id = $1 AND a.workspace_id = $2"
    ))
    .bind(run_id)
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `ListAutopilotRuns`：newest first，`limit` / `offset` 由调用方（handler）收口。
pub async fn list(
    pool: &PgPool,
    autopilot_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<AutopilotRunRow>> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "SELECT {AUTOPILOT_RUN_COLUMNS} FROM autopilot_run \
         WHERE autopilot_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(autopilot_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateAutopilotRunIssueCreated`：create_issue 线把 issue 链进 run（同 tx）。
pub async fn update_issue_created(
    conn: &mut PgConnection,
    run_id: Uuid,
    issue_id: Uuid,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "UPDATE autopilot_run SET status = 'issue_created', issue_id = $2 \
         WHERE id = $1 RETURNING {AUTOPILOT_RUN_COLUMNS}"
    ))
    .bind(run_id)
    .bind(issue_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateAutopilotRunRunning`：run_only 线把 task 链进 run。
pub async fn update_running(
    conn: &mut PgConnection,
    run_id: Uuid,
    task_id: Uuid,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "UPDATE autopilot_run SET status = 'running', task_id = $2 \
         WHERE id = $1 RETURNING {AUTOPILOT_RUN_COLUMNS}"
    ))
    .bind(run_id)
    .bind(task_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateAutopilotRunCompleted`（**无预留**的旧路径；调度线终态走
/// [`update_terminal_with_quota`]，两者不可混用，注释见上游同名 query）。
pub async fn update_completed(
    conn: &mut PgConnection,
    run_id: Uuid,
    result: Option<&Value>,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "UPDATE autopilot_run SET status = 'completed', completed_at = now(), result = $2 \
         WHERE id = $1 RETURNING {AUTOPILOT_RUN_COLUMNS}"
    ))
    .bind(run_id)
    .bind(result.cloned())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateAutopilotRunFailed`（无预留旧路径）。
pub async fn update_failed(
    conn: &mut PgConnection,
    run_id: Uuid,
    failure_reason: Option<&str>,
    reason_code: Option<&str>,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "UPDATE autopilot_run SET status = 'failed', completed_at = now(), \
             failure_reason = $2, reason_code = $3 \
         WHERE id = $1 RETURNING {AUTOPILOT_RUN_COLUMNS}"
    ))
    .bind(run_id)
    .bind(failure_reason)
    .bind(reason_code)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateAutopilotRunSkipped`：**预准入**跳过（此时还没有预留，所以不走 quota CTE）。
pub async fn update_skipped(
    conn: &mut PgConnection,
    run_id: Uuid,
    failure_reason: Option<&str>,
    reason_code: Option<&str>,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "UPDATE autopilot_run SET status = 'skipped', completed_at = now(), \
             failure_reason = $2, reason_code = $3 \
         WHERE id = $1 RETURNING {AUTOPILOT_RUN_COLUMNS}"
    ))
    .bind(run_id)
    .bind(failure_reason)
    .bind(reason_code)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateAutopilotRunTerminalWithQuota`：终态 + 预留结算**一条语句**。
///
/// `consume = true` ⇒ 预留转 `consumed`（`used_count + 1`）；`false` ⇒ 转 `released`
/// （只减 `reserved_count`）。没有预留（quota 关掉时建的 run）时 quota CTE 自然为空，
/// 不需要额外的 `BEGIN`/`COMMIT` 往返。
///
/// `status` ∈ {`completed`, `failed`, `skipped`}：`result` 只在 `completed` 生效，
/// `failure_reason` / `reason_code` 只在 `failed` / `skipped` 生效（上游 CASE 逐字照抄，
/// 所以传错了不会**覆盖**另一侧的既有值）。
pub async fn update_terminal_with_quota(
    conn: &mut PgConnection,
    run_id: Uuid,
    status: &str,
    result: Option<&Value>,
    failure_reason: Option<&str>,
    reason_code: Option<&str>,
    consume: bool,
) -> Result<AutopilotRunRow> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "WITH updated_run AS ( \
             UPDATE autopilot_run AS ar \
             SET status = $2, completed_at = now(), \
                 result = CASE WHEN $2 = 'completed' THEN $3::jsonb ELSE ar.result END, \
                 failure_reason = CASE WHEN $2 IN ('failed', 'skipped') THEN $4::text \
                                       ELSE ar.failure_reason END, \
                 reason_code = CASE WHEN $2 IN ('failed', 'skipped') THEN $5::text \
                                    ELSE ar.reason_code END \
             WHERE ar.id = $1 RETURNING ar.* \
         ), locked_reservation AS MATERIALIZED ( \
             SELECT qr.id, qr.workspace_id, qr.period_start, qr.period_end \
             FROM autopilot_quota_reservation qr \
             JOIN updated_run ar ON ar.quota_reservation_id = qr.id \
             WHERE qr.state = 'reserved' FOR UPDATE \
         ), finalized_reservation AS ( \
             UPDATE autopilot_quota_reservation AS qr \
             SET state = CASE WHEN $6::boolean THEN 'consumed' ELSE 'released' END, \
                 finalized_at = now() \
             FROM locked_reservation AS locked WHERE qr.id = locked.id \
             RETURNING locked.workspace_id, locked.period_start, locked.period_end \
         ), settled_period AS ( \
             UPDATE autopilot_quota_period AS p \
             SET reserved_count = reserved_count - 1, \
                 used_count = used_count + CASE WHEN $6::boolean THEN 1 ELSE 0 END, \
                 updated_at = now() \
             FROM finalized_reservation AS finalized \
             WHERE p.workspace_id = finalized.workspace_id \
               AND p.period_start = finalized.period_start \
               AND p.period_end = finalized.period_end \
             RETURNING p.workspace_id \
         ) \
         SELECT {AUTOPILOT_RUN_COLUMNS} FROM updated_run"
    ))
    .bind(run_id)
    .bind(status)
    .bind(result.cloned())
    .bind(failure_reason)
    .bind(reason_code)
    .bind(consume)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `GetAutopilotRunByIssue`：仍在飞的 run（`SyncRunFromIssue` / 重复守卫用）。
pub async fn find_active_by_issue(
    conn: &mut PgConnection,
    issue_id: Uuid,
) -> Result<Option<AutopilotRunRow>> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "SELECT {AUTOPILOT_RUN_COLUMNS} FROM autopilot_run \
         WHERE issue_id = $1 AND status IN ({ACTIVE_RUN_STATUSES}) LIMIT 1"
    ))
    .bind(issue_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `FailAutopilotRunsByIssue`：issue 被删（`ON DELETE SET NULL` 之前）时把在飞的 run 判失败，
/// 并释放**仍为 reserved** 的预留（create_issue 的预留早在建 issue 时就 consume 了，不再退）。
pub async fn fail_by_issue(
    conn: &mut PgConnection,
    issue_id: Uuid,
) -> Result<Vec<AutopilotRunRow>> {
    sqlx::query_as::<_, AutopilotRunRow>(&format!(
        "WITH updated_runs AS ( \
             UPDATE autopilot_run \
             SET status = 'failed', completed_at = now(), \
                 failure_reason = 'linked issue was deleted' \
             WHERE issue_id = $1 AND status IN ({ACTIVE_RUN_STATUSES}) \
             RETURNING * \
         ), locked_reservations AS MATERIALIZED ( \
             SELECT qr.id, qr.workspace_id, qr.period_start, qr.period_end \
             FROM autopilot_quota_reservation qr \
             JOIN updated_runs ar ON ar.quota_reservation_id = qr.id \
             WHERE qr.state = 'reserved' FOR UPDATE \
         ), released_reservations AS ( \
             UPDATE autopilot_quota_reservation AS qr \
             SET state = 'released', finalized_at = now() \
             FROM locked_reservations AS locked WHERE qr.id = locked.id \
             RETURNING locked.workspace_id, locked.period_start, locked.period_end \
         ), released_by_period AS ( \
             SELECT workspace_id, period_start, period_end, count(*)::bigint AS released_count \
             FROM released_reservations GROUP BY workspace_id, period_start, period_end \
         ), settled_periods AS ( \
             UPDATE autopilot_quota_period AS p \
             SET reserved_count = reserved_count - released.released_count, updated_at = now() \
             FROM released_by_period AS released \
             WHERE p.workspace_id = released.workspace_id \
               AND p.period_start = released.period_start \
               AND p.period_end = released.period_end \
             RETURNING p.workspace_id \
         ) \
         SELECT {AUTOPILOT_RUN_COLUMNS} FROM updated_runs"
    ))
    .bind(issue_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

/// `HasActiveTaskForIssue`：该 issue 还有非终态任务（`SyncRunFromLinkedIssueTask` 的判据）。
///
/// 状态集合与上游逐字相同（多出的取值在本仓 CHECK 里可能不存在，字符串比较不受影响）。
pub async fn has_active_task_for_issue(conn: &mut PgConnection, issue_id: Uuid) -> Result<bool> {
    let has: bool = sqlx::query_scalar(
        "SELECT count(*) > 0 FROM agent_task_queue WHERE issue_id = $1 \
         AND status IN ('queued', 'dispatched', 'running', 'waiting_local_directory')",
    )
    .bind(issue_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(has)
}

/// `GetAutopilotTaskByRun`：修 `run_only` 的窄崩溃窗口（task 已提交但 `run.task_id` 没写上）。
pub async fn find_task_id_by_run(pool: &PgPool, run_id: Uuid) -> Result<Option<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM agent_task_queue WHERE autopilot_run_id = $1 \
         ORDER BY created_at LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateAutopilotLastRunAt`（跳过与派发两条线都会写）。
pub async fn update_autopilot_last_run_at(pool: &PgPool, autopilot_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE autopilot SET last_run_at = now(), updated_at = now() WHERE id = $1")
        .bind(autopilot_id)
        .execute(pool)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 任务入队（run_only 与 create_issue 两线共用）
// ---------------------------------------------------------------------------

/// `CreateAutopilotTask` 的入参。
///
/// `issue_id`：create_issue 线是新建的 issue（上游那条链路是 issue 监听器入队，本地没这条流，
/// 于是由派发自己建任务并同时链上 issue 与 run —— 见 `docs/44` §8 偏差）；run_only 线是 `None`
/// （上游该类任务是**纯 run 任务**，不挂 issue）。
#[derive(Debug, Clone)]
pub struct NewAutopilotTask {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub runtime_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub priority: i32,
    pub autopilot_run_id: Option<Uuid>,
    pub trigger_summary: Option<String>,
    pub originator_user_id: Option<Uuid>,
    pub accountable_user_id: Option<Uuid>,
    pub rule_version_id: Option<Uuid>,
    pub originator_source: Option<String>,
    pub trigger_evidence_kind: Option<String>,
    pub trigger_evidence_ref_id: Option<Uuid>,
}

/// `CreateAutopilotTask`：入队一条 autopilot 任务。
///
/// 归属栅栏 `lock_task_owner_rows(agent_id, issue_id, runtime_id)`（迁移 284）在**本语句自己的
/// WHERE 里**被调用 ⇒ workspace 正在拆除时写零行，返回 `None`（而不是把任务留在一个刚被删掉的
/// workspace 里）。这是全仓「写 agent_task_queue 归属必须过栅栏」的硬约束。
pub async fn create_task(conn: &mut PgConnection, new: &NewAutopilotTask) -> Result<Option<Uuid>> {
    sqlx::query_scalar(
        "INSERT INTO agent_task_queue ( \
             agent_id, runtime_id, issue_id, status, priority, autopilot_run_id, trigger_summary, \
             originator_user_id, accountable_user_id, rule_version_id, originator_source, \
             trigger_evidence_kind, trigger_evidence_ref_id, id \
         ) \
         SELECT $1, $2, $3, 'queued', $4, $5, $6, $7, $8, $9, $10, $11, $12, $13 \
         WHERE lock_task_owner_rows($1, $3, $2) \
         RETURNING id",
    )
    .bind(new.agent_id)
    .bind(new.runtime_id)
    .bind(new.issue_id)
    .bind(new.priority)
    .bind(new.autopilot_run_id)
    .bind(new.trigger_summary.as_deref())
    .bind(new.originator_user_id)
    .bind(new.accountable_user_id)
    .bind(new.rule_version_id)
    .bind(new.originator_source.as_deref())
    .bind(new.trigger_evidence_kind.as_deref())
    .bind(new.trigger_evidence_ref_id)
    .bind(new.id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_err)
}

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
             project_id, origin, origin_type, origin_id, last_activity_at, revision) \
         VALUES ($1, $2, $3, $4, $5, $6, 'todo', 'none', $7, $8, 'agent', $9, $10, \
             $11, $12, 'autopilot', $13, now(), 1) \
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

/// 派发用的 trigger 主体（归属判定：`manual` 之外的线从 trigger 的 `created_by` 取）。
///
/// 放在本文件是因为 `autopilot/trigger.rs` 不属本切片写集（`docs/44` §3.2），而这里只是
/// 一条只读查询。
pub async fn load_trigger_principal(
    pool: &PgPool,
    trigger_id: Uuid,
) -> Result<Option<(Option<String>, Option<Uuid>)>> {
    sqlx::query_as::<_, (Option<String>, Option<Uuid>)>(
        "SELECT created_by_type, created_by_id FROM autopilot_trigger WHERE id = $1",
    )
    .bind(trigger_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// 归一化标题（`issueguard.NormalizeTitle`：小写 + 折叠空白）。
///
/// 与 SQL 侧 `lower(btrim(regexp_replace(title, '[[:space:]]+', ' ', 'g')))` 必须给出同一个
/// 字符串，否则重复守卫会漏（纯函数放在这里，测试直接锁它）。
#[must_use]
pub fn normalize_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
