//! autopilot 调度 job：按 `next_run_at` 把到期的 trigger 变成 run。
//!
//! - **写者**：M5-8（`LUM-1571`）。
//! - **上游**：`scheduler/jobs_autopilot.go`448（449）—— `autopilotScopes`66 +
//!   `autopilotPlansForScope`84 + `isAutopilotSchedulePlanStale`9 + `advancedNextRun`15。
//! - **难点**：按 **workspace × 时区**分桶算 `plan_time`，并且要处理「慢 tick 之后 plan 已过期」
//!   （[`is_autopilot_schedule_plan_stale`]）；`autopilot_run.planned_at`（`124`）是分桶的证据列。
//! - **cron 解析**：调用 `mc_autopilot::cron` 的解析器（**不**在 job 里重写）。
//! - **写行**：结果写入 `agent_task_queue` / `autopilot_run` 的动作在 M5-4 的
//!   `AutopilotDispatcher::dispatch_for_plan` 里（本片只调用它，不重复实现）；**不碰 daemon 协议**（R9）。
//!
//! # 三个作用面（与上游一一对应）
//!
//! | 上游 | 本地 | 说明 |
//! | --- | --- | --- |
//! | `autopilotScopes`66 | [`catalog_scopes`] | 每 tick 重列「可调度的 schedule trigger」，一 trigger 一 scope |
//! | `autopilotPlansForScope`84 | [`plans_hook`] | 用 trigger 自己的 cron + 时区算 `plan_time`，只留最近一格 |
//! | `autopilotHandler` | [`schedule_handler`] | tick 中重读 trigger + autopilot，过期/停用立即生效 |
//!
//! **每个 trigger 就是一把租约**（`scope_kind = "autopilot_trigger"`、`scope_id = trigger.id`）
//! ⇒ 「同一 trigger 同一 `plan_time` 两实例不双跑」由 `sys_cron_executions` 的唯一键保证；
//! 「同一 `(trigger, planned_at)` 不产生第二条 run」由 M5-4 的幂等快路径
//! （`uq_autopilot_run_trigger_planned`）保证。两层叠起来，陈旧租约被偷 + 重入也只会复用同一条 run。
//!
//! # 数据面为什么是「端口」（本片的**已知缺口**）
//!
//! 上游 `autopilotScopes` / `autopilotHandler` 直接拿 `*db.Queries` 打 4 条 SQL
//! （`ListSchedulableAutopilotTriggers` / `GetAutopilotTrigger` / `Autopilot` /
//! `AdvanceTriggerNextRun` + `TouchAutopilotTriggerFiredAt`）。本地这三条约束把 SQL 挡在本文件外：
//!
//! 1. `mc-scheduler` 的依赖表**没有 `sqlx`**（M5-0 冻结）⇒ 本 crate 连 `PgPool` 都写不出来；
//! 2. `mc-repos` 里**没有**这四个查询/写入（M5-1..M5-4 只交付了「单机详情 + 列表」读面与
//!    `update_trigger` 的通用补丁），而 `mc-repos` 不在本片写集内；
//! 3. 写集只到 `jobs/**`。
//!
//! ⇒ 本片把数据面抽成两个**窄端口**（上游本来就为单测定义了 `AutopilotScheduleDispatcher`
//! 接口，本地只是把「queries 也要可替换」一并做掉）：
//!
//! * [`AutopilotSchedulePort`]：4 个读/展示列写入（**生产实现缺位，登记为 P0 缺口**）；
//! * [`ScheduleDispatch`]：派发面，**已有真实现** —— [`ScheduleDispatch`] 对
//!   `mc_autopilot::dispatch::AutopilotDispatcher` 的实现就在本文件里（编译期受检）。
//!
//! 端口的**全部业务语义都落在本文件**：分桶、锚点选择、过期判定、重试面、展示列推进、
//! tick 内重读。所以本片不是「空壳」——真库证据见 `tests/jobs_autopilot.rs`（租约 + 端口）。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use mc_autopilot::cron::{self, CronError};
use mc_autopilot::dispatch::{AutopilotDispatcher, DispatchError, DispatchRequest};
use mc_core::autopilot::RunSource;
use mc_repos::autopilot::{AutopilotRow, AutopilotTriggerRow};
use mc_repos::scheduler::LatestPlanInfo;

use crate::error::{SchedulerError, SchedulerResult};
use crate::spec::{
    CatchUpMode, Handler, HandlerInput, HandlerResult, JobSpec, PlansHook, Scope, ScopeProvider,
};

use super::{json_object, PortFuture};

/// job 名（`sys_cron_executions.job_name`）。**跨版本必须稳定**：改名会孤立历史审计行。
pub const JOB_NAME: &str = "autopilot_schedule_dispatch";

/// 作用域种类：每个**启用的 schedule trigger** 是一个 scope，`scope_id` 是 trigger 的 uuid。
pub const SCOPE_KIND: &str = "autopilot_trigger";

/// trigger 的 `kind` 字面量（可调度的那种）。`mc-repos` 只有 `TRIGGER_KIND_WEBHOOK`。
pub const TRIGGER_KIND_SCHEDULE: &str = "schedule";

/// `autopilot_trigger.timezone` 为 `NULL` / 空时的兜底（= `mc_autopilot::trigger` 的默认值）。
pub const DEFAULT_TIMEZONE: &str = "UTC";

/// 一次 plan 的**可接受迟到上限**：超过它就不补发，等下一个配置槽位。
///
/// 正常 tick 抖动是几十秒；更久的迟到属于「停机追赶」，在任意时刻突然开火会吓到用户
/// ⇒ 宁可跳过（上游 `maxAutopilotScheduleLateness` 的注释就是这个判据）。
pub const MAX_LATENESS_MINUTES: i64 = 5;

/// 冷启动枚举的历史回看上限（上游 `autopilotPlansForScope` 的 `replayWindow`）。
pub const REPLAY_WINDOW_HOURS: i64 = 24;

// ---------------------------------------------------------------------------
// 数据面端口
// ---------------------------------------------------------------------------

/// trigger 行 + 展示列的**只读写面**（上游 `*db.Queries` 的 5 个调用点）。
///
/// 生产实现必须打这几条 SQL（缺口登记在片尾与交付注释里）：
///
/// 1. `list_schedulable_triggers` = `ListSchedulableAutopilotTriggers`：
///    `JOIN autopilot a`，过滤 `t.enabled AND t.kind='schedule' AND t.cron_expression <> ''
///    AND a.status='active'`。**过滤口径必须在这里**——本片的分桶逻辑假设「列出来的都是可跑的」。
/// 2. `load_trigger` = `GetAutopilotTrigger`：查不到 ⇒ `Ok(None)`（上游 `pgx.ErrNoRows`）。
/// 3. `load_autopilot` = `GetAutopilot`：同样 `Ok(None)`。
/// 4. `advance_next_run` = `AdvanceTriggerNextRun`（`next_run_at = $2`）：返回影响行数。
/// 5. `touch_fired_at` = `TouchAutopilotTriggerFiredAt`（只推 `last_fired_at`）：返回影响行数。
pub trait AutopilotSchedulePort: Send + Sync {
    /// 本 tick 的可调度 trigger（过滤口径见 trait 文档第 1 条）。
    fn list_schedulable_triggers(
        &self,
    ) -> PortFuture<'_, SchedulerResult<Vec<AutopilotTriggerRow>>>;

    /// 按 id 重读 trigger（不在本工作区过滤 —— 上游同，scope 里的 id 本来就来自第 1 条）。
    fn load_trigger(
        &self,
        trigger_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<AutopilotTriggerRow>>>;

    /// 按 id 重读 autopilot。
    fn load_autopilot(
        &self,
        autopilot_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<AutopilotRow>>>;

    /// 把**展示用**的 `next_run_at` 推到的下一格。
    fn advance_next_run(
        &self,
        trigger_id: Uuid,
        next_run_at: Option<DateTime<Utc>>,
    ) -> PortFuture<'_, SchedulerResult<u64>>;

    /// 只推 `last_fired_at`（cron / 时区解析失败时的退路，见 [`advanced_next_run`]）。
    fn touch_fired_at(&self, trigger_id: Uuid) -> PortFuture<'_, SchedulerResult<u64>>;
}

/// 一次派发的**窄结果**（只要审计需要的两项；不让 job 认识完整的 run 行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRun {
    /// `autopilot_run.id`。
    pub run_id: Uuid,
    /// `autopilot_run.status`（可能是 `skipped` —— 准入拦下的 run 也算已派发，上游同）。
    pub status: String,
}

/// 派发面（上游 `AutopilotScheduleDispatcher` 接口）。
///
/// 本片**自带真实现**：见下面的 `impl ScheduleDispatch for AutopilotDispatcher`。
/// 桩实现给单测用（不碰库就能造出「派发成功 / 派发失败 / 幂等复用」三条路径）。
pub trait ScheduleDispatch: Send + Sync {
    /// 按 `(autopilot, trigger, planned_at)` 派发一次；幂等键由实现侧保证
    /// （M5-4 的 `dispatch_for_plan` 用 `schedule:{trigger}:{plannedAt}` + 唯一索引）。
    fn dispatch_for_plan(
        &self,
        autopilot: &AutopilotRow,
        trigger_id: Uuid,
        planned_at: DateTime<Utc>,
    ) -> PortFuture<'_, SchedulerResult<ScheduleRun>>;
}

/// 真实现：直接转调 M5-4 的派发器（**不重复实现**准入 / 幂等 / 建 run）。
///
/// `req` 用 [`DispatchRequest::for_source`] 造：`dispatch_for_plan` 会自己把 `source` 覆写成
/// `schedule`、把 `planned_at` 与幂等键补齐 ⇒ 这里传的 `planned_at` 只占位（上游同，
/// handler 只把 `in.PlanTime` 交给 dispatcher 的第 6 个参数）。
impl ScheduleDispatch for AutopilotDispatcher {
    fn dispatch_for_plan(
        &self,
        autopilot: &AutopilotRow,
        trigger_id: Uuid,
        planned_at: DateTime<Utc>,
    ) -> PortFuture<'_, SchedulerResult<ScheduleRun>> {
        // `DispatchRequest<'a>` 借 `&'a AutopilotRow` ⇒ 先把行拷进来，再在 future 内部造请求，
        // 免得掉进自引用。`AutopilotRow` 是 16 列的浅拷贝，代价可忽略。
        let autopilot = autopilot.clone();
        Box::pin(async move {
            let req = DispatchRequest::for_source(
                &autopilot,
                RunSource::Schedule,
                Some(trigger_id),
                None,
            );
            let outcome = self
                .dispatch_for_plan(req, planned_at)
                .await
                .map_err(map_dispatch_err)?;
            Ok(ScheduleRun {
                run_id: outcome.run.id,
                status: outcome.run.status,
            })
        })
    }
}

/// `DispatchError` → [`SchedulerError`]。
///
/// 上游把派发错误直接 `return` 出 handler ⇒ `classifyError` 的 `default` 分支（**可重试**）。
/// 本地逐条对齐：只有「入参不合法」是 `Permanent`（重试不会好），其余都按可重试处理 ——
/// 配额拦下也是可重试：额度重置后同一 `plan_time` 还要能接着跑（幂等键保证不会双开 run）。
fn map_dispatch_err(err: DispatchError) -> SchedulerError {
    match err {
        DispatchError::Invalid(message) => SchedulerError::Permanent {
            code: "invalid_dispatch_request".to_owned(),
            message,
        },
        other => SchedulerError::Handler(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// tick 内缓存
// ---------------------------------------------------------------------------

/// 一个可调度 trigger 的「本 tick 视图」（上游 `autopilotTriggerConfig`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerConfig {
    /// `autopilot_trigger.id`。
    pub trigger_id: Uuid,
    /// cron 表达式（已保证非空）。
    pub cron_expression: String,
    /// 时区 IANA 名（空列已兜底成 [`DEFAULT_TIMEZONE`]）。
    pub timezone: String,
    /// `created_at`：**从未触发过**的新 trigger 的枚举锚点（别补到它出生之前）。
    pub created_at: DateTime<Utc>,
    /// `last_fired_at`：上次真的触发过 ⇒ 枚举从它之后开始（防「迁移后重放」）。
    pub last_fired_at: Option<DateTime<Utc>>,
}

impl TriggerConfig {
    /// 由 trigger 行构造；`cron_expression` 缺失 / 空 ⇒ `None`（上游 `continue` 跳过）。
    #[must_use]
    pub fn from_row(row: &AutopilotTriggerRow) -> Option<Self> {
        let cron_expression = row
            .cron_expression
            .as_deref()
            .filter(|expr| !expr.is_empty())?
            .to_owned();
        let timezone = row
            .timezone
            .as_deref()
            .filter(|tz| !tz.is_empty())
            .unwrap_or(DEFAULT_TIMEZONE)
            .to_owned();
        Some(Self {
            trigger_id: row.id,
            cron_expression,
            timezone,
            created_at: row.created_at,
            last_fired_at: row.last_fired_at,
        })
    }

    /// 本 trigger 的租约作用域。
    #[must_use]
    pub fn scope(&self) -> Scope {
        Scope::new(SCOPE_KIND, self.trigger_id.to_string())
    }
}

/// 每 tick 重写一次的 trigger 配置表（上游 `autopilotScheduleCache`）。
///
/// 作用域提供者在本 tick 顶部**整体替换**它，计划钩子再逐 scope 读一次 ⇒ 两者看到的
/// 一定是同一份快照（不会出现「scope 列出来了、配置却是上一 tick 的」）。
/// 用 `std::sync::RwLock` 而不是 tokio 的：临界区里没有 `await`（纯 map 读写）。
#[derive(Debug, Default)]
pub struct ScheduleCache {
    triggers: RwLock<HashMap<Uuid, TriggerConfig>>,
}

impl ScheduleCache {
    /// 空缓存。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 整体替换（本 tick 的快照）。
    pub fn replace(&self, next: HashMap<Uuid, TriggerConfig>) {
        match self.triggers.write() {
            Ok(mut guard) => *guard = next,
            // 中毒只可能来自别的线程 panic；缓存是纯派生数据 ⇒ 取回内层继续用，
            // 不要在这里二次 panic（否则一个坏 tick 会把调度器带崩）。
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// 取一条配置。
    #[must_use]
    pub fn get(&self, trigger_id: Uuid) -> Option<TriggerConfig> {
        match self.triggers.read() {
            Ok(guard) => guard.get(&trigger_id).cloned(),
            Err(poisoned) => poisoned.into_inner().get(&trigger_id).cloned(),
        }
    }

    /// 快照里的条数（日志 / 测试用）。
    #[must_use]
    pub fn len(&self) -> usize {
        match self.triggers.read() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// 是否为空（`len` 的伴生方法，clippy 要求成对出现）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 由行构造 `(cache 快照, scopes)` —— **纯函数**（读库在 [`catalog_scopes`]）。
#[must_use]
pub fn plan_scopes(rows: &[AutopilotTriggerRow]) -> (HashMap<Uuid, TriggerConfig>, Vec<Scope>) {
    let mut next = HashMap::with_capacity(rows.len());
    let mut scopes = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(cfg) = TriggerConfig::from_row(row) else {
            continue;
        };
        scopes.push(cfg.scope());
        next.insert(cfg.trigger_id, cfg);
    }
    (next, scopes)
}

// ---------------------------------------------------------------------------
// 作用域提供者 / 计划钩子 / handler
// ---------------------------------------------------------------------------

/// 作用域提供者：每 tick 重列可调度 trigger，并刷新缓存（上游 `autopilotScopes`）。
///
/// 重启语义：**不维护任何进程内定时器表** —— 每个 tick 都从库里重新推导「本 tick 要跑谁」
/// （trigger 被停用 / autopilot 被暂停 ⇒ 自然掉出 scope 列表，无需失效通知）。
#[must_use]
pub fn catalog_scopes(
    catalog: Arc<dyn AutopilotSchedulePort>,
    cache: Arc<ScheduleCache>,
) -> ScopeProvider {
    Arc::new(move |_now: DateTime<Utc>| {
        let catalog = catalog.clone();
        let cache = cache.clone();
        Box::pin(async move {
            let rows = catalog.list_schedulable_triggers().await?;
            let (next, scopes) = plan_scopes(&rows);
            cache.replace(next);
            Ok(scopes)
        })
    })
}

/// 计划钩子：算这一 scope 本 tick 该跑的 `plan_time`（上游 `autopilotPlansForScope`）。
///
/// 五步，顺序就是语义：
///
/// 1. **缓存里没有** ⇒ 空（trigger 在「列 scope」与「算计划」之间被删/被过滤 ⇒ 静默 no-op，正确）；
/// 2. **重试面优先**：最新一行是 `FAILED` 且预算未尽、退避已到 ⇒ **原样返回那个 `plan_time`**。
///    没有这一支，半开区间 `(latest.plan_time, now]` 会跳过失败桶，那次触发就永久丢了
///    （上游注释里的 `#4444` 教训）；
/// 3. **锚点三选一**：有历史 → 最新 `plan_time`；否则有 `last_fired_at` → 它（防迁移后重放）；
///    否则 `created_at`（别补到 trigger 出生之前）；
/// 4. 锚点被 [`REPLAY_WINDOW_HOURS`] 夹住（长期休眠的 trigger 不会枚举出百万个历史桶）；
/// 5. 枚举 `(after, now]` 的每一格，**只留最近一格**（`latest_only` 折叠），再过
///    [`is_autopilot_schedule_plan_stale`] 的迟到闸。
#[must_use]
pub fn plans_hook(cache: Arc<ScheduleCache>) -> PlansHook {
    Arc::new(
        move |scope: Scope, now: DateTime<Utc>, latest: LatestPlanInfo| {
            let cache = cache.clone();
            Box::pin(async move { plans_for_scope(&cache, &scope, now, latest) })
        },
    )
}

/// [`plans_hook`] 的纯逻辑体（不装箱，便于直接单测）。
fn plans_for_scope(
    cache: &ScheduleCache,
    scope: &Scope,
    now: DateTime<Utc>,
    latest: LatestPlanInfo,
) -> SchedulerResult<Vec<DateTime<Utc>>> {
    let Some(trigger_id) = parse_scope_id(&scope.id).ok() else {
        // scope 不是本 job 造的（不可能）：按「缓存里没有」处理，不制造 panic。
        return Ok(Vec::new());
    };
    let Some(cfg) = cache.get(trigger_id) else {
        return Ok(Vec::new());
    };

    // ② 重试面：必须原样回吐同一个 plan_time（理由见函数文档）。
    if latest.retry_eligible(now) {
        return Ok(vec![latest.plan_time]);
    }

    // ③ 锚点三选一。
    let mut after = if latest.found {
        latest.plan_time
    } else if let Some(fired_at) = cfg.last_fired_at {
        fired_at
    } else {
        cfg.created_at
    };
    // ④ 回看上限。
    let oldest = now - Duration::hours(REPLAY_WINDOW_HOURS);
    if after < oldest {
        after = oldest;
    }

    // ⑤ 枚举 + 折叠 + 迟到闸。
    let occurrences =
        cron::next_occurrences_between_utc(&cfg.cron_expression, &cfg.timezone, after, now)
            .map_err(|err| invalid_cron(&err))?;
    let Some(latest_due) = occurrences.last().copied() else {
        return Ok(Vec::new());
    };
    if is_autopilot_schedule_plan_stale(now, latest_due) {
        return Ok(Vec::new());
    }
    Ok(vec![latest_due])
}

/// cron / 时区坏掉的分类：**不可重试**（表达式存错了不会自己变好）。
///
/// 上游只是把错误 `return` 出来（由 `classifyError` 的默认分支当可重试）；本地改成
/// `Permanent` 是有意的**加法**：否则一个坏表达式会按 `MaxAttempts` 白烧 3 轮网络往返，
/// 而且在租约表里留下一串 `FAILED` 噪音。登记为偏差 D3（见 `docs/55`）。
fn invalid_cron(err: &CronError) -> SchedulerError {
    SchedulerError::Permanent {
        code: "invalid_cron".to_owned(),
        message: err.to_string(),
    }
}

/// 「慢 tick 之后这一格已经过期了吗」（上游 `isAutopilotSchedulePlanStale`9）。
///
/// 判据只有一条：`now - plan_time > ` [`MAX_LATENESS_MINUTES`]。
#[must_use]
pub fn is_autopilot_schedule_plan_stale(now: DateTime<Utc>, plan_time: DateTime<Utc>) -> bool {
    now - plan_time > Duration::minutes(MAX_LATENESS_MINUTES)
}

/// 算「派发完成后写进 `next_run_at` 的下一格」（上游 `advancedNextRun`15）。
///
/// 用**本进程时钟** `now` 锚（与 trigger 的创建/更新展示路径同口径），但下限钉在刚派发的
/// `plan_time` 上：本实例时钟慢于 DB 时钟（`plan_time` 是 DB 判的）时，不这样做会把刚跑完的
/// 那一格**又算一次**，UI 的「下次触发」就永远是过去时刻（上游 `MUL-3749`）。
///
/// 返回 `None` = cron / 时区解析失败 ⇒ 调用方退化成「只推 `last_fired_at`」。
#[must_use]
pub fn advanced_next_run(
    cron_expression: &str,
    timezone: &str,
    plan_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let anchor = if plan_time > now { plan_time } else { now };
    cron::next_occurrence_after_utc(cron_expression, timezone, anchor)
        .ok()
        .flatten()
}

/// `scope.id`（字符串）→ `Uuid`。
///
/// 上游 `parseScopeUUID` 同时接受带连字符与 32 位无连字符两种形态；`Uuid::parse_str` 同样接受
/// 这两种（外加 `urn:` / 大括号形态 —— 比上游更宽容，且不会因此产生歧义）。
fn parse_scope_id(raw: &str) -> Result<Uuid, SchedulerError> {
    Uuid::parse_str(raw)
        .map_err(|_| SchedulerError::Handler(format!("scope id is not a valid uuid: {raw}")))
}

/// 业务 handler（上游 `autopilotHandler`）。
///
/// 与 scope 列表**刻意重复**读一次 trigger + autopilot：tick 之间发生的状态变化
/// （trigger 被停用、autopilot 被暂停）要**立刻**生效，不能等到下一轮 scope 列表。
/// 三条「没事可做」的分支都写 `SUCCESS` 审计行（带 `skipped_reason`）—— 这不是失败。
#[must_use]
pub fn schedule_handler(
    catalog: Arc<dyn AutopilotSchedulePort>,
    dispatch: Arc<dyn ScheduleDispatch>,
) -> Handler {
    Arc::new(move |input: HandlerInput| {
        let catalog = catalog.clone();
        let dispatch = dispatch.clone();
        Box::pin(async move {
            handle_scope(&*catalog, &*dispatch, &input.scope.id, input.plan_time).await
        })
    })
}

/// [`schedule_handler`] 的逻辑体，入参已展开成「上游 `autopilotHandler` 真正读到的东西」。
///
/// 单独开成 `pub` 是**有意的可测缝**（偏差 D7）：上游 handler 的入参是
/// `(ctx, job, scope, planTime)`，本地 `HandlerInput` 里还挂着一个 [`crate::db_ops::Heartbeat`]
/// —— 而 `Heartbeat` 必须由真库的 `SchedulerRepo` 构造 ⇒ 若只暴露 `Handler`，
/// 「停用 / 暂停 / cron 坏掉」这几条分支就只能去真库测试里验（慢且不在门禁 ⑤ 里）。
pub async fn handle_scope(
    catalog: &dyn AutopilotSchedulePort,
    dispatch: &dyn ScheduleDispatch,
    scope_id: &str,
    plan_time: DateTime<Utc>,
) -> SchedulerResult<HandlerResult> {
    let trigger_id = parse_scope_id(scope_id)
        .map_err(|err| SchedulerError::Handler(format!("autopilot handler: {err}")))?;

    let Some(trigger) = catalog
        .load_trigger(trigger_id)
        .await
        .map_err(|err| wrap("load trigger", &err))?
    else {
        // trigger 在「列 scope」与「跑 handler」之间被删：为一条已消失的租约写 SUCCESS 是对的
        // —— 没有要派发的东西，后续 tick 也不会再返回这个 scope。
        return Ok(skipped("trigger_not_found", None));
    };
    if !trigger.enabled || trigger.kind != TRIGGER_KIND_SCHEDULE {
        return Ok(skipped("trigger_disabled", None));
    }

    let Some(autopilot) = catalog
        .load_autopilot(trigger.autopilot_id)
        .await
        .map_err(|err| wrap("load autopilot", &err))?
    else {
        return Ok(skipped("autopilot_not_found", None));
    };
    if autopilot.status != "active" {
        return Ok(skipped("autopilot_inactive", Some(&autopilot.status)));
    }

    let run = dispatch
        .dispatch_for_plan(&autopilot, trigger.id, plan_time)
        .await?;

    // 展示列推进：**失败不致命** —— 权威记录是 `autopilot_run.created_at`，下次派发也会重刷。
    // 解析失败（脏 cron / 脏时区）退化成只推 `last_fired_at`，至少让「刚触发过」可见。
    let timezone = trigger
        .timezone
        .as_deref()
        .filter(|tz| !tz.is_empty())
        .unwrap_or(DEFAULT_TIMEZONE);
    let cron_expression = trigger.cron_expression.as_deref().unwrap_or_default();
    match advanced_next_run(cron_expression, timezone, plan_time, Utc::now()) {
        Some(next) => {
            if let Err(err) = catalog.advance_next_run(trigger.id, Some(next)).await {
                tracing::warn!(
                    trigger = %trigger.id,
                    error = %err,
                    "autopilot schedule: advance next_run_at failed"
                );
            }
        }
        None => {
            if let Err(err) = catalog.touch_fired_at(trigger.id).await {
                tracing::warn!(
                    trigger = %trigger.id,
                    error = %err,
                    "autopilot schedule: touch last_fired_at failed"
                );
            }
        }
    }

    Ok(HandlerResult {
        rows_affected: 1,
        result_json: Some(json_object(&[
            ("run_id", &run.run_id.to_string()),
            ("run_status", &run.status),
        ])),
    })
}

/// 给端口错误补一层上下文（上游 `fmt.Errorf("load trigger: %w", err)`）。
///
/// 端口错误在本 job 里都是可重试那侧（`Handler`）⇒ 用 [`SchedulerError::Handler`] 包一层不改分类。
fn wrap(context: &str, err: &SchedulerError) -> SchedulerError {
    SchedulerError::Handler(format!("{context}: {err}"))
}

/// 「没事可做」的 SUCCESS 结果（上游 `HandlerResult{RowsAffected: 0, Result: {...}}`）。
fn skipped(reason: &str, status: Option<&str>) -> HandlerResult {
    let mut pairs: Vec<(&str, &str)> = vec![("skipped_reason", reason)];
    if let Some(status) = status {
        pairs.push(("status", status));
    }
    HandlerResult {
        rows_affected: 0,
        result_json: Some(json_object(&pairs)),
    }
}

// ---------------------------------------------------------------------------
// job 规格
// ---------------------------------------------------------------------------

/// 构造 job 规格（上游 `AutopilotScheduleDispatchJob`）。
///
/// `catalog` / `dispatch` 是构造期**冻结**的两个端口：注册进 `Manager` 后不再变化。
/// 时间预算逐字对齐上游：`cadence = 0`（钩子驱动，cron 表达式是任意的）、
/// `catch_up_window = 24h`、`run = 2m` / `stale = 5m` / `hb = 30s`、
/// `allow_stale_reentry = true`、`max_attempts = 3` + `1m/5m/15m` 退避、`max_plans_per_tick = 5`
/// （`LatestOnly` 用不到 5 格，这个上限只在有人把钩子换成 `every_plan` 时起作用）。
///
/// 返回 `JobSpec`（**不是** `Result`）：上游也不返回错误 —— 规格合法性由
/// `Manager::register` 的 `validate` 统一把关（漏填时间预算会在这里被拒）。
#[must_use]
pub fn job(
    catalog: Arc<dyn AutopilotSchedulePort>,
    dispatch: Arc<dyn ScheduleDispatch>,
) -> JobSpec {
    let cache = Arc::new(ScheduleCache::new());
    JobSpec::new(
        JOB_NAME,
        Duration::zero(),
        catalog_scopes(catalog.clone(), cache.clone()),
        schedule_handler(catalog, dispatch),
    )
    .with_catch_up(
        // 有钩子时 `catch_up_mode` 只当审计用；写 `LatestOnly` 是为了与钩子的折叠策略一致。
        CatchUpMode::LatestOnly,
        Duration::hours(REPLAY_WINDOW_HOURS),
        5,
    )
    .with_timing(
        std::time::Duration::from_secs(120),
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(30),
    )
    .with_retry(
        3,
        vec![
            Duration::minutes(1),
            Duration::minutes(5),
            Duration::minutes(15),
        ],
    )
    .with_allow_stale_reentry(true)
    .with_plans_for_scope(plans_hook(cache))
}
