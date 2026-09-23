//! issue wakeup 派发 job：每 30s 把到期的 wakeup 规则变成队列行。
//!
//! - **写者**：M5-8（`LUM-1571`）。
//! - **上游**：`scheduler/jobs_issue_wakeup.go`21（22）是**规格**（`Name` /
//!   `Cadence: 30s` / `CatchUpMode: latest_only` / `CatchUpWindow: 1h` / `RunTimeout: 45s` /
//!   `StaleTimeout: 1min` / `HeartbeatInterval: 10s` / `AllowStaleReentry: true` /
//!   `MaxAttempts: 1` / `Scopes: StaticScopes(ScopeGlobal)`）；
//!   `service/issue_wakeup.go:513` 的 `Tick` 才是**函数体**。
//! - **去重 / 认领 / 围栏全在 M5-6**：本 job 只做「取一批 → 逐条跑 → 收尾」的循环和预算。
//!   收据（receipt）是 wakeup 侧的幂等键，`sys_cron_executions` 是本 job 的租约键，两层独立。
//!
//! # 与上游 `Tick` 的逐行对应
//!
//! | 上游 `Tick` | 本地 |
//! | --- | --- |
//! | `DeleteExpiredWakeupReceipts`（2s 预算，错误只收不抛） | [`WakeupDispatchPort::tick_candidates`]（M5-6 在同一函数里先清后列） |
//! | `ListReadyWakeups`（失败 ⇒ 整 tick 立刻 `return err`） | 同上，`?` 直接冒泡 |
//! | 逐行 `ctx.Err()` 检查（批次预算用尽 ⇒ join 后返回） | 外层 `run_timeout=45s` 由内核 `timeout` 掉整个 handler（见偏差 D5） |
//! | `s.dispatch(ctx, w)`（**含** 2s 单规则预算） | [`WakeupDispatchPort::dispatch_wakeup`] + [`PER_RULE_BUDGET`] |
//! | 失败 ⇒ `NoteWakeupFailure`（100ms 预算，错误忽略） | [`WakeupDispatchPort::note_dispatch_failure`] + [`OUTCOME_BUDGET`] |
//! | 无论成败 `TouchWakeupDispatch`（100ms 预算，错误收集） | [`WakeupDispatchPort::touch_dispatch`] + [`OUTCOME_BUDGET`] |
//! | `errors.Join(errs...)` | `Err(SchedulerError::Handler(join))` ⇒ 写一行 `FAILED` 审计（`max_attempts=1` ⇒ 不重试，同上游） |
//!
//! 失败**不**逐条重试、也不阻断后续行：上游的设计意图是「一批里一条坏规则不拖住其它规则」，
//! 而每行的权威状态（收据 / `last_error` / 调度推进）已经落在 wakeup 侧的表里。
//!
//! # 数据面为什么是「端口」
//!
//! M5-6 在 `mc-autopilot/src/wakeup/dispatch.rs` 的模块头把上游 `dispatch` **刻意切成两半**
//! （`plan_dispatch` + `consume_dispatch`），并把「建队列行 + 凭据 overlay + `task.queued` 广播」
//! 明确判给 M5-8 —— 因为那几步要 `sqlx` / realtime 出口。而 `mc-scheduler` 的依赖表里
//! **没有 `sqlx`、没有 `mc-ws`**（M5-0 冻结，本片不得加）⇒ 连 `PgPool` / `PgConnection`
//! 都写不出来。于是这里只保留**循环、预算、错误聚合**（纯逻辑，可单测），
//! 数据面由 [`WakeupDispatchPort`] 注入；生产实现待 P0 接线（`apps/mc-server` 缺
//! `mc-scheduler` 依赖边，`docs/48` §7.1）。

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Duration;
use uuid::Uuid;

use mc_repos::wakeup::WakeupRow;

use crate::error::{SchedulerError, SchedulerResult};
use crate::spec::{global_scopes, CatchUpMode, Handler, HandlerInput, HandlerResult, JobSpec};

use super::{JsonObject, PortFuture};

/// job 名（`sys_cron_executions.job_name`）。
pub const JOB_NAME: &str = "issue_wakeup_dispatch";

/// 单规则预算：**含**凭据解析与全部 SQL（上游 `dispatch` 的 `2*time.Second` 注释：
/// 即使 100 条规则互相争锁也只吃掉 45s 批次预算的 ~5s，剩下留给别的 issue）。
pub const PER_RULE_BUDGET: StdDuration = StdDuration::from_secs(2);

/// 收尾写入（`last_error` / `touch`）的预算：**不能**让一条忙碌的规则把整批拖住
/// （上游的 `100*time.Millisecond`；注意它用的是**批次** context，不是已经过期的单规则 context）。
pub const OUTCOME_BUDGET: StdDuration = StdDuration::from_millis(100);

/// 一条规则本轮的结论（上游 `dispatch` 的四种收场）。
///
/// 生产实现负责把 `mc_autopilot::wakeup::dispatch::DispatchPlan` 映射到这里：
/// `Settled` ← `Plan::Settled`、`Removed` ← `Plan::Removed`、`Waiting` ← `Plan::Waiting{..}`、
/// `Dispatched` ← `Plan::Dispatch{..}` 走完「建队列行 + `consume_dispatch` + 提交」之后。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupOutcome {
    /// 事务内已收尾（调度推进 / 关停 / 版本失配 / 没有可派收据）⇒ `COMMIT`。
    Settled,
    /// issue 行已不在 ⇒ 收据与 wakeup 已被级联清掉。
    Removed,
    /// 已有一条 `dispatched` 的 run 在等认领 / 恢复：只推进了调度。
    Waiting,
    /// 本轮真的写了队列行（新建或合并证据）。
    Dispatched,
}

/// 数据面端口（生产实现的 7 步事务顺序写在 [`WakeupDispatchPort::dispatch_wakeup`] 上）。
pub trait WakeupDispatchPort: Send + Sync {
    /// `Tick` 的前半：清过期收据 + 取本轮要处理的规则。
    ///
    /// 生产实现必须调用 `mc_autopilot::wakeup::dispatch::tick_candidates`（它内部已按
    /// 7 天保留 + 1000 条一批清收据，再取 `ready_wakeups`）—— **不要**在这里自己重写
    /// `ready_wakeups` 的筛选条件：哪些规则算「就绪」是 M5-6 的契约。
    fn tick_candidates(&self) -> PortFuture<'_, SchedulerResult<Vec<WakeupRow>>>;

    /// 单条规则的完整派发（上游 `s.dispatch(ctx, w)`）。
    ///
    /// 生产实现必须**照 `mc-autopilot/src/wakeup/dispatch.rs` 模块头的 7 步**做，
    /// 且全部在**同一个事务**内：
    ///
    /// 1. 事务外先解析凭据 overlay（`buildRuntimeMCPOverlay`）；
    /// 2. `BEGIN` + `SET LOCAL lock_timeout`（由 `plan_dispatch` 内部设置，50ms）；
    /// 3. `plan_dispatch(conn, &wakeup)`（锁序、围栏、调度推进、证据合并）；
    /// 4. `Dispatch{previous_task: None}` ⇒ `guardIssueNotInTriage` + 建队列行（带
    ///    `context.wakeup_id` / `wakeup_revision` / `wakeup_evidence` 与 `handoff_note`）；
    ///    `Some` ⇒ 合并证据进那条 `queued` 行；
    /// 5. `consume_dispatch(conn, &plan, task_id)`（认领收据 + 写回 `enabled`/`next_fire_at`）；
    /// 6. `COMMIT`；
    /// 7. 提交**之后**广播 `task.queued` + 通知 runtime。
    ///
    /// 返回 [`WakeupOutcome::Dispatched`] 当且仅当第 4 步真的写了队列行。
    fn dispatch_wakeup(&self, wakeup: &WakeupRow)
        -> PortFuture<'_, SchedulerResult<WakeupOutcome>>;

    /// 失败分支：写 `last_error`（上游 `NoteWakeupFailure`）。
    ///
    /// 生产实现调 `mc_autopilot::wakeup::dispatch::note_dispatch_failure`（内部按 500 rune 截断）。
    fn note_dispatch_failure(
        &self,
        wakeup_id: Uuid,
        error: &str,
    ) -> PortFuture<'_, SchedulerResult<()>>;

    /// 无论成败都调：让「这条规则刚被看过」在 UI 上可见（上游 `TouchWakeupDispatch`）。
    fn touch_dispatch(&self, wakeup_id: Uuid) -> PortFuture<'_, SchedulerResult<()>>;
}

/// 一轮 tick 的计数（**加法**：上游 `Tick` 只返回 error，没有回传结果；见偏差 D2）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WakeupTickSummary {
    /// `ready_wakeups` 给出的候选条数。
    pub candidates: usize,
    /// 真的写了队列行的条数。
    pub dispatched: usize,
    /// 已有 run 在等认领的条数。
    pub waiting: usize,
    /// 事务内已收尾的条数。
    pub settled: usize,
    /// issue 已不在 ⇒ 被级联清掉的条数。
    pub removed: usize,
    /// 出错/超时的条数（`dispatched + waiting + settled + removed + errors` 应等于 `candidates`）。
    pub errors: usize,
}

impl WakeupTickSummary {
    fn record(&mut self, outcome: WakeupOutcome) {
        match outcome {
            WakeupOutcome::Dispatched => self.dispatched += 1,
            WakeupOutcome::Waiting => self.waiting += 1,
            WakeupOutcome::Settled => self.settled += 1,
            WakeupOutcome::Removed => self.removed += 1,
        }
    }

    /// 审计行的 `result_json`。
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut json = JsonObject::new();
        json.number("candidates", to_i64(self.candidates))
            .number("dispatched", to_i64(self.dispatched))
            .number("waiting", to_i64(self.waiting))
            .number("settled", to_i64(self.settled))
            .number("removed", to_i64(self.removed))
            .number("errors", to_i64(self.errors));
        json.finish()
    }
}

/// `usize` → `i64`（计数不会溢出；溢出时钳到 `i64::MAX` 而不是回绕）。
fn to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// 业务 handler（上游 `wakeupHandler`：`return HandlerResult{}, dispatcher.Tick(ctx)`）。
#[must_use]
pub fn wakeup_handler(port: Arc<dyn WakeupDispatchPort>) -> Handler {
    Arc::new(move |input: HandlerInput| {
        let port = port.clone();
        Box::pin(async move {
            // 上游这个 handler 不读 tick 上下文（`Tick(ctx)` 只用 ctx 做取消/超时）⇒ 本地也
            // 只用它写日志。批次超时由内核的 `run_timeout` 落在外层，不在这里重复一次。
            let result = run_tick(&port).await;
            if let Err(err) = &result {
                tracing::warn!(
                    job = %input.job.name,
                    runner = %input.runner_id,
                    error = %err,
                    "issue wakeup: tick finished with errors"
                );
            }
            result
        })
    })
}

/// 一轮 tick（上游 `Tick`：清收据 → 取一批 → 逐条派发 → 逐条收尾 → `errors.Join`）。
///
/// 预算用上游常量（[`PER_RULE_BUDGET`] / [`OUTCOME_BUDGET`]）。
pub async fn run_tick(port: &Arc<dyn WakeupDispatchPort>) -> SchedulerResult<HandlerResult> {
    run_tick_with_budgets(port, PER_RULE_BUDGET, OUTCOME_BUDGET).await
}

/// [`run_tick`] 的**可注入预算**版本。
///
/// 存在的唯一理由是让超时分支可测：上游把 2s / 100ms 写成常量，本地要验「单规则超时会写
/// `last_error` 且不拖住后续行」就只能等 2s 真实墙钟（`mc-scheduler` 的 `tokio` feature 集里
/// 没有 `test-util`，用不了 `start_paused`）⇒ 把预算开成参数，测试压到毫秒级。见偏差 D6。
pub async fn run_tick_with_budgets(
    port: &Arc<dyn WakeupDispatchPort>,
    per_rule_budget: StdDuration,
    outcome_budget: StdDuration,
) -> SchedulerResult<HandlerResult> {
    let candidates = port.tick_candidates().await?;
    let mut summary = WakeupTickSummary {
        candidates: candidates.len(),
        ..WakeupTickSummary::default()
    };
    let mut errors: Vec<String> = Vec::new();

    for wakeup in &candidates {
        // 单规则：2s（含凭据 + SQL + 锁）。
        match tokio::time::timeout(per_rule_budget, port.dispatch_wakeup(wakeup)).await {
            Ok(Ok(outcome)) => summary.record(outcome),
            Ok(Err(err)) => {
                summary.errors += 1;
                let message = format!("wakeup {}: {err}", wakeup.id);
                errors.push(message.clone());
                // 上游 `_ =`：这条写入失败**不再**追加错误（否则一条坏规则会贡献两条噪音）。
                let _ = tokio::time::timeout(
                    outcome_budget,
                    port.note_dispatch_failure(wakeup.id, &message),
                )
                .await;
            }
            Err(_elapsed) => {
                summary.errors += 1;
                let message = format!(
                    "wakeup {}: dispatch exceeded {}ms budget",
                    wakeup.id,
                    per_rule_budget.as_millis()
                );
                errors.push(message.clone());
                let _ = tokio::time::timeout(
                    outcome_budget,
                    port.note_dispatch_failure(wakeup.id, &message),
                )
                .await;
            }
        }

        // 无论成败都 touch；只有它的失败进 errs（上游同）。
        match tokio::time::timeout(outcome_budget, port.touch_dispatch(wakeup.id)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => errors.push(format!("wakeup {}: touch dispatch: {err}", wakeup.id)),
            Err(_elapsed) => errors.push(format!(
                "wakeup {}: touch dispatch exceeded {}ms budget",
                wakeup.id,
                outcome_budget.as_millis()
            )),
        }
    }

    if errors.is_empty() {
        return Ok(HandlerResult {
            // 与上游的 `HandlerResult{}`（0 行）不同：这里报「本 tick 真写了几条队列行」。
            // 纯审计列，写错也不会重跑业务（`rows_affected` 没有任何控制流作用）⇒ 偏差 D2。
            rows_affected: to_i64(summary.dispatched),
            result_json: Some(summary.to_json()),
        });
    }
    Err(SchedulerError::Handler(errors.join("; ")))
}

/// 构造 job 规格（上游 `IssueWakeupDispatchJob`）。
///
/// 时间预算与上游逐一对应；`max_attempts = 1` + 空退避表 = **不重试**（上游 `MaxAttempts: 1`）：
/// 一轮失败的代价是「这批规则晚 30s 再来」，而重试会让同一批在租约表里反复出现。
#[must_use]
pub fn job(port: Arc<dyn WakeupDispatchPort>) -> JobSpec {
    JobSpec::new(
        JOB_NAME,
        Duration::seconds(30),
        global_scopes(),
        wakeup_handler(port),
    )
    .with_catch_up(CatchUpMode::LatestOnly, Duration::hours(1), 1)
    .with_timing(
        StdDuration::from_secs(45),
        StdDuration::from_secs(60),
        StdDuration::from_secs(10),
    )
    .with_retry(1, Vec::new())
    .with_allow_stale_reentry(true)
}
