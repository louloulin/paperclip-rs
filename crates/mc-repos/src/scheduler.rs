//! 调度租约表仓储：`sys_cron_executions`（迁移 `113`）。
//!
//! - **写者**：M5-7（**W**；`docs/44` §3.2）。M5-8 只读内核算出的结果，不写本文件。
//! - **这是 M5 唯一的跨波共享点**（R2）：M5 的 autopilot job、M6 的 plugin-hook job、
//!   M3/M9 的 task-usage job 都通过这张表做并发控制 ⇒ 本文件的操作要按「内核」写，不要按
//!   「autopilot 专用」写（别在这里出现 autopilot 语义）。
//! - **上游**：`scheduler/db_ops.go`402（403）；表结构见迁移 `113_sys_cron_executions.up.sql`。
//! - **本仓约定**：裸 `Uuid` / `chrono` 类型 + `sqlx::FromRow` + `map_sqlx_err`；认领走
//!   `INSERT … ON CONFLICT DO NOTHING` 与 `UPDATE … WHERE` 的**单语句原子**路径。
//!
//! # 本文件只做 SQL，不做决策
//!
//! 「一次执行算不算抢到 / 还要不要重试 / 陈旧租约归谁」这些**决策**在
//! `mc_scheduler::db_ops`（内核侧）；本文件把上游 `db_ops.go` 的 SQL 逐条搬过来，
//! 返回值只表达「影响了几行 / 拿到哪一行」。这样 M6 / M3 / M9 复用内核时不会被迫
//! 接受 autopilot 的语义（`docs/44` §6.2 第 5 条）。
//!
//! # 租约四条（迁移 `113` 的注释就是契约）
//!
//! 1. 认领是原子的：唯一键 `(job_name, scope_kind, scope_id, plan_time)` 是
//!    「两实例不会双跑」的**唯一**屏障；抢输的一方写 0 行，静默 no-op。
//! 2. 心跳续期：`heartbeat` 只对 `lease_token` 匹配且仍 `RUNNING` 的行生效。
//! 3. 陈旧回收：没有 `STALE` 状态，「陈旧」= `status='RUNNING' AND stale_after < now()`。
//! 4. 终态写入必须带 `id + lease_token + status='RUNNING'` 三连守卫 ⇒ 被窃取过租约的
//!    旧持有者写终态**影响 0 行**，不会覆盖新一轮的状态。
//!
//! 时间基准一律取 DB `now()`（`db_now`），**不用进程时钟**：两实例时钟偏移不得导致
//! 不同的 `plan_time`（`spec.go` 的 `dbNow` 注释 / R2）。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

use mc_db::Db;

/// `sys_cron_executions.status` 的取值域（迁移 `113` 的 `chk_sys_cron_status`）。
///
/// 没有 `STALE`：陈旧是**算出来的**（`RUNNING` + `stale_after < now()`），不是物化状态 ——
/// 这样「偷租约」才是单条 `UPDATE`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// 正在跑（持有租约）。
    Running,
    /// 终态：成功。
    Success,
    /// 终态：失败（是否还能重试由 `next_retry_at` / `attempt` 决定）。
    Failed,
}

impl ExecutionStatus {
    /// 落库字面量（与迁移里的 `CHECK` 逐字一致）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Success => "SUCCESS",
            Self::Failed => "FAILED",
        }
    }

    /// 解析库里的字面量。未知值返回 `None`（由调用方决定怎么报错）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "RUNNING" => Some(Self::Running),
            "SUCCESS" => Some(Self::Success),
            "FAILED" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// 一次认领/窃取成功后的租约句柄（`RETURNING id, lease_token, attempt`）。
///
/// `lease_token` 会**每次认领都轮换**（`gen_random_uuid()`）；调用方必须原样带回去写终态，
/// 否则守卫不匹配、写 0 行。
#[derive(Debug, Clone, FromRow)]
pub struct LeaseRow {
    /// `sys_cron_executions.id`。
    pub id: Uuid,
    /// 本轮租约令牌（成功认领时刚生成的那个）。
    pub lease_token: Uuid,
    /// 本轮是第几次尝试（首次插入为 1，重试/窃取为 `attempt + 1`）。
    pub attempt: i32,
}

impl LeaseRow {
    /// 取出只用于「守卫」的那两项（心跳与终态写入只需要它们）。
    #[must_use]
    pub const fn lease(&self) -> Lease {
        Lease {
            id: self.id,
            lease_token: self.lease_token,
        }
    }
}

/// **已持有**的租约句柄：心跳与终态写入的守卫（`WHERE id = … AND lease_token = …`）。
///
/// 把两项绑成一个类型是故意的：拆成两个参数时很容易把 `id` 与 `lease_token` 传反，
/// 而传反的后果是「写 0 行」——**在日志里看不出来**（看起来像「租约被偷了」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// `sys_cron_executions.id`。
    pub id: Uuid,
    /// 本轮租约令牌。
    pub lease_token: Uuid,
}

/// 认领目标：唯一键三元组 + 计划时间桶（迁移 `113` 的 `uq_sys_cron_execution`）。
#[derive(Debug, Clone, Copy)]
pub struct PlanKey<'a> {
    /// 业务 job 名（不是 `TaskKind`）。
    pub job_name: &'a str,
    /// 作用域种类（`global` / `workspace` / `agent` …）。
    pub scope_kind: &'a str,
    /// 作用域 id（`global` 时是字面量 `global`）。
    pub scope_id: &'a str,
    /// 计划时间（已 `FloorPlan` 取整）。
    pub plan_time: DateTime<Utc>,
}

impl<'a> PlanKey<'a> {
    /// 构造。
    #[must_use]
    pub const fn new(
        job_name: &'a str,
        scope_kind: &'a str,
        scope_id: &'a str,
        plan_time: DateTime<Utc>,
    ) -> Self {
        Self {
            job_name,
            scope_kind,
            scope_id,
            plan_time,
        }
    }
}

/// 写 `FAILED` 终态时的载荷（拆出来是为了让 `finish_failure` 的参数表不失控）。
#[derive(Debug, Clone, Copy)]
pub struct FailureWrite<'a> {
    /// `None` ⇒ 落 `NULL`：语义是「尽快重试」，**不是**「不重试」（见 `LatestPlanInfo::retry_eligible`）。
    pub next_retry_at: Option<DateTime<Utc>>,
    /// 审计码；空串会被换成 `handler_error`。
    pub error_code: &'a str,
    /// 审计详情；超 4000 字符会被截断。
    pub error_msg: &'a str,
    /// 覆盖 `attempt`（内核用它烧掉重试预算，表达「不可重试」）。
    pub attempt_override: Option<i32>,
}

/// `latestPlan` 的返回视图：某个 `(job, scope)` 已存在的最新一行计划。
///
/// 上游 `LatestPlanInfo`（`db_ops.go`333）+ `RetryEligible`（`db_ops.go`351）在这里
/// **合体**：数据在哪里读出来，判据就在哪里 —— 避免内核与仓储出现两份真值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatestPlanInfo {
    /// 是否读到过行。`false` 时其余字段无意义（`plan_time` 是零值）。
    pub found: bool,
    /// 最新一行的 `plan_time`（`found == false` 时是 `DateTime::MIN_UTC`，即上游的零值）。
    pub plan_time: DateTime<Utc>,
    /// 最新一行的状态。
    pub status: ExecutionStatus,
    /// 已尝试次数。
    pub attempt: i32,
    /// 重试上限。
    pub max_attempts: i32,
    /// `next_retry_at`：`None` 表示「没有下次」或「立刻可重试」，语义见 `retry_eligible`。
    pub next_retry_at: Option<DateTime<Utc>>,
}

impl Default for LatestPlanInfo {
    fn default() -> Self {
        Self {
            found: false,
            plan_time: DateTime::<Utc>::MIN_UTC,
            status: ExecutionStatus::Running,
            attempt: 0,
            max_attempts: 0,
            next_retry_at: None,
        }
    }
}

impl LatestPlanInfo {
    /// 空视图（`(job, scope)` 还没有任何历史）。
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// 「这一行还要不要在下个 tick 继续占用同一个 `plan_time`」（上游 `RetryEligible`18）。
    ///
    /// 判据顺序与上游逐条对应：
    /// 1. 没读到过 → `false`；
    /// 2. 不是 `FAILED` → `false`（`SUCCESS` / `RUNNING` 都不再重试）；
    /// 3. `attempt >= max_attempts` → `false`（预算用尽）；
    /// 4. `next_retry_at` 为 `NULL` → `true`（`COALESCE` 语义：NULL 表示「尽快」，就是现在）；
    /// 5. 否则 `next_retry_at <= now`。
    #[must_use]
    pub fn retry_eligible(&self, now: DateTime<Utc>) -> bool {
        if !self.found {
            return false;
        }
        if self.status != ExecutionStatus::Failed {
            return false;
        }
        if self.attempt >= self.max_attempts {
            return false;
        }
        self.next_retry_at.is_none_or(|at| at <= now)
    }
}

/// `sys_cron_executions` 的仓储。
///
/// 克隆成本 = 一次 `Db` 克隆（内部是 `sqlx::Pool` 的 `Arc`），所以内核可以放心把它塞进
/// 每个 handler 的 `Heartbeat` 里。
#[derive(Clone)]
pub struct SchedulerRepo {
    db: Db,
}

impl SchedulerRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 从连接串建仓储（池规格由调用方给，与其它仓储一致）。
    ///
    /// `main.rs` 里已经有 `Db` 时应走 [`SchedulerRepo::new`]（复用同一个池）；本入口是给
    /// 「手上只有连接串」的调用方 —— 尤其是**跨 crate 的内核测试**：`mc-scheduler` 看不到
    /// `mc-db`，有这个入口就不用为一个测试加依赖边（见 `docs/46` §7）。
    pub async fn connect(url: &str, max_connections: u32, min_connections: u32) -> Result<Self> {
        let db = Db::connect(url, max_connections, min_connections)
            .await
            .map_err(|err| crate::RepoError::Db(err.to_string()))?;
        Ok(Self::new(db))
    }

    /// DB 的「现在」（`SELECT now()`）——**调度器唯一的时钟来源**。
    ///
    /// 用 DB 时钟而不是进程时钟，是为了让两台机器上的 `plan_time` 取整落在同一个桶里
    /// （`utils` 里那句「两实例时钟偏移不得导致不同 `plan_time`」）。
    pub async fn db_now(&self) -> Result<DateTime<Utc>> {
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(now)
    }

    /// 新鲜插入路径：抢一个新 `(job, scope, plan_time)` 的租约。
    ///
    /// `ON CONFLICT … DO NOTHING` ⇒ 输了的一方**碰都不碰**已存在的行（不轮换别人的 token）。
    /// 返回 `None` = 冲突（行已存在，胜负由调用方的第二条 SQL 决定）。
    ///
    /// `exec_id`: `None` 时由 DB 生成（`gen_random_uuid()`）；上游用 `UUIDv7` 求主键局部性，
    /// 内核侧会显式传 `Uuid::now_v7()`，这里两种都支持。
    pub async fn claim_fresh(
        &self,
        key: &PlanKey<'_>,
        max_attempts: i32,
        runner_id: &str,
        db_time: DateTime<Utc>,
        stale_secs: f64,
        exec_id: Option<Uuid>,
    ) -> Result<Option<LeaseRow>> {
        let row = sqlx::query_as::<_, LeaseRow>(
            r"
            INSERT INTO sys_cron_executions (
                job_name, scope_kind, scope_id, plan_time,
                status, attempt, max_attempts,
                runner_id, lease_token,
                heartbeat_at, stale_after,
                started_at, updated_at,
                id
            ) VALUES (
                $1, $2, $3, $4,
                'RUNNING', 1, $5,
                $6, gen_random_uuid(),
                $7::timestamptz, $7::timestamptz + make_interval(secs => $8),
                $7::timestamptz, $7::timestamptz,
                COALESCE($9::uuid, gen_random_uuid())
            )
            ON CONFLICT ON CONSTRAINT uq_sys_cron_execution DO NOTHING
            RETURNING id, lease_token, attempt
            ",
        )
        .bind(key.job_name)
        .bind(key.scope_kind)
        .bind(key.scope_id)
        .bind(key.plan_time)
        .bind(max_attempts)
        .bind(runner_id)
        .bind(db_time)
        .bind(stale_secs)
        .bind(exec_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 冲突路径：**一条语句**同时表达「失败后重试」与「偷陈旧租约」两个分支。
    ///
    /// `WHERE` 的语义（与上游 `db_ops.go`116 逐字对应）：
    /// * `attempt < max_attempts` —— 预算校验放在 SQL 里，handler 无从「逃出」重试信封；
    /// * `status='FAILED' AND COALESCE(next_retry_at, now) <= now` —— 退避到期才允许重试；
    /// * `status='RUNNING' AND stale_after < now AND $8` —— 只有 `allow_stale_reentry` 的
    ///   job 才允许被偷；否则陈旧租约只能由 [`Self::mark_stale_as_failed`] 收成 `FAILED`。
    ///
    /// 命中时 `attempt = attempt + 1` 且**轮换** `lease_token` ⇒ 旧持有者的终态写入从此失效。
    /// 返回 `None` = 仍被别人持有 / 已终态 / 退避未到 / 预算用尽（调用方一律当 no-op）。
    pub async fn claim_steal_or_retry(
        &self,
        key: &PlanKey<'_>,
        runner_id: &str,
        db_time: DateTime<Utc>,
        stale_secs: f64,
        allow_stale_reentry: bool,
    ) -> Result<Option<LeaseRow>> {
        let row = sqlx::query_as::<_, LeaseRow>(
            r"
            UPDATE sys_cron_executions
               SET status        = 'RUNNING',
                   attempt       = attempt + 1,
                   runner_id     = $1,
                   lease_token   = gen_random_uuid(),
                   heartbeat_at  = $2::timestamptz,
                   stale_after   = $2::timestamptz + make_interval(secs => $3),
                   started_at    = $2::timestamptz,
                   finished_at   = NULL,
                   duration_ms   = NULL,
                   next_retry_at = NULL,
                   error_code    = NULL,
                   error_msg     = NULL,
                   updated_at    = $2::timestamptz
             WHERE job_name   = $4
               AND scope_kind = $5
               AND scope_id   = $6
               AND plan_time  = $7
               AND attempt < max_attempts
               AND (
                    (status = 'FAILED' AND COALESCE(next_retry_at, $2::timestamptz) <= $2::timestamptz)
                    OR
                    (status = 'RUNNING' AND stale_after < $2::timestamptz AND $8)
               )
            RETURNING id, lease_token, attempt
            ",
        )
        .bind(runner_id)
        .bind(db_time)
        .bind(stale_secs)
        .bind(key.job_name)
        .bind(key.scope_kind)
        .bind(key.scope_id)
        .bind(key.plan_time)
        .bind(allow_stale_reentry)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 心跳续期。`false` = 影响 0 行 = 租约已不是我们的（被偷 / 已终态）。
    ///
    /// 与上游一致：`stale_after` 基于 **SQL 里的 `now()`** 推进，不绑定调用方的时钟。
    pub async fn heartbeat(&self, lease: Lease, stale_secs: f64) -> Result<bool> {
        let tag = sqlx::query(
            r"
            UPDATE sys_cron_executions
               SET heartbeat_at = now(),
                   stale_after  = now() + make_interval(secs => $3),
                   updated_at   = now()
             WHERE id          = $1
               AND lease_token = $2
               AND status      = 'RUNNING'
            ",
        )
        .bind(lease.id)
        .bind(lease.lease_token)
        .bind(stale_secs)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(tag.rows_affected() == 1)
    }

    /// 写终态 `SUCCESS`（带 `id + lease_token + status='RUNNING'` 守卫）。
    ///
    /// `false` = 影响 0 行 = 租约已丢 ⇒ 旧持有者**不得**覆盖新一轮的状态。
    /// `result_json` 为 `None` 时落 `{}`（列默认值），非 `None` 时校验大小上限。
    pub async fn finish_success(
        &self,
        lease: Lease,
        db_time: DateTime<Utc>,
        duration_ms: i64,
        rows_affected: i64,
        result_json: Option<&str>,
    ) -> Result<bool> {
        let payload = encode_result(result_json)?;
        let tag = sqlx::query(
            r"
            UPDATE sys_cron_executions
               SET status        = 'SUCCESS',
                   finished_at   = $3,
                   duration_ms   = $4,
                   rows_affected = $5,
                   result        = $6::jsonb,
                   error_code    = NULL,
                   error_msg     = NULL,
                   updated_at    = $3
             WHERE id            = $1
               AND lease_token   = $2
               AND status        = 'RUNNING'
            ",
        )
        .bind(lease.id)
        .bind(lease.lease_token)
        .bind(db_time)
        .bind(duration_ms)
        .bind(rows_affected)
        .bind(payload)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(tag.rows_affected() == 1)
    }

    /// 写终态 `FAILED`（同样的三连守卫）。
    ///
    /// * `next_retry_at = None` ⇒ 落 `NULL`：**只有当调用方已经耗光重试预算（或错误被判定为
    ///   不可重试）时**才可以这么传。注意 `NULL` 在 [`LatestPlanInfo::retry_eligible`] 里不是
    ///   「不重试」而是「尽快重试」，所以「不可重试」必须靠 `attempt_override` 把 `attempt`
    ///   推到 `max_attempts` 来表达（迁移 `113` 的 `CHECK` 不允许 `attempt > max_attempts`）。
    /// * `attempt_override`：`None` 保持原值；`Some(n)` 把 `attempt` 改成 `n`（内核侧用它烧掉
    ///   预算，见 `mc_scheduler::error::ErrorClass::Permanent`）。
    ///
    /// `false` = 影响 0 行 = 租约已丢。
    pub async fn finish_failure(
        &self,
        lease: Lease,
        db_time: DateTime<Utc>,
        duration_ms: i64,
        write: &FailureWrite<'_>,
    ) -> Result<bool> {
        let tag = sqlx::query(
            r"
            UPDATE sys_cron_executions
               SET status        = 'FAILED',
                   finished_at   = $3,
                   duration_ms   = $4,
                   next_retry_at = $5,
                   error_code    = $6,
                   error_msg     = $7,
                   attempt       = COALESCE($8, attempt),
                   updated_at    = $3
             WHERE id            = $1
               AND lease_token   = $2
               AND status        = 'RUNNING'
            ",
        )
        .bind(lease.id)
        .bind(lease.lease_token)
        .bind(db_time)
        .bind(duration_ms)
        .bind(write.next_retry_at)
        .bind(default_error_code(write.error_code))
        .bind(truncate_error_msg(write.error_msg))
        .bind(write.attempt_override)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(tag.rows_affected() == 1)
    }

    /// 把某 job 的**陈旧** `RUNNING` 行收成 `FAILED`（`error_code='stale_timeout'`）。
    ///
    /// 返回影响行数，供内核记指标 / 打日志。上游对**每个 job 每个 tick**都跑这一步，
    /// 与 `allow_stale_reentry` 无关：允许被偷的 job 也可能因为「历史 `plan_time` 再也不会
    /// 被重算」而把租约永远挂在表里（`manager.go`146 那段注释）。
    pub async fn mark_stale_as_failed(
        &self,
        job_name: &str,
        db_time: DateTime<Utc>,
    ) -> Result<u64> {
        let tag = sqlx::query(
            r"
            UPDATE sys_cron_executions
               SET status      = 'FAILED',
                   finished_at = $2,
                   error_code  = 'stale_timeout',
                   error_msg   = 'lease expired without heartbeat',
                   updated_at  = $2
             WHERE job_name    = $1
               AND status      = 'RUNNING'
               AND stale_after < $2
            ",
        )
        .bind(job_name)
        .bind(db_time)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(tag.rows_affected())
    }
}

impl SchedulerRepo {
    /// 某个 `(job, scope)` 的**最新一行**（按 `plan_time DESC LIMIT 1`）。
    ///
    /// 没有任何历史时返回 [`LatestPlanInfo::empty`]（`found = false`），不是错误 ——
    /// 「首次启动」是正常状态，`every_plan` 的追赶游标就是从这里 bootstrap 的。
    pub async fn latest_plan(
        &self,
        job_name: &str,
        scope_kind: &str,
        scope_id: &str,
    ) -> Result<LatestPlanInfo> {
        let row: Option<LatestPlanRow> = sqlx::query_as(
            r"
            SELECT plan_time, status, attempt, max_attempts, next_retry_at
              FROM sys_cron_executions
             WHERE job_name   = $1
               AND scope_kind = $2
               AND scope_id   = $3
             ORDER BY plan_time DESC
             LIMIT 1
            ",
        )
        .bind(job_name)
        .bind(scope_kind)
        .bind(scope_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;

        let Some(row) = row else {
            return Ok(LatestPlanInfo::empty());
        };
        let status = ExecutionStatus::parse(&row.status).ok_or_else(|| {
            crate::RepoError::Db(format!(
                "sys_cron_executions.status 越出 CHECK 约束：{:?}",
                row.status
            ))
        })?;
        Ok(LatestPlanInfo {
            found: true,
            plan_time: row.plan_time,
            status,
            attempt: row.attempt,
            max_attempts: row.max_attempts,
            next_retry_at: row.next_retry_at,
        })
    }
}

impl RepoWithDb for SchedulerRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// `latest_plan` 的原始行（`status` 先按文本读，再交给 [`ExecutionStatus::parse`]）。
#[derive(Debug, sqlx::FromRow)]
struct LatestPlanRow {
    plan_time: DateTime<Utc>,
    status: String,
    attempt: i32,
    max_attempts: i32,
    next_retry_at: Option<DateTime<Utc>>,
}

/// `error_code` 为空时落 `handler_error`（上游 `db_ops.go`277）。
fn default_error_code(code: &str) -> &str {
    if code.is_empty() {
        "handler_error"
    } else {
        code
    }
}

/// 错误文本截断。上游按 4000 **字节**截（`errorMsg[:4000]`）—— 本地按 **char** 截，
/// 因为按字节切会切碎 UTF-8 字符（PG 侧会直接报 `invalid byte sequence`）。
/// 4000 个字符的 UTF-8 最长 16KB，仍在 `TEXT` 无压力区内。
fn truncate_error_msg(msg: &str) -> String {
    const LIMIT: usize = 4000;
    if msg.chars().count() <= LIMIT {
        return msg.to_owned();
    }
    msg.chars().take(LIMIT).collect()
}

/// `result` 列的守卫：空 ⇒ `{}`；非空 ⇒ 原样交给 `$6::jsonb`，但先卡 16KB。
///
/// 上游 `encodeResult`308 用 `json.Marshal` 之后再卡大小；本地**不**重新序列化 ——
/// 内核不引 `serde_json`（M5-0 的依赖表已冻结），handler 直接给 JSON 文本，
/// 合法性由 PG 的 `::jsonb` 转换来判（非法 JSON ⇒ `RepoError::Db`）。
fn encode_result(result_json: Option<&str>) -> Result<String> {
    const MAX_BYTES: usize = 16 * 1024;
    let payload = match result_json {
        None => return Ok("{}".to_owned()),
        Some(raw) if raw.trim().is_empty() => return Ok("{}".to_owned()),
        Some(raw) => raw,
    };
    if payload.len() > MAX_BYTES {
        return Err(crate::RepoError::Db(format!(
            "result 载荷过大（{} 字节 > {}）；大对象请进结构化日志",
            payload.len(),
            MAX_BYTES
        )));
    }
    Ok(payload.to_owned())
}
