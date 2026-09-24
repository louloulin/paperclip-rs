//! plugin hook 计划投递 job：按 `plugin_hook_schedule` 的 cron 把到期的格子变成一次签名调用。
//!
//! - **写者**：M6-8（`jobs/**` 本片只新增本文件 + `mod.rs` 的注册）。
//! - **上游**：`scheduler/jobs_plugin_hook.go`353（`PluginHookScheduleDispatchJob` +
//!   `pluginHookScheduleScopes` + `pluginHookSchedulePlans` + `pluginHookScheduleHandler` +
//!   `latestPluginHookOccurrence` + `pluginHookScheduleDeliveryID`）。
//!
//! # 与上游的逐条对应
//!
//! | 上游 | 本地 |
//! | --- | --- |
//! | `JobNamePluginHookScheduleDispatch` | [`JOB_NAME`]（持久化键，跨版本必须稳定） |
//! | `ScopeKindPluginHookSchedule` | [`SCOPE_KIND`] |
//! | `pluginHookScheduleScopeID` = `<id>:<generation>` | [`plugin_hook_scope_id`] / [`parse_scope_id`] |
//! | `pluginHookScheduleScopes` | [`catalog_scopes`]（一启用日程一 scope；开关关闭 ⇒ 空表） |
//! | `pluginHookSchedulePlans` | [`plans_hook`]（最新一格的折叠 + 重试复用同一 `plan_time`） |
//! | `latestPluginHookOccurrence` | [`latest_occurrence`]（分页枚举，长停机折成真正最新的那一格） |
//! | `pluginHookScheduleHandler` | [`schedule_handler`]（租约内一格；数据面全在端口里） |
//! | `pluginHookScheduleDeliveryID` | 端口侧（`psd_` + sha256，见 `mc-http` 的 `schedule_delivery_id`） |
//! | `advancePluginHookNextRun` | 端口侧的 `advance_next_run`（`WHERE id AND generation AND enabled`） |
//!
//! # 幂等是**内核**给的，不是本文件给的
//!
//! 「同一 schedule 桶只派发一次」的屏障是 `sys_cron_executions` 的唯一键
//! `(job_name, scope_kind, scope_id, plan_time)`（M5-7 的内核）：两个实例同时 tick 同一个
//! 格子，只有一方认领成功，另一方拿到 `Conflicted` 并**完全不进 handler**。本文件要做对的
//! 只有一件事：**同一个桶必须算出同一个 `plan_time`** —— 也就是「取最新一格 + 把它当锚」
//! （[`plans_hook`]），而不是每个 tick 拿 `now()` 现取一格。
//!
//! # 换代即换作用域
//!
//! scope id 里带 `generation`：改 cron / 停用重开都会换代（`mc-repos::plugin::hook` 的
//! `reconcile_tx` / `set_enabled_tx`），于是上一代的租约行成为不可变历史，新时间线从
//! `activated_at` 重新起算 —— 停机期间错过的格子**永不**补发（上游的注释就是这条）。
//!
//! # 数据面为什么是「端口」
//!
//! `mc-scheduler` 的依赖表里**没有 `sqlx`**（M5-0 冻结）⇒ 连 `PgPool` 都写不出来。本 job 的
//! 数据面（列日程 / 读单条 / 投递一格 / 推进展示列）由 [`PluginHookPort`] 注入；生产实现是
//! `apps/mc-server/src/scheduler/hook_port.rs`，它把活交给 `mc_http::routes::plugins::hooks_job`
//! 的引擎 —— 于是「限流 / 熔断 / `net:` 目的地检查 / 出站签名 / 调用记录」全仓只有一份。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use mc_autopilot::cron::{self, MAX_BETWEEN_OCCURRENCES};
use mc_repos::plugin::hook::HookScheduleRow;
use mc_repos::scheduler::LatestPlanInfo;

use crate::error::{SchedulerError, SchedulerResult};
use crate::spec::{
    CatchUpMode, Handler, HandlerInput, HandlerResult, JobSpec, PlansHook, Scope, ScopeProvider,
};

use super::{JsonObject, PortFuture};

/// job 名（`sys_cron_executions.job_name`）。**跨版本必须稳定**：改名会孤立历史审计行。
pub const JOB_NAME: &str = "plugin_hook_schedule_dispatch";

/// 作用域种类：每个**启用的**日程是一个 scope，`scope_id` 是 `<schedule_id>:<generation>`。
pub const SCOPE_KIND: &str = "plugin_hook_schedule";

/// 单次投递的尝试上限（上游 `pluginHookScheduleAttempts = 3`）。
pub const MAX_ATTEMPTS: i32 = 3;

/// 运行上限（上游 `RunTimeout: 45s`；必须小于 `stale_timeout`）。
pub const RUN_TIMEOUT_SECS: u64 = 45;

/// 陈旧上限（上游 `StaleTimeout: 90s`）。
pub const STALE_TIMEOUT_SECS: u64 = 90;

/// 心跳间隔（上游 `HeartbeatInterval: 15s`）。
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

/// 单 tick 只认一个计划（上游 `MaxPlansPerTick: 1`）：一个钩子的一格跑完再进下一格，
/// 免得一条慢端点把整条时间线搅乱。
pub const MAX_PLANS_PER_TICK: usize = 1;

/// 重试退避（上游 `RetryBackoff: []{30s, 2m}`）。
pub const RETRY_BACKOFF: [i64; 2] = [30, 120];

// ---------------------------------------------------------------------------
// 数据面端口
// ---------------------------------------------------------------------------

/// 一次投递的入参（`mc-http` 的 `dispatch_scheduled_hook` 的入参投影）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleDispatchRequest {
    /// `plugin_hook_schedule.id`。
    pub schedule_id: Uuid,
    /// scope 里的那一代（handler 已按作用域拆出；端口负责与库里的行比对）。
    pub generation: Uuid,
    /// 本格的规范 UTC 计划时刻。
    pub plan_time: DateTime<Utc>,
    /// 第几次尝试（`1..=MAX_ATTEMPTS`）。
    pub attempt: i32,
    /// 本轮是不是最后一次尝试（失败后必须推进 `next_run_at`，否则时间线卡死）。
    pub last_attempt: bool,
}

/// 一次投递的结论（上游 `HandlerResult` 的两个分支）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleOutcome {
    /// 真的发出去了（`delivery_id` 进审计 JSON）。
    Delivered {
        /// `psd_` 前缀的投递 id（同一次计划投递跨重试稳定）。
        delivery_id: String,
    },
    /// 没发：上游的 `skipped_reason`（`feature_disabled` / `schedule_not_found` /
    /// `schedule_generation_changed` / `installation_not_found` / `installation_disabled` /
    /// `manifest_changed` / `circuit_open`）。
    ///
    /// 跳过是**终态**：内核据此收尾，同一个 `plan_time` 不会再被认领。
    Skipped(String),
}

/// hook job 的数据面（生产实现：`apps/mc-server/src/scheduler/hook_port.rs`）。
pub trait PluginHookPort: Send + Sync {
    /// 上游 `ListEnabledPluginHookSchedules`：**启用的**日程（开关关闭时返回空表）。
    ///
    /// 生产实现不要按 `next_run_at` 过滤（它是展示列），也不要丢掉对 `plugins_v1` 的判定 ——
    /// 上游在 scope 枚举那一层就短路了，这里保持一致。
    fn list_enabled_schedules(&self) -> PortFuture<'_, SchedulerResult<Vec<HookScheduleRow>>>;

    /// 按 id 重读日程（上游 `GetPluginHookSchedule`）。未命中 ⇒ `Ok(None)`。
    fn load_schedule(
        &self,
        schedule_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<HookScheduleRow>>>;

    /// 投递一格（上游 `pluginHookScheduleHandler` 的实体：开关 → 日程 → 换代 → 安装 → manifest
    /// 一致性 → 熔断 → 签名调用 → 推进 `next_run_at`）。
    ///
    /// 生产实现必须把「失败且是最后一次尝试」也算成已收尾（推进展示列），否则整条时间线会
    /// 停在同一个 `plan_time` 上反复重试到永远。
    fn dispatch_schedule(
        &self,
        request: ScheduleDispatchRequest,
    ) -> PortFuture<'_, SchedulerResult<ScheduleOutcome>>;

    /// 推进**展示用**的 `next_run_at`（上游 `advancePluginHookNextRun`）。返回影响行数。
    ///
    /// 与 [`Self::dispatch_schedule`] 的重叠是**故意的**：投递成功/终结时端口自己就推进了，
    /// 这个方法留给「跳过」分支（熔断）与重试预算耗尽两条路径。
    fn advance_next_run(
        &self,
        schedule_id: Uuid,
        generation: Uuid,
        plan_time: DateTime<Utc>,
    ) -> PortFuture<'_, SchedulerResult<u64>>;
}

// ---------------------------------------------------------------------------
// 作用域与计划
// ---------------------------------------------------------------------------

/// 一格的日志作用域 id：`<schedule_id>:<generation>`（上游 `pluginHookScheduleScopeID`）。
#[must_use]
pub fn plugin_hook_scope_id(schedule_id: Uuid, generation: Uuid) -> String {
    format!("{schedule_id}:{generation}")
}

/// 反向解析（上游 `parsePluginHookScheduleScopeID`）：形状不对 ⇒ 错误（handler 会记一行
/// `FAILED`，而不是静默少跑）。上游要求**恰好**两段 —— 这里也一样。
///
/// # Errors
///
/// [`SchedulerError::Handler`]（作用域不是本 job 造出来的形状）。
pub fn parse_scope_id(scope_id: &str) -> SchedulerResult<(Uuid, Uuid)> {
    let error = || SchedulerError::Handler("expected schedule:generation scope".to_owned());
    let (schedule, generation) = scope_id.split_once(':').ok_or_else(error)?;
    if generation.contains(':') {
        return Err(error());
    }
    let schedule = Uuid::parse_str(schedule).map_err(|_| error())?;
    let generation = Uuid::parse_str(generation).map_err(|_| error())?;
    Ok((schedule, generation))
}

/// scope id → 日程行的小缓存（上游 `pluginHookScheduleCache` 的 `schedules` 半边）。
///
/// 计划钩子拿到的是 `Scope`（只有 id），投递的那一格要 `cron/tz/activated_at` ⇒ 必须在
/// scope 枚举那一遍就把行留下。缓存**按 tick 替换**（`replace`）：日程被删/改代之后，
/// 旧条目随之消失，plan 钩子对未知 scope 一律回空表。
#[derive(Default)]
pub struct ScheduleCache {
    inner: Mutex<HashMap<String, HookScheduleRow>>,
    coalesced: Mutex<HashMap<String, usize>>,
}

impl ScheduleCache {
    pub fn replace(&self, rows: &[HookScheduleRow]) {
        let mut active: HashMap<String, HookScheduleRow> = HashMap::with_capacity(rows.len());
        for row in rows {
            active.insert(row.scope_id(), row.clone());
        }
        {
            let mut coalesced = self
                .coalesced
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // 已不在表里的 scope 的折叠计数一并丢掉（上游 `replace` 的同一步）。
            coalesced.retain(|key, _| {
                key.split('/')
                    .next()
                    .is_some_and(|scope| active.contains_key(scope))
            });
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *inner = active;
    }

    pub fn get(&self, scope_id: &str) -> Option<HookScheduleRow> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(scope_id)
            .cloned()
    }

    /// 记下这一格**被折叠掉**的格子数（`count - 1`）。
    pub fn set_coalesced(&self, scope_id: &str, plan_time: DateTime<Utc>, count: usize) {
        let mut coalesced = self
            .coalesced
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prefix = format!("{scope_id}/");
        coalesced.retain(|key, _| !key.starts_with(&prefix));
        coalesced.insert(format!("{prefix}{}", plan_time.to_rfc3339()), count);
    }

    /// 取出并清掉这一格的折叠计数（只能被取一次：成功/熔断两条路径都取）。
    pub fn take_coalesced(&self, scope_id: &str, plan_time: DateTime<Utc>) -> usize {
        let key = format!("{scope_id}/{}", plan_time.to_rfc3339());
        self.coalesced
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key)
            .unwrap_or(0)
    }
}

/// 作用域提供者（上游 `pluginHookScheduleScopes`）：一启用日程一 scope。
///
/// 列出来的东西会被缓存，[`plans_hook`] 与 [`schedule_handler`] 都从缓存读 —— 因此三个
/// 回调看到的是**同一次**枚举结果（上游同）。
pub fn catalog_scopes(port: Arc<dyn PluginHookPort>, cache: Arc<ScheduleCache>) -> ScopeProvider {
    Arc::new(move |_now: DateTime<Utc>| {
        let port = port.clone();
        let cache = cache.clone();
        Box::pin(async move {
            let rows = port.list_enabled_schedules().await?;
            cache.replace(&rows);
            let scopes: Vec<Scope> = rows
                .iter()
                .map(|row| Scope::new(SCOPE_KIND, row.scope_id()))
                .collect();
            Ok(scopes)
        })
    })
}

/// 计划钩子（上游 `pluginHookSchedulePlans`）。
///
/// 三条分支与上游逐条对应：
///
/// 1. 最新一行是 `RUNNING` ⇒ 本 tick 无计划（同一代内**严格串行**）；
/// 2. 最新一行是 `FAILED` 且还有重试预算 ⇒ 复用**同一个** `plan_time`
///    （`retry_eligible` 为假时也无计划 —— 等退避到点或烧完预算）；
/// 3. 否则从 `latest.plan_time`（没有历史则 `activated_at`）往后枚举到 `now`，
///    只取**最新**那一格，并把折叠掉的格子数记进缓存。
pub fn plans_hook(cache: Arc<ScheduleCache>) -> PlansHook {
    Arc::new(
        move |scope: Scope, now: DateTime<Utc>, latest: LatestPlanInfo| {
            let cache = cache.clone();
            Box::pin(async move {
                let Some(row) = cache.get(&scope.id) else {
                    return Ok(Vec::new());
                };
                if latest.found {
                    match latest.status {
                        mc_repos::scheduler::ExecutionStatus::Running => return Ok(Vec::new()),
                        mc_repos::scheduler::ExecutionStatus::Failed => {
                            if latest.attempt < latest.max_attempts {
                                if latest.retry_eligible(now) {
                                    return Ok(vec![latest.plan_time]);
                                }
                                return Ok(Vec::new());
                            }
                        }
                        mc_repos::scheduler::ExecutionStatus::Success => {}
                    }
                }
                let after = if latest.found {
                    latest.plan_time
                } else {
                    row.activated_at
                };
                let (plan, count) =
                    latest_occurrence(&row.cron_expression, &row.timezone, after, now).map_err(
                        |error| {
                            SchedulerError::Handler(format!(
                                "plugin schedule plans for {}: {error}",
                                scope.id
                            ))
                        },
                    )?;
                let Some(plan) = plan else {
                    return Ok(Vec::new());
                };
                cache.set_coalesced(&scope.id, plan, count.saturating_sub(1));
                Ok(vec![plan])
            })
        },
    )
}

/// 上游 `latestPluginHookOccurrence`：半开区间 `(after, until]` 里**最新**的一格与总格子数。
///
/// 分页枚举（每页 `MAX_BETWEEN_OCCURRENCES` = 1024，与上游 `pluginHookSchedulePageSize` 同值）：
/// 一次长停机不该在第一页就停下、把中间那些格子当「最新」派发出去。
///
/// # Errors
///
/// cron 表达式或时区非法（⇒ handler 记一行 `FAILED`，比跳过更容易被发现）。
pub fn latest_occurrence(
    expression: &str,
    timezone: &str,
    after: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<(Option<DateTime<Utc>>, usize), cron::CronError> {
    let mut latest: Option<DateTime<Utc>> = None;
    let mut count = 0usize;
    let mut cursor = after;
    loop {
        let occurrences = cron::next_occurrences_between_utc(expression, timezone, cursor, until)?;
        if occurrences.is_empty() {
            return Ok((latest, count));
        }
        count += occurrences.len();
        let last = occurrences[occurrences.len() - 1];
        latest = Some(last);
        if occurrences.len() < MAX_BETWEEN_OCCURRENCES || last >= until {
            return Ok((latest, count));
        }
        cursor = last;
    }
}

/// 业务 handler（上游 `pluginHookScheduleHandler`）。
///
/// 纯逻辑只有三件事：解析 scope、算 `last_attempt`、把结论折成审计 JSON；其余全在端口里。
pub fn schedule_handler(port: Arc<dyn PluginHookPort>, cache: Arc<ScheduleCache>) -> Handler {
    Arc::new(move |input: HandlerInput| {
        let port = port.clone();
        let cache = cache.clone();
        Box::pin(async move {
            run_schedule_once(
                &port,
                &cache,
                &input.scope.id,
                input.plan_time,
                input.attempt,
                input.job.max_attempts,
            )
            .await
        })
    })
}

/// 一格投递的**纯逻辑**（`schedule_handler` 的本体）。
///
/// 与闭包拆开是因为它不依赖 `HandlerInput`（那个结构体要求一个活着的 `Heartbeat`）——
/// 于是「一格的审计 JSON 长什么样」可以在无库用例里逐字断言。
///
/// # Errors
///
/// 作用域形状不对 / 端口报错。
pub async fn run_schedule_once(
    port: &Arc<dyn PluginHookPort>,
    cache: &ScheduleCache,
    scope_id: &str,
    plan_time: DateTime<Utc>,
    attempt: i32,
    max_attempts: i32,
) -> SchedulerResult<HandlerResult> {
    let (schedule_id, generation) = parse_scope_id(scope_id)?;
    let attempt = attempt.max(1);
    let request = ScheduleDispatchRequest {
        schedule_id,
        generation,
        plan_time,
        attempt,
        last_attempt: attempt >= max_attempts.max(1),
    };
    let coalesced = cache.take_coalesced(scope_id, plan_time);
    // 折叠数是内存计数（`usize`），审计列是 `i64`；两者都不可能接近上限，但仍显式收敛，
    // 免得 `as` 在 64 位目标上被 clippy 判为可能截断。
    let coalesced = i64::try_from(coalesced).unwrap_or(i64::MAX);
    let lag_ms = (Utc::now() - plan_time).num_milliseconds().max(0);
    match port.dispatch_schedule(request).await? {
        ScheduleOutcome::Delivered { delivery_id } => {
            // 计数走 `number`（上游的 `map[string]any` 里它们是 **int**，落 `result::jsonb`
            // 后必须还是 JSON 数字 —— 字符串计数在审计查询里排不了序）。
            let mut json = JsonObject::new();
            json.text("delivery_id", &delivery_id);
            json.number("coalesced_occurrences", coalesced);
            json.number("dispatch_lag_ms", lag_ms);
            Ok(HandlerResult {
                rows_affected: 1,
                result_json: Some(json.finish()),
            })
        }
        ScheduleOutcome::Skipped(reason) => {
            let mut json = JsonObject::new();
            json.text("skipped_reason", &reason);
            json.number("coalesced_occurrences", coalesced);
            json.number("dispatch_lag_ms", lag_ms);
            Ok(HandlerResult {
                rows_affected: 0,
                result_json: Some(json.finish()),
            })
        }
    }
}

/// 组装 job（上游 `PluginHookScheduleDispatchJob` 的规格逐字照抄）。
#[must_use]
pub fn job(port: Arc<dyn PluginHookPort>) -> JobSpec {
    let cache = Arc::new(ScheduleCache::default());
    JobSpec::new(
        JOB_NAME,
        // 有 `plans_for_scope` 时 cadence 只当审计用；上游同（写零值会被 `validate` 放行，
        // 但审计里一个 0 不好看，故给一分钟）。
        Duration::minutes(1),
        catalog_scopes(port.clone(), cache.clone()),
        schedule_handler(port, cache.clone()),
    )
    .with_catch_up(
        CatchUpMode::LatestOnly,
        Duration::zero(),
        MAX_PLANS_PER_TICK,
    )
    .with_timing(
        std::time::Duration::from_secs(RUN_TIMEOUT_SECS),
        std::time::Duration::from_secs(STALE_TIMEOUT_SECS),
        std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS),
    )
    .with_retry(
        MAX_ATTEMPTS,
        RETRY_BACKOFF
            .iter()
            .map(|seconds| Duration::seconds(*seconds))
            .collect(),
    )
    .with_allow_stale_reentry(true)
    .with_plans_for_scope(plans_hook(cache))
}
