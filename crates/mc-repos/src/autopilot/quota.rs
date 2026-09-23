//! 配额仓储（`autopilot_quota_period` / `autopilot_quota_reservation`）。
//!
//! - **写者**：M5-1（**W**；`docs/44` §3.2）。M5-4 读（派发前保留配额）。
//! - **上游 SQL**：`db/queries/autopilot_quota.sql`148 / 10 查询。
//! - **两张表的键不一样**：`autopilot_quota_period` 的主键是
//!   `(workspace_id, period_start, period_end)`（**没有 `id`**，多种周期约定可共存）；
//!   `autopilot_quota_reservation` 的主键是 `id`。
//! - **幂等键**：`uq_autopilot_quota_reservation_key` =
//!   `(workspace_id, period_start, period_end, idempotency_key) WHERE state <> 'released'`
//!   ⇒ `released` 之后同一幂等键可再占用（**这是重试语义**）。
//! - **要落的入口**：保留（reserve）/ 记账（consume）/ 释放（release）+ **扫陈旧保留**
//!   （`idx_autopilot_quota_reservation_state` 是 `state='reserved'` 的部分索引，专为它建）。
//!   `448` 的 `rejection_notified_at` 是一次性通知标记（同一个周期只通知一次）。
//! - **不要发明限额**：限额与周期边界由 entitlement 平面在运行时下发（`352` 的迁移注释）
//!   ⇒ 本文件只读写事实，不写商业默认值。
//!
//! ## 本文件的形态：`PgExecutor` 自由函数
//!
//! 所有查询都是**自由函数**并接 `E: sqlx::PgExecutor<'c>`（照 `mc-repos::agent::env` 的
//! 既有写法）⇒ 同一个函数既能接连接池（单语句路径），也能接 `&mut *tx`（M5-4 的准入事务）。
//! `QuotaRepo` 只是池形态的薄包装 + 读面入口。
//!
//! ## 准入（admission）的**边界**
//!
//! 上游 `createAutopilotRunWithQuota` 在**同一个事务**里「占配额 + 建 run」；本文件只落
//! **配额那一半**（[`admit`]），因为 `autopilot_run` 的 insert 属 M5-4（`crate::autopilot::run`）。
//! 组合方式：`begin()` → `admit(&mut *tx, …)` → 建 run（把 `quota_reservation_id` 写进去）→
//! `commit()`。**不要**把 [`admit`] 接在池上再单独建 run：那样崩在中间会留下孤儿预留
//! （`autopilot_run.quota_reservation_id` 没有外键，正是 [`sweep_stale`] 存在的原因）。
//! run 终态时调 [`consume`]（成功路径）或 [`release`]（跳过/失败且从未落副作用）。

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb};

/// `autopilot_quota_period` 的列清单（9 列）。
pub(crate) const QUOTA_PERIOD_COLUMNS: &str = "workspace_id, period_start, period_end, \
     used_count, reserved_count, blocked_counts, created_at, updated_at, rejection_notified_at";

/// `autopilot_quota_reservation` 的列清单（11 列）。
pub(crate) const QUOTA_RESERVATION_COLUMNS: &str = "id, workspace_id, period_start, period_end, \
     policy_revision, subscription_version, source, idempotency_key, state, created_at, finalized_at";

/// `autopilot_quota_reservation.state` 的合法取值（`352` 的 CHECK）。
pub const STATE_RESERVED: &str = "reserved";
/// 见 [`STATE_RESERVED`]。
pub const STATE_CONSUMED: &str = "consumed";
/// 见 [`STATE_RESERVED`]。
pub const STATE_RELEASED: &str = "released";

// ---------------------------------------------------------------------------
// 行结构
// ---------------------------------------------------------------------------

/// `autopilot_quota_period` 行（9 列；**没有 `id`**）。
#[derive(Debug, Clone, FromRow)]
pub struct QuotaPeriodRow {
    pub workspace_id: Uuid,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub used_count: i64,
    pub reserved_count: i64,
    pub blocked_counts: JsonValue,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub rejection_notified_at: Option<DateTime<Utc>>,
}

impl QuotaPeriodRow {
    /// `used + reserved`（上游 `AutopilotQuotaUsage` 的 `total`）。
    pub fn total(&self) -> i64 {
        self.used_count + self.reserved_count
    }
}

/// `autopilot_quota_reservation` 行（11 列）。
#[derive(Debug, Clone, FromRow)]
pub struct QuotaReservationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub policy_revision: i64,
    pub subscription_version: i64,
    pub source: String,
    pub idempotency_key: String,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
}

impl QuotaReservationRow {
    /// 是否还是「占用中」的预留（只有这一态能被消费/释放）。
    pub fn is_reserved(&self) -> bool {
        self.state == STATE_RESERVED
    }

    /// 领域 id。
    pub fn domain_id(&self) -> Id {
        Id::from(self.id)
    }
}

// ---------------------------------------------------------------------------
// 查询面（自由函数：池 / 事务两用）
// ---------------------------------------------------------------------------

/// 上游 `EnsureAutopilotQuotaPeriod`：建行或**锁行**。
///
/// `ON CONFLICT DO UPDATE SET updated_at = updated_at` 是**故意的空更新** ——
/// 它让 `ON CONFLICT` 路径也拿到该行的写锁，从而把一个工作区/周期的准入串行化。
/// **不要**把它改成 `DO NOTHING`：那样两个并发准入会同时看到 `reserved_count` 的旧值。
pub async fn ensure_period<'c, E>(
    executor: E,
    workspace_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> Result<QuotaPeriodRow, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = "INSERT INTO autopilot_quota_period (workspace_id, period_start, period_end) \
               VALUES ($1, $2, $3) \
         ON CONFLICT (workspace_id, period_start, period_end) DO UPDATE \
                 SET updated_at = autopilot_quota_period.updated_at \
             RETURNING *";
    sqlx::query_as::<_, QuotaPeriodRow>(sql)
        .bind(workspace_id)
        .bind(period_start)
        .bind(period_end)
        .fetch_one(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `GetAutopilotQuotaPeriod`（用法读面：没有行 = 本周期还没用过额度）。
pub async fn get_period<'c, E>(
    executor: E,
    workspace_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> Result<Option<QuotaPeriodRow>, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = format!(
        "SELECT {QUOTA_PERIOD_COLUMNS} FROM autopilot_quota_period \
         WHERE workspace_id = $1 AND period_start = $2 AND period_end = $3"
    );
    sqlx::query_as::<_, QuotaPeriodRow>(&sql)
        .bind(workspace_id)
        .bind(period_start)
        .bind(period_end)
        .fetch_optional(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `GetAutopilotQuotaReservationByKey`：幂等命中查询（`state <> 'released'`）。
pub async fn get_reservation_by_key<'c, E>(
    executor: E,
    workspace_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    idempotency_key: &str,
) -> Result<Option<QuotaReservationRow>, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = format!(
        "SELECT {QUOTA_RESERVATION_COLUMNS} FROM autopilot_quota_reservation \
         WHERE workspace_id = $1 AND period_start = $2 AND period_end = $3 \
           AND idempotency_key = $4 AND state <> '{STATE_RELEASED}'"
    );
    sqlx::query_as::<_, QuotaReservationRow>(&sql)
        .bind(workspace_id)
        .bind(period_start)
        .bind(period_end)
        .bind(idempotency_key)
        .fetch_optional(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `CreateAutopilotQuotaReservation`。
#[allow(clippy::too_many_arguments)] // 与上游 sqlc 的 params struct 字段一一对应
pub async fn reserve<'c, E>(
    executor: E,
    workspace_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    policy_revision: i64,
    subscription_version: i64,
    source: &str,
    idempotency_key: &str,
) -> Result<QuotaReservationRow, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = format!(
        "INSERT INTO autopilot_quota_reservation (workspace_id, period_start, period_end, \
             policy_revision, subscription_version, source, idempotency_key) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {QUOTA_RESERVATION_COLUMNS}"
    );
    sqlx::query_as::<_, QuotaReservationRow>(&sql)
        .bind(workspace_id)
        .bind(period_start)
        .bind(period_end)
        .bind(policy_revision)
        .bind(subscription_version)
        .bind(source)
        .bind(idempotency_key)
        .fetch_one(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `IncrementAutopilotQuotaReserved`。
pub async fn increment_reserved<'c, E>(
    executor: E,
    workspace_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> Result<QuotaPeriodRow, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = "UPDATE autopilot_quota_period SET reserved_count = reserved_count + 1, \
               updated_at = now() \
               WHERE workspace_id = $1 AND period_start = $2 AND period_end = $3 RETURNING *";
    sqlx::query_as::<_, QuotaPeriodRow>(sql)
        .bind(workspace_id)
        .bind(period_start)
        .bind(period_end)
        .fetch_one(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `IncrementAutopilotQuotaBlocked`：按 `source` 累加拒绝计数（jsonb 内嵌 map）。
///
/// `source` 是**自由文本**（库里没有 CHECK），也是 `blocked_counts` 的 key ⇒ 用量响应里
/// 客户端看到的就是这几个 key。
pub async fn increment_blocked<'c, E>(
    executor: E,
    workspace_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    source: &str,
) -> Result<QuotaPeriodRow, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = "UPDATE autopilot_quota_period \
                  SET blocked_counts = jsonb_set( \
                          blocked_counts, ARRAY[$4::text], \
                          to_jsonb(COALESCE((blocked_counts ->> $4::text)::bigint, 0) + 1), true), \
                      updated_at = now() \
                WHERE workspace_id = $1 AND period_start = $2 AND period_end = $3 \
             RETURNING *";
    sqlx::query_as::<_, QuotaPeriodRow>(sql)
        .bind(workspace_id)
        .bind(period_start)
        .bind(period_end)
        .bind(source)
        .fetch_one(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `MarkAutopilotQuotaRejectionNotified`：一次性标记（`COALESCE` 保留首次时间）。
pub async fn mark_rejection_notified<'c, E>(
    executor: E,
    workspace_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> Result<QuotaPeriodRow, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = "UPDATE autopilot_quota_period \
                  SET rejection_notified_at = COALESCE(rejection_notified_at, now()), \
                      updated_at = now() \
                WHERE workspace_id = $1 AND period_start = $2 AND period_end = $3 RETURNING *";
    sqlx::query_as::<_, QuotaPeriodRow>(sql)
        .bind(workspace_id)
        .bind(period_start)
        .bind(period_end)
        .fetch_one(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `ConsumeAutopilotQuotaReservation`：`reserved → consumed` 并把 `reserved_count`
/// 转成 `used_count`。
///
/// `used_count` 在周期内**单调**：没有任何路径会把它减回去（被取消/阻塞/删除的 run
/// 只要消费过就仍然计数）。
///
/// `Ok(None)` = **终态重放**（预留已不是 `reserved`，上游 `pgx.ErrNoRows`）——这是正常路径，
/// 不是错误：重试的终态回调会命中它。
pub async fn consume<'c, E>(
    executor: E,
    reservation_id: Uuid,
) -> Result<Option<QuotaPeriodRow>, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = "WITH locked AS ( \
                   SELECT qr.* FROM autopilot_quota_reservation qr \
                    WHERE qr.id = $1 AND qr.state = 'reserved' FOR UPDATE \
               ), changed AS ( \
                   UPDATE autopilot_quota_reservation AS r \
                      SET state = 'consumed', finalized_at = now() \
                     FROM locked WHERE r.id = locked.id \
                      AND EXISTS (SELECT 1 FROM autopilot_quota_period p \
                                   WHERE p.workspace_id = locked.workspace_id \
                                     AND p.period_start = locked.period_start \
                                     AND p.period_end = locked.period_end) \
                  RETURNING locked.workspace_id, locked.period_start, locked.period_end \
               ) \
               UPDATE autopilot_quota_period AS p \
                  SET reserved_count = reserved_count - 1, used_count = used_count + 1, \
                      updated_at = now() \
                 FROM changed \
                WHERE p.workspace_id = changed.workspace_id \
                  AND p.period_start = changed.period_start \
                  AND p.period_end = changed.period_end \
             RETURNING p.*";
    sqlx::query_as::<_, QuotaPeriodRow>(sql)
        .bind(reservation_id)
        .fetch_optional(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `ReleaseAutopilotQuotaReservation`：`reserved → released`，`reserved_count - 1`。
///
/// **只放行仍是 `reserved` 的行** —— 已经消费掉的 `create_issue` 额度在 run 被取消/阻塞/删除后
/// 依然计入周期用量。`Ok(None)` 同上：终态重放。
pub async fn release<'c, E>(
    executor: E,
    reservation_id: Uuid,
) -> Result<Option<QuotaPeriodRow>, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = "WITH locked AS ( \
                   SELECT qr.* FROM autopilot_quota_reservation qr \
                    WHERE qr.id = $1 AND qr.state = 'reserved' FOR UPDATE \
               ), changed AS ( \
                   UPDATE autopilot_quota_reservation AS r \
                      SET state = 'released', finalized_at = now() \
                     FROM locked WHERE r.id = locked.id \
                      AND EXISTS (SELECT 1 FROM autopilot_quota_period p \
                                   WHERE p.workspace_id = locked.workspace_id \
                                     AND p.period_start = locked.period_start \
                                     AND p.period_end = locked.period_end) \
                  RETURNING locked.workspace_id, locked.period_start, locked.period_end \
               ) \
               UPDATE autopilot_quota_period AS p \
                  SET reserved_count = reserved_count - 1, updated_at = now() \
                 FROM changed \
                WHERE p.workspace_id = changed.workspace_id \
                  AND p.period_start = changed.period_start \
                  AND p.period_end = changed.period_end \
             RETURNING p.*";
    sqlx::query_as::<_, QuotaPeriodRow>(sql)
        .bind(reservation_id)
        .fetch_optional(executor)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `ListRecoverableAutopilotQuotaReservations`：找出「预留还挂着但 run 已终态（或
/// manual/api 的半截 run 且连 task 行都没有）」的行。
///
/// 两个时间界都由调用方给：`terminal_created_before` 给「run 已终态」的宽限，
/// `partial_created_before` 给「manual/api 半截 run」的宽限。schedule/webhook 的
/// 半截状态由它们自己的重试逻辑修复，**故意排除**在本查询之外。
///
/// 本仓**不发明时间窗**：上游那两条界来自 Cloud 侧策略，本地只接受参数、不给默认值；
/// 旧版的 `expired` 语义也完全由调用方决定。
pub async fn list_recoverable<'c, E>(
    executor: E,
    terminal_created_before: DateTime<Utc>,
    partial_created_before: DateTime<Utc>,
    row_limit: i64,
) -> Result<Vec<QuotaReservationRow>, RepoError>
where
    E: sqlx::PgExecutor<'c>,
{
    let sql = format!(
        // 上游就是 `SELECT r.*`（`ListRecoverableAutopilotQuotaReservations`）。这里**不能**用
        // [`QUOTA_RESERVATION_COLUMNS`]：`autopilot_run` 也有 `id` / `created_at`，展开成不限定
        // 的列名会 `column reference "id" is ambiguous`（真库用例抓到的）。
        "SELECT r.* FROM autopilot_quota_reservation r \
           LEFT JOIN autopilot_run ar ON ar.quota_reservation_id = r.id \
          WHERE r.state = 'reserved' \
            AND ((r.created_at < $1 \
                  AND (ar.id IS NULL OR ar.status IN ('completed', 'failed', 'skipped'))) \
                 OR (r.created_at < $2 AND ar.source IN ('manual', 'api') \
                     AND (ar.status = 'pending' \
                          OR (ar.status = 'issue_created' AND ar.issue_id IS NULL) \
                          OR (ar.status = 'running' AND ar.task_id IS NULL)) \
                     AND NOT EXISTS (SELECT 1 FROM agent_task_queue task \
                                      WHERE task.autopilot_run_id = ar.id))) \
          ORDER BY r.created_at LIMIT $3"
    );
    sqlx::query_as::<_, QuotaReservationRow>(&sql)
        .bind(terminal_created_before)
        .bind(partial_created_before)
        .bind(row_limit)
        .fetch_all(executor)
        .await
        .map_err(map_sqlx_err)
}

// ---------------------------------------------------------------------------
// 准入编排（配额那一半）
// ---------------------------------------------------------------------------

/// 一次准入请求的输入（**不含** `autopilot_run` 的字段 —— 那半边属 M5-4）。
#[derive(Debug, Clone)]
pub struct AdmitInput {
    /// 工作区。
    pub workspace_id: Uuid,
    /// 周期开始（entitlement 面下发）。
    pub period_start: DateTime<Utc>,
    /// 周期结束（entitlement 面下发）。
    pub period_end: DateTime<Utc>,
    /// 来源（`schedule` / `manual` / `webhook` / `api`；库里是自由文本）。
    pub source: String,
    /// 幂等键（HTTP 调用方没给时由调用方生成，作用域是「那一次请求」）。
    pub idempotency_key: String,
    /// 策略版本（entitlement 面下发，本仓不解释）。
    pub policy_revision: i64,
    /// 订阅版本（同上）。
    pub subscription_version: i64,
    /// 额度上限；`None` = 拿不到额度定义 ⇒ **fail-open**（照上游：策略形态非法就不碰配额表）。
    pub limit: Option<i64>,
    /// 是否 `enforce`：`false` = observe（只记账，不拒绝）。
    pub enforce: bool,
    /// 拒绝原因码（自由文本；上游在 enforce 拒绝时把它写进 `blocked_counts` 的 key）。
    pub reason_code: String,
}

/// 准入结果（`mc_core::autopilot_quota::QuotaDecision` 的仓储侧对应物）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// 新建了预留；调用方必须在**同一事务**里建 run 并写回 `quota_reservation_id`。
    Reserved {
        /// 预留 id。
        reservation_id: Uuid,
        /// `true` = observe 模式下「本该被拒但放行了」（上游 `would_block` 指标）。
        would_block: bool,
    },
    /// 幂等命中：复用既有预留，**不要**再建 run（否则一次请求吃两个额度）。
    Replayed {
        /// 既有预留 id。
        reservation_id: Uuid,
    },
    /// 拒绝：未占位。`blocked_counts` 已累加、周期行已锁定并提交前的最终计数在此返回。
    Denied {
        /// 已消费。
        used: i64,
        /// 已占位。
        reserved: i64,
        /// 上限。
        limit: i64,
    },
}

/// 孤儿预留的回收判定（供 [`admit`] 内部与 `mc-autopilot::quota` 复用）。
///
/// 上游：幂等键命中一个 `reserved` 预留，但**没有任何 run 指向它** ⇒ 先释放它，
/// 否则这个 key 会在整个周期内把每次重试都卡死。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanAction {
    /// 预留仍有 run 指向它 ⇒ 正常复用。
    Reuse,
    /// 预留没有 run 指向它 ⇒ 释放后走新建路径。
    ReleaseOrphan,
}

/// 判定孤儿（纯函数，便于单测）。
pub fn orphan_action(has_run: bool, reservation_state: &str) -> OrphanAction {
    if has_run || reservation_state != STATE_RESERVED {
        OrphanAction::Reuse
    } else {
        OrphanAction::ReleaseOrphan
    }
}

/// 上游 `createAutopilotRunWithQuota` 的**配额半边**（不含 run insert，见文件头）。
///
/// `existing_has_run` 由调用方提供（它才知道「按 `quota_reservation_id` 查 run」的结果），
/// 这样本函数不需要依赖 M5-4 的 run 读面。
///
/// 调用约定（**必须在事务里**，`conn` 传 `&mut *tx`）：
/// 1. 本函数内部会 `EnsureAutopilotQuotaPeriod`（拿周期行锁）→ 幂等查询 → 计数判定；
/// 2. `Reserved` 时调用方接着 `INSERT INTO autopilot_run (… quota_reservation_id …)`；
/// 3. `Denied` 时调用方**不再**建 run，直接 commit（`blocked_counts` 已累加）。
///
/// 签名收成 `&mut PgConnection`（而非泛型执行器）：函数体要多条语句复用同一个执行器，
/// `PgExecutor` 是**按值消费**的，泛型版本无法在多处复用（只读单语句函数才用泛型）。
pub async fn admit(
    conn: &mut PgConnection,
    input: &AdmitInput,
    existing_has_run: bool,
) -> Result<AdmitOutcome, RepoError> {
    let period = ensure_period(
        &mut *conn,
        input.workspace_id,
        input.period_start,
        input.period_end,
    )
    .await?;
    let existing = get_reservation_by_key(
        &mut *conn,
        input.workspace_id,
        input.period_start,
        input.period_end,
        &input.idempotency_key,
    )
    .await?;
    if let Some(row) = existing {
        match orphan_action(existing_has_run, &row.state) {
            OrphanAction::Reuse => {
                return Ok(AdmitOutcome::Replayed {
                    reservation_id: row.id,
                })
            }
            OrphanAction::ReleaseOrphan => {
                release(&mut *conn, row.id).await?;
            }
        }
    }

    let limit = input.limit.unwrap_or(i64::MAX);
    let would_block = period.total() >= limit;
    if would_block && input.enforce {
        let blocked = increment_blocked(
            &mut *conn,
            input.workspace_id,
            input.period_start,
            input.period_end,
            &input.reason_code,
        )
        .await?;
        return Ok(AdmitOutcome::Denied {
            used: blocked.used_count,
            reserved: blocked.reserved_count,
            limit,
        });
    }

    let reservation = reserve(
        &mut *conn,
        input.workspace_id,
        input.period_start,
        input.period_end,
        input.policy_revision,
        input.subscription_version,
        &input.source,
        &input.idempotency_key,
    )
    .await?;
    increment_reserved(
        &mut *conn,
        input.workspace_id,
        input.period_start,
        input.period_end,
    )
    .await?;
    Ok(AdmitOutcome::Reserved {
        reservation_id: reservation.id,
        would_block,
    })
}

// ---------------------------------------------------------------------------
// 仓储包装
// ---------------------------------------------------------------------------

/// 配额仓储（池形态入口）。
#[derive(Debug, Clone)]
pub struct QuotaRepo {
    db: Db,
}

impl QuotaRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 开一个事务（M5-4 的准入事务起点；配额与 run 必须同事务）。
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, RepoError> {
        self.db.pool().begin().await.map_err(map_sqlx_err)
    }

    /// 用量读面：本周期没有行 ⇒ `None`（= 还没用过额度，**不是** 0 行的错误）。
    pub async fn usage_period(
        &self,
        workspace_id: Uuid,
        period_start: DateTime<Utc>,
        period_end: DateTime<Utc>,
    ) -> Result<Option<QuotaPeriodRow>, RepoError> {
        get_period(self.db.pool(), workspace_id, period_start, period_end).await
    }

    /// 扫陈旧保留并逐个释放，返回被释放的预留 id（顺序与查询一致）。
    ///
    /// 释放失败**不吞**：调用方要能看到是哪一条卡住了。
    pub async fn sweep_stale(
        &self,
        terminal_created_before: DateTime<Utc>,
        partial_created_before: DateTime<Utc>,
        row_limit: i64,
    ) -> Result<Vec<Uuid>, RepoError> {
        let rows = list_recoverable(
            self.db.pool(),
            terminal_created_before,
            partial_created_before,
            row_limit,
        )
        .await?;
        let mut released = Vec::with_capacity(rows.len());
        for row in rows {
            release(self.db.pool(), row.id).await?;
            released.push(row.id);
        }
        Ok(released)
    }

    /// 事务内释放（给 [`QuotaRepo::sweep_stale`] 之外的手动修复路径用）。
    pub async fn release_in_tx(
        &self,
        tx: &mut PgConnection,
        reservation_id: Uuid,
    ) -> Result<Option<QuotaPeriodRow>, RepoError> {
        release(&mut *tx, reservation_id).await
    }
}

impl RepoWithDb for QuotaRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
