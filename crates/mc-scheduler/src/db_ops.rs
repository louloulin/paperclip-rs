//! 租约表的读写（`sys_cron_executions`）。
//!
//! - **写者**：M5-7。
//! - **上游**：`scheduler/db_ops.go`402（403）—— `tryClaim`110 + `markStaleAsFailed`26 +
//!   `finishFailure`46 + `RetryEligible`18。
//! - **租约四条**（`docs/44` §6.2）：认领要原子（`RETURNING`）、心跳要续期、
//!   陈旧要回收（`markStaleAsFailed`）、失败要按 `RetryEligible` 决定是否留待重试。
//! - **仓库层**：本 crate **不**直接拿 `sqlx::Pool` 写 SQL；表访问走 `mc_repos::scheduler`
//!   （M5-7 的仓储片，写者也是 M5-7，所以同一 PR 内自洽；别的切片不要加查询）。
//!
//! **状态：M5-7 已落地**（`LUM-1566`）。本文件是**薄壳**：SQL 全在 `mc_repos::scheduler`，
//! 这里只做两件事 ——
//!
//! 1. 把上游 `claim` 的三个 `bool` 收敛成 [`Claim`] 枚举（`Won` / `Stole` / `Conflicted`）：
//!    三个互斥布尔在 Rust 里是典型的「可以用类型排除的非法状态」。
//! 2. 把「影响 0 行」翻译成[`SchedulerError::LeaseLost`]（心跳与终态写入的**唯一**失败语义）。
//!
//! 这样 `mc-scheduler` 的内核不绑定任何业务，M6 的 plugin-hook job / M3、M9 的 task-usage
//! job 可以直接复用这套原语（`docs/44` §1.2）。

use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use mc_repos::scheduler::{FailureWrite, LatestPlanInfo, Lease, PlanKey, SchedulerRepo};

use crate::error::{SchedulerError, SchedulerResult};
use crate::spec::{stale_secs_f64, HandlerResult, JobSpec, Scope};

/// 认领的三种归宿（上游 `claim` 的 `Won` / `Stole` / `Conflicted`）。
///
/// `Conflicted` 的成因有四种（别人正持有 / 已终态 / 退避未到 / 预算用尽），
/// 调用方**一律当 no-op**：上游也是这么做的（「the caller treats every Conflicted
/// outcome the same way」），区分它们靠审计行指标而不是返回值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimKind {
    /// 新鲜插入拿下。
    Won,
    /// 抢到陈旧租约 / 重试已到期的 `FAILED` 行。
    Stole,
    /// 没抢到（no-op）。
    Conflicted,
}

/// 认领成功后的租约 + 尝试号。
#[derive(Debug, Clone, Copy)]
pub struct Claimed {
    /// 心跳与终态写入的守卫（`id + lease_token`）。
    pub lease: Lease,
    /// 本轮是第几次尝试（首次为 1）。
    pub attempt: i32,
}

/// `try_claim` 的结果。
#[derive(Debug, Clone, Copy)]
pub enum Claim {
    /// 新鲜插入拿下。
    Won(Claimed),
    /// 抢到陈旧租约 / 重试已到期的 `FAILED` 行。
    Stole(Claimed),
    /// 没抢到（no-op）。
    Conflicted,
}

impl Claim {
    /// 归宿（日志 / 指标用）。
    #[must_use]
    pub const fn kind(&self) -> ClaimKind {
        match self {
            Self::Won(_) => ClaimKind::Won,
            Self::Stole(_) => ClaimKind::Stole,
            Self::Conflicted => ClaimKind::Conflicted,
        }
    }

    /// 拿到租约则返回它（`Conflicted` ⇒ `None`）。
    #[must_use]
    pub const fn claimed(&self) -> Option<&Claimed> {
        match self {
            Self::Won(c) | Self::Stole(c) => Some(c),
            Self::Conflicted => None,
        }
    }
}

/// 认领一个 `(job, scope, plan_time)` 的租约（上游 `db_ops.go:66` 的 `tryClaim`）。
///
/// 两条 SQL 的分工（**唯一键是唯一的同步点**，没有进程内闸）：
/// 1. `INSERT … ON CONFLICT DO NOTHING RETURNING` —— 赢家一次插入拿到租约；输家 0 行，
///    **碰都不碰**已存在的行（绝不轮换别人的 token）。
/// 2. 冲突才走 `UPDATE … WHERE (FAILED 且退避到期) OR (RUNNING 且陈旧且允许偷)` ——
///    单条语句由 DB 判断到底能不能夺过来。
///
/// `db_time` 是**唯一**的时钟（调用方每 tick 只读一次 `SELECT now()`，所有 scope 共用）。
pub async fn try_claim(
    repo: &SchedulerRepo,
    job: &JobSpec,
    scope: &Scope,
    plan_time: DateTime<Utc>,
    db_time: DateTime<Utc>,
    runner_id: &str,
) -> SchedulerResult<Claim> {
    let key = PlanKey::new(&job.name, &scope.kind, &scope.id, plan_time);
    let stale_secs = job.stale_secs();

    // 主键用 UUIDv7（PG 主键局部性）；`lease_token` 由 DB 的 `gen_random_uuid()` 轮换
    // ——它是**认领令牌**，不能带可猜的时间戳（上游同此注释）。
    let fresh = repo
        .claim_fresh(
            &key,
            job.max_attempts,
            runner_id,
            db_time,
            stale_secs,
            Some(Uuid::now_v7()),
        )
        .await?;
    if let Some(row) = fresh {
        return Ok(Claim::Won(Claimed {
            lease: row.lease(),
            attempt: row.attempt,
        }));
    }

    let stolen = repo
        .claim_steal_or_retry(&key, runner_id, db_time, stale_secs, job.allow_stale_reentry)
        .await?;
    Ok(match stolen {
        Some(row) => Claim::Stole(Claimed {
            lease: row.lease(),
            attempt: row.attempt,
        }),
        None => Claim::Conflicted,
    })
}

/// 心跳续期一次。影响 0 行 ⇒ [`SchedulerError::LeaseLost`]。
pub async fn heartbeat(
    repo: &SchedulerRepo,
    lease: Lease,
    stale_timeout: StdDuration,
) -> SchedulerResult<()> {
    if repo.heartbeat(lease, stale_secs_f64(stale_timeout)).await? {
        Ok(())
    } else {
        Err(SchedulerError::LeaseLost("heartbeat"))
    }
}

/// 写终态 `SUCCESS`。影响 0 行 ⇒ 租约已丢（旧持有者**不得**覆盖新一轮的状态）。
pub async fn finish_success(
    repo: &SchedulerRepo,
    lease: Lease,
    db_time: DateTime<Utc>,
    duration_ms: i64,
    result: &HandlerResult,
) -> SchedulerResult<()> {
    let ok = repo
        .finish_success(
            lease,
            db_time,
            duration_ms,
            result.rows_affected,
            result.result_json.as_deref(),
        )
        .await?;
    if ok {
        Ok(())
    } else {
        Err(SchedulerError::LeaseLost("finish_success"))
    }
}

/// 写终态 `FAILED`。影响 0 行 ⇒ 租约已丢。
///
/// 重试决策（`next_retry_at` / `attempt_override`）由**调用方**算好后装进 [`FailureWrite`]：
/// 上游也是这么分工的（`manager.go` 算 `nextRetry`，`db_ops.go` 只管写）。
pub async fn finish_failure(
    repo: &SchedulerRepo,
    lease: Lease,
    db_time: DateTime<Utc>,
    duration_ms: i64,
    write: &FailureWrite<'_>,
) -> SchedulerResult<()> {
    let ok = repo
        .finish_failure(lease, db_time, duration_ms, write)
        .await?;
    if ok {
        Ok(())
    } else {
        Err(SchedulerError::LeaseLost("finish_failure"))
    }
}

/// 把某 job 的陈旧 `RUNNING` 行收成 `FAILED(error_code='stale_timeout')`，返回影响行数。
///
/// 上游对**每个 job 每个 tick**都跑（与 `allow_stale_reentry` 无关）—— 对允许被偷的 job
/// 也一样，否则崩溃进程留下的 `RUNNING` 行会永远挂着（`manager.go:146` 那段注释）。
pub async fn mark_stale_as_failed(
    repo: &SchedulerRepo,
    job_name: &str,
    db_time: DateTime<Utc>,
) -> SchedulerResult<u64> {
    Ok(repo.mark_stale_as_failed(job_name, db_time).await?)
}

/// 某个 `(job, scope)` 的最新一行历史（`found == false` 是正常状态，不是错误）。
///
/// 暴露给 [`crate::spec::PlansHook`]：hook 拿到的就是这个视图，于是「重试游标」只有一个真值。
pub async fn latest_plan(
    repo: &SchedulerRepo,
    job_name: &str,
    scope: &Scope,
) -> SchedulerResult<LatestPlanInfo> {
    Ok(repo
        .latest_plan(job_name, &scope.kind, &scope.id)
        .await?)
}

/// 递给 handler 的租约续期句柄（上游 `HandlerInput.Heartbeat` 闭包的对应物）。
///
/// 做成 `Clone` 的小结构体而不是装箱闭包：handler 可以在自己的子任务里持有它，
/// 也可以在测试里直接构造。
#[derive(Clone)]
pub struct Heartbeat {
    repo: SchedulerRepo,
    lease: Lease,
    stale_timeout: StdDuration,
}

impl Heartbeat {
    /// 构造（内核在 `run_claimed` 里建；handler 只消费）。
    #[must_use]
    pub const fn new(repo: SchedulerRepo, lease: Lease, stale_timeout: StdDuration) -> Self {
        Self {
            repo,
            lease,
            stale_timeout,
        }
    }

    /// 续期一次。`Err(LeaseLost)` ⇒ handler 应尽快收尾返回。
    pub async fn beat(&self) -> SchedulerResult<()> {
        heartbeat(&self.repo, self.lease, self.stale_timeout).await
    }

    /// 当前持有的租约（审计 / 日志用）。
    #[must_use]
    pub const fn lease(&self) -> Lease {
        self.lease
    }
}
