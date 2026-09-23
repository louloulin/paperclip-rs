//! 主循环：每 tick 取计划、认领、执行、心跳。
//!
//! - **写者**：M5-7。
//! - **上游**：`scheduler/manager.go`489（490）—— `plansForTick`95 + `runClaimed`116 +
//!   `runHeartbeats`33 + `classifyError`23（分类在 `error.rs`）。
//! - **形态**：`tokio::time::interval` + `tokio::spawn`，取消用 `CancellationToken`
//!   （`tokio-util` 已在本 crate 依赖表里）。
//! - **空注册表也能跑**：M5-7 交付时注册表为空 ⇒ 本循环必须能空转并干净退出（可独立验收）。
//! - **单进程多实例安全**：任何「只跑一次」的判定都要过 `db_ops.rs` 的租约，不要用进程内静态量。
//!
//! **状态：M5-7 已落地**（`LUM-1566`）。与上游的差异：
//!
//! 1. **注册表在 `spawn` 时冻结**：上游用 `RWMutex` + 每次 tick `snapshot()`，允许 `Run`
//!    之后再 `Register`。本地改成「`register` 要 `&mut self`，`spawn(self)` 消费管理器」
//!    —— 直接让「注册早于启动」变成类型约束，省掉锁与撕快照的可能。
//! 2. **handler 在独立 task 里跑**：超时/取消时用 [`AbortOnDrop`] abort 它（上游靠
//!    `context` 传播，Go 里没法强杀 goroutine）。因此超时会话里 handler **真的**被中断，
//!    不会出现「已经写了 FAILED、handler 还在写业务行」。
//! 3. 心跳任务照上游用**分离**的取消源：`run_timeout` 到期不该让续期停摆
//!    （上游注释：「a slow ctx cancellation cannot drop the renewal」）。
//! 4. `Permanent` 错误的处置是本地加法（见 [`crate::error::ErrorClass`]）。

use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use mc_repos::scheduler::{FailureWrite, Lease, SchedulerRepo};

use crate::db_ops::{self, Claim, ClaimKind, Claimed, Heartbeat};
use crate::error::{ErrorClass, SchedulerError, SchedulerResult};
use crate::spec::{floor_plan, CatchUpMode, HandlerInput, HandlerResult, JobSpec, Scope};

/// 默认 tick 间隔（上游 `NewManager` 的 `30 * time.Second`）。
pub const DEFAULT_TICK_INTERVAL: StdDuration = StdDuration::from_secs(30);

/// 单次心跳续期的超时（上游 `runHeartbeats` 里硬编码的 5s）。
const HEARTBEAT_TIMEOUT: StdDuration = StdDuration::from_secs(5);

/// 管理器配置（上游 `Options`）。
#[derive(Debug, Clone)]
pub struct Options {
    /// 本进程在审计行里的标识；空串 ⇒ 随机 UUID（构造时补齐）。
    pub runner_id: String,
    /// tick 间隔；应小于最短 job cadence；`0` ⇒ [`DEFAULT_TICK_INTERVAL`]。
    pub tick_interval: StdDuration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            runner_id: Uuid::new_v4().to_string(),
            tick_interval: DEFAULT_TICK_INTERVAL,
        }
    }
}

impl Options {
    /// 指定 runner 标识（测试里要可预测的审计行时用）。
    #[must_use]
    pub fn with_runner_id(mut self, runner_id: impl Into<String>) -> Self {
        self.runner_id = runner_id.into();
        self
    }

    /// 指定 tick 间隔。
    #[must_use]
    pub const fn with_tick_interval(mut self, tick_interval: StdDuration) -> Self {
        self.tick_interval = tick_interval;
        self
    }
}

/// 每进程一个的调度管理器：注册若干 job，`spawn()` 之后每 tick 跑一遍。
pub struct Manager {
    repo: SchedulerRepo,
    opts: Options,
    jobs: Vec<Arc<JobSpec>>,
}

impl Manager {
    /// 构造（上游 `NewManager`；`runner_id` / `tick_interval` 的零值在这里补齐）。
    #[must_use]
    pub fn new(repo: SchedulerRepo, mut opts: Options) -> Self {
        if opts.runner_id.is_empty() {
            opts.runner_id = Uuid::new_v4().to_string();
        }
        if opts.tick_interval.is_zero() {
            opts.tick_interval = DEFAULT_TICK_INTERVAL;
        }
        Self {
            repo,
            opts,
            jobs: Vec::new(),
        }
    }

    /// 租约仓储（handler 之外的调用方偶尔要读 `db_now()` 之类）。
    #[must_use]
    pub const fn repo(&self) -> &SchedulerRepo {
        &self.repo
    }

    /// 已注册的 job（按注册顺序；上游是 map，顺序无意义，这里顺序稳定便于测试）。
    #[must_use]
    pub fn jobs(&self) -> &[Arc<JobSpec>] {
        &self.jobs
    }

    /// 本进程的 runner 标识。
    #[must_use]
    pub fn runner_id(&self) -> &str {
        &self.opts.runner_id
    }

    /// 注册一个 job（上游 `Register`）。**必须在 `spawn` 之前**调（见本模块文档第 1 条）。
    ///
    /// 失败原因只有两种：`InvalidSpec`（[`JobSpec::validate`] 不通过）与 `DuplicateJob`。
    pub fn register(&mut self, job: JobSpec) -> SchedulerResult<()> {
        job.validate()?;
        if self.jobs.iter().any(|existing| existing.name == job.name) {
            return Err(SchedulerError::DuplicateJob(job.name));
        }
        self.jobs.push(Arc::new(job));
        Ok(())
    }

    /// 跑**一个** tick（上游 `RunOnce`）：读一次 DB 的 `now()`，然后逐个 job 跑。
    ///
    /// 单个 job 的错误只记 warn、不中断整轮（上游同）；只有「读 DB 时钟失败」才会让
    /// 整轮返回 `Err` —— 时钟都读不到就没有任何计划可算。
    pub async fn run_once(&self) -> SchedulerResult<()> {
        let now = self.repo.db_now().await?;
        for job in &self.jobs {
            if let Err(err) = self.run_job(job, now).await {
                tracing::warn!(job = %job.name, error = %err, "scheduler: job tick error");
            }
        }
        Ok(())
    }

    /// 起后台循环并返回句柄（`shutdown()` 会等循环真的退出）。
    ///
    /// 第一次 tick 是**立刻**的（`tokio::time::interval` 的语义，对应上游「先 `RunOnce`
    /// 再 `NewTicker`」），所以新进程不用等一整个 interval 才干活。
    #[must_use]
    pub fn spawn(self) -> SchedulerHandle {
        let token = CancellationToken::new();
        let join = tokio::spawn(self.run_loop(token.clone()));
        SchedulerHandle {
            token,
            join: Some(join),
        }
    }

    /// 后台循环：每 `tick_interval` 跑一次 `run_once`，取消时立刻退出。
    async fn run_loop(self, token: CancellationToken) {
        tracing::info!(
            tick_interval_ms = u64::try_from(self.opts.tick_interval.as_millis()).unwrap_or(u64::MAX),
            jobs = self.jobs.len(),
            runner_id = %self.opts.runner_id,
            "scheduler starting"
        );
        let mut ticker = tokio::time::interval(self.opts.tick_interval);
        // 上游是 `time.Ticker`（错过就丢，不补跑）；`Delay` 是同一语义。
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = token.cancelled() => break,
                _ = ticker.tick() => {}
            }
            // 外面再套一层 select：取消要能打断**正在跑**的一轮，而不是等它跑完
            // （graceful shutdown 不能让一个慢 job 把进程吊住）。
            tokio::select! {
                () = token.cancelled() => break,
                result = self.run_once() => {
                    if let Err(err) = result {
                        tracing::warn!(error = %err, "scheduler: tick error");
                    }
                }
            }
        }
        tracing::info!("scheduler stopped");
    }

    /// 一个 job 的一轮：作用域 → 回收陈旧 → 逐 scope 取计划 → 逐计划认领执行。
    ///
    /// 返回 `Err` 只表示**作用域提供者**失败（上游把整 job 本 tick 跳过）；
    /// 计划计算 / 认领失败都是 warn + continue。
    async fn run_job(&self, job: &Arc<JobSpec>, now: DateTime<Utc>) -> SchedulerResult<()> {
        let scopes = (job.scopes)(now).await?;

        // 回收陈旧租约：对**每个 job 每个 tick**都做，与 `allow_stale_reentry` 无关
        // （上游 `manager.go:146`：允许被偷的 job 也可能再也不重算那个历史 plan_time，
        // 不回收就会让 RUNNING 永远挂着）。
        match db_ops::mark_stale_as_failed(&self.repo, &job.name, now).await {
            Ok(0) => {}
            Ok(rows) => tracing::warn!(
                job = %job.name,
                rows,
                reentrant = job.allow_stale_reentry,
                "scheduler: closed out abandoned RUNNING leases"
            ),
            Err(err) => tracing::warn!(
                job = %job.name,
                error = %err,
                "scheduler: mark stale failed"
            ),
        }

        for scope in &scopes {
            let plans = match self.plans_for_tick(job, scope, now).await {
                Ok(plans) => plans,
                Err(err) => {
                    tracing::warn!(
                        job = %job.name,
                        scope = %scope,
                        error = %err,
                        "scheduler: plan computation"
                    );
                    continue;
                }
            };
            for plan_time in plans {
                self.process_plan(job, scope, plan_time, now).await;
            }
        }
        Ok(())
    }
}

impl Manager {
    /// 算本 tick 要尝试的 `plan_time` 列表（上游 `plansForTick`95）。
    ///
    /// 两条路：
    /// * 有 [`crate::spec::PlansHook`] ⇒ 读最新历史交给 hook；hook 说了算，
    ///   `max_plans_per_tick` 只当安全阀（`0` = 不截断）。
    /// * 没有 hook ⇒ `floor_plan(db_now - schedule_delay)` 就是本 tick 的桶；
    ///   `LatestOnly` 只交它一个，`EveryPlan` 从历史游标往后补齐。
    async fn plans_for_tick(
        &self,
        job: &Arc<JobSpec>,
        scope: &Scope,
        now: DateTime<Utc>,
    ) -> SchedulerResult<Vec<DateTime<Utc>>> {
        if let Some(hook) = job.plans_for_scope.clone() {
            let info = db_ops::latest_plan(&self.repo, &job.name, scope).await?;
            let mut plans = hook(scope.clone(), now, info).await?;
            if job.max_plans_per_tick > 0 && plans.len() > job.max_plans_per_tick {
                plans.truncate(job.max_plans_per_tick);
            }
            return Ok(plans);
        }

        let eligible = now - job.schedule_delay;
        let latest = floor_plan(eligible, job.cadence);
        if latest > eligible {
            // 取整落到了未来（只会发生在比 tick 还小的 cadence 上）：本 tick 无事。
            return Ok(Vec::new());
        }
        match job.catch_up_mode {
            CatchUpMode::LatestOnly => Ok(vec![latest]),
            CatchUpMode::EveryPlan => self.every_plan_plans(job, scope, now, latest).await,
        }
    }

    /// `EveryPlan` 的追赶游标（上游 `plansForTick` 的 `case CatchUpEveryPlan`）。
    ///
    /// 游标三条规则：
    /// 1. 最新行**还能重试**（`FAILED` + 预算未尽 + 退避到期）⇒ 停在**同一个** `plan_time`
    ///    （否则那个 FAILED 桶会被永久跳过）；
    /// 2. 否则有历史 ⇒ 从 `plan_time + cadence` 往后；
    /// 3. 否则（首次启动）⇒ 从本 tick 的桶开始。
    ///
    /// 然后被 `catch_up_window` 夹住（`<= 0` = 只认最新桶），并受 `max_plans_per_tick` 封顶。
    async fn every_plan_plans(
        &self,
        job: &Arc<JobSpec>,
        scope: &Scope,
        now: DateTime<Utc>,
        latest: DateTime<Utc>,
    ) -> SchedulerResult<Vec<DateTime<Utc>>> {
        if job.cadence <= Duration::zero() {
            // 不可达：无 hook 时 `validate` 已要求 cadence > 0；这里只是让「循环一定终止」
            // 一眼可见，而不是靠「上游保证」。
            return Ok(vec![latest]);
        }
        let info = db_ops::latest_plan(&self.repo, &job.name, scope).await?;
        let oldest_allowed = if job.catch_up_window <= Duration::zero() {
            latest
        } else {
            now - job.catch_up_window
        };
        let mut start = if info.retry_eligible(now) {
            info.plan_time
        } else if info.found {
            info.plan_time + job.cadence
        } else {
            latest
        };
        if start < oldest_allowed {
            start = floor_plan(oldest_allowed, job.cadence);
            if start < oldest_allowed {
                start += job.cadence;
            }
        }
        let mut plans = Vec::new();
        while start <= latest && plans.len() < job.max_plans_per_tick {
            plans.push(start);
            start += job.cadence;
        }
        Ok(plans)
    }

    /// 一个 `(job, scope, plan_time)`：认领 → 跑 handler（带心跳）→ 写终态。
    async fn process_plan(
        &self,
        job: &Arc<JobSpec>,
        scope: &Scope,
        plan_time: DateTime<Utc>,
        now: DateTime<Utc>,
    ) {
        let claim = match db_ops::try_claim(
            &self.repo,
            job,
            scope,
            plan_time,
            now,
            &self.opts.runner_id,
        )
        .await
        {
            Ok(claim) => claim,
            Err(err) => {
                tracing::warn!(
                    job = %job.name,
                    scope = %scope,
                    plan_time = %plan_time.to_rfc3339(),
                    error = %err,
                    "scheduler: claim error"
                );
                return;
            }
        };
        match claim {
            // 别人持有 / 已终态 / 退避未到 / 预算用尽 —— 一律静默（这是预期路径）。
            Claim::Conflicted => {}
            Claim::Won(claimed) => {
                self.run_claimed(job, scope, plan_time, ClaimKind::Won, claimed)
                    .await;
            }
            Claim::Stole(claimed) => {
                self.run_claimed(job, scope, plan_time, ClaimKind::Stole, claimed)
                    .await;
            }
        }
    }

    /// 跑一次已认领的租约并写终态（上游 `runClaimed`116）。
    async fn run_claimed(
        &self,
        job: &Arc<JobSpec>,
        scope: &Scope,
        plan_time: DateTime<Utc>,
        kind: ClaimKind,
        claimed: Claimed,
    ) {
        let lease = claimed.lease;
        if kind == ClaimKind::Stole {
            tracing::info!(
                job = %job.name,
                scope = %scope,
                plan_time = %plan_time.to_rfc3339(),
                attempt = claimed.attempt,
                execution_id = %lease.id,
                "scheduler: stole stale lease"
            );
        } else {
            tracing::info!(
                job = %job.name,
                scope = %scope,
                plan_time = %plan_time.to_rfc3339(),
                attempt = claimed.attempt,
                execution_id = %lease.id,
                "scheduler: claimed plan"
            );
        }

        // 心跳用**分离**的取消源：`run_timeout` 到期不该让续期停摆（上游同）。
        let hb_token = CancellationToken::new();
        let hb = tokio::spawn(run_heartbeats(
            self.repo.clone(),
            lease,
            job.heartbeat_interval,
            job.stale_timeout,
            hb_token.clone(),
        ));

        let input = HandlerInput {
            job: job.clone(),
            scope: scope.clone(),
            plan_time,
            attempt: claimed.attempt,
            runner_id: self.opts.runner_id.clone(),
            heartbeat: Heartbeat::new(self.repo.clone(), lease, job.stale_timeout),
        };
        let handler = job.handler.clone();
        let started = Instant::now();
        let task = tokio::spawn(async move { handler(input).await });
        let guard = AbortOnDrop(task.abort_handle());
        let outcome = match tokio::time::timeout(job.run_timeout, task).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(join_err)) if join_err.is_panic() => {
                Err(SchedulerError::HandlerPanic(join_err.to_string()))
            }
            Ok(Err(_join_err)) => Err(SchedulerError::Canceled),
            Err(_elapsed) => {
                // 超时：`timeout` 只是丢下 `JoinHandle`（detach），必须真的 abort 掉，
                // 否则 handler 会在「已经写了 FAILED」之后继续写业务行。
                guard.abort_now();
                Err(SchedulerError::RunTimeout)
            }
        };
        let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);

        // 终态写入之前先等心跳任务退出：保证「跑完之后不会再有心跳 UPDATE」。
        hb_token.cancel();
        let _ = hb.await;

        // 审计行的时间也用 DB 时钟；读不到才退本地（上游同："keeps the audit row honest
        // enough for a triage"）。
        let db_time = match self.repo.db_now().await {
            Ok(db_time) => db_time,
            Err(err) => {
                tracing::warn!(error = %err, "scheduler: db now failed, falling back to local clock");
                Utc::now()
            }
        };
        match outcome {
            Ok(result) => self.write_success(lease, db_time, duration_ms, &result).await,
            Err(err) => {
                self.write_failure(job, lease, claimed.attempt, db_time, duration_ms, &err)
                    .await;
            }
        }
    }

    /// 写 `SUCCESS` 终态（`LeaseLost` 是预期可能，不当错误记）。
    async fn write_success(
        &self,
        lease: Lease,
        db_time: DateTime<Utc>,
        duration_ms: i64,
        result: &HandlerResult,
    ) {
        match db_ops::finish_success(&self.repo, lease, db_time, duration_ms, result).await {
            Ok(()) => tracing::info!(
                duration_ms,
                rows_affected = result.rows_affected,
                "scheduler: handler succeeded"
            ),
            Err(SchedulerError::LeaseLost(_)) => tracing::warn!(
                duration_ms,
                "scheduler: terminal SUCCESS ignored, lease was stolen"
            ),
            Err(err) => tracing::error!(
                duration_ms,
                error = %err,
                "scheduler: write terminal SUCCESS"
            ),
        }
    }

    /// 写 `FAILED` 终态：**重试决策在这里**（退避时间 / 烧预算），与上游分工一致。
    async fn write_failure(
        &self,
        job: &JobSpec,
        lease: Lease,
        attempt: i32,
        db_time: DateTime<Utc>,
        duration_ms: i64,
        err: &SchedulerError,
    ) {
        let code = err.code().to_owned();
        let message = err.to_string();
        let write = FailureWrite {
            next_retry_at: next_retry_at(job, err, attempt, db_time),
            error_code: &code,
            error_msg: &message,
            attempt_override: match err.class() {
                ErrorClass::Permanent => Some(job.max_attempts),
                _ => None,
            },
        };
        let will_retry = write.next_retry_at.is_some();
        match db_ops::finish_failure(&self.repo, lease, db_time, duration_ms, &write).await {
            Ok(()) => tracing::warn!(
                duration_ms,
                error_code = %code,
                error = %message,
                will_retry,
                "scheduler: handler failed"
            ),
            Err(SchedulerError::LeaseLost(_)) => tracing::warn!(
                duration_ms,
                error = %message,
                "scheduler: terminal FAILED ignored, lease was stolen"
            ),
            Err(write_err) => tracing::error!(
                duration_ms,
                error = %write_err,
                handler_error = %message,
                "scheduler: write terminal FAILED"
            ),
        }
    }
}

/// 下一次重试时间（上游 `manager.go`：`if c.Attempt < job.MaxAttempts
/// { dbTime.Add(retryDelay(attempt)) }`）。
///
/// 本地加法：`Permanent` 一律 `None`，配合 `attempt_override = max_attempts` 让
/// [`mc_repos::scheduler::LatestPlanInfo::retry_eligible`] 立刻为假。
fn next_retry_at(
    job: &JobSpec,
    err: &SchedulerError,
    attempt: i32,
    db_time: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if err.class() == ErrorClass::Permanent {
        return None;
    }
    if attempt < job.max_attempts {
        Some(db_time + job.retry_delay(attempt))
    } else {
        None
    }
}

/// 把 handler 任务的寿命绑在本次 `run_claimed` 上：未来被 drop（关闭 / 取消）时
/// abort 掉还在跑的 handler，避免关闭后留一个还在写库的任务（DoD 的「不泄漏 task」）。
struct AbortOnDrop(AbortHandle);

impl AbortOnDrop {
    /// 立刻中止（超时路径用；`drop` 时原来的 abort 就变成 no-op）。
    fn abort_now(&self) {
        self.0.abort();
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 心跳循环（上游 `runHeartbeats`33）。
///
/// 独立 task + 独立取消源：`run_timeout` 到期 / 关闭时由 `run_claimed` 取消它。
/// 单次续期自带 5s 超时（上游同），失败只 warn —— 除了 `LeaseLost`：那说明租约已经不是
/// 我们的了，继续跳没有意义，直接退出（handler 那边会拿 `beat()` 的错误自己收尾）。
async fn run_heartbeats(
    repo: SchedulerRepo,
    lease: Lease,
    interval: StdDuration,
    stale_timeout: StdDuration,
    token: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // 上游 `time.NewTicker` 的第一拍在 interval **之后**；`tokio::time::interval` 的
    // 第一拍是「立刻」——刚认领时才写过 `heartbeat_at`，把这一拍吃掉。
    ticker.tick().await;
    loop {
        tokio::select! {
            () = token.cancelled() => return,
            _ = ticker.tick() => {
                let beat = tokio::time::timeout(
                    HEARTBEAT_TIMEOUT,
                    db_ops::heartbeat(&repo, lease, stale_timeout),
                )
                .await;
                match beat {
                    Ok(Ok(())) => {}
                    Ok(Err(SchedulerError::LeaseLost(_))) => {
                        tracing::warn!(
                            execution_id = %lease.id,
                            "scheduler: lease lost during heartbeat, runner should stop"
                        );
                        return;
                    }
                    Ok(Err(err)) => tracing::warn!(
                        execution_id = %lease.id,
                        error = %err,
                        "scheduler: heartbeat error"
                    ),
                    Err(_elapsed) => tracing::warn!(
                        execution_id = %lease.id,
                        "scheduler: heartbeat timed out"
                    ),
                }
            }
        }
    }
}

/// 后台调度循环的句柄：`shutdown()` 取消并**等循环退出**（graceful shutdown 的接入点）。
///
/// 直接 drop 句柄也会取消（不留下一个没人能停的循环），但不等于等它退出 ——
/// 要在关闭序列里保证「已经停了」，必须 `shutdown().await`。
pub struct SchedulerHandle {
    token: CancellationToken,
    join: Option<JoinHandle<()>>,
}

impl SchedulerHandle {
    /// 请求停止（不等循环退出）。
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// 是否已经请求过停止。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// 取消并等循环退出（在 `main.rs` 的关闭序列里 await 它）。
    ///
    /// 正在跑的那一轮会被取消：`run_once` 的 future 被 drop ⇒ handler 的
    /// [`AbortOnDrop`] 触发 ⇒ handler 任务被 abort。
    pub async fn shutdown(mut self) {
        self.token.cancel();
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

impl Drop for SchedulerHandle {
    fn drop(&mut self) {
        self.token.cancel();
    }
}
