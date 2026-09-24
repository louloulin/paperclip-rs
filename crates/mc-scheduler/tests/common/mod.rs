//! 两个 job 测试共用的夹具与桩端口（`tests/common/mod.rs`）。
//!
//! 单独成模块是**门禁 ⑩**（R7 单文件 800 行硬上限）逼出来的：一个测试文件塞不下
//! autopilot 的时区/过期/展示列用例与 wakeup 的 Tick 循环用例 ⇒ 夹具提出来共享。
//! 每个测试目标都 `mod common;` 一次（`common` 本身不会被当成独立测试目标）。
//!
//! `allow(dead_code, unused_imports)`：两个测试目标各用其中一部分，另一半在对方
//! 视角里就是「没人用」——这是共享夹具的正常状态，不是死代码。

#![allow(dead_code, unused_imports)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, TimeZone, Utc};
use uuid::Uuid;

use mc_repos::autopilot::{AutopilotRow, AutopilotTriggerRow};
use mc_repos::plugin::hook::HookScheduleRow;
use mc_repos::scheduler::{ExecutionStatus, LatestPlanInfo, Lease};
use mc_repos::wakeup::WakeupRow;
use mc_scheduler::db_ops;
use mc_scheduler::error::{SchedulerError, SchedulerResult};
use mc_scheduler::jobs::autopilot::{
    self as sched, AutopilotSchedulePort, ScheduleCache, ScheduleDispatch, ScheduleRun,
    TriggerConfig,
};
use mc_scheduler::jobs::issue_wakeup::{self as wakeup, WakeupDispatchPort, WakeupOutcome};
use mc_scheduler::jobs::plugin_hook::{PluginHookPort, ScheduleDispatchRequest, ScheduleOutcome};
use mc_scheduler::jobs::{json_object, JsonObject, PortFuture};
use mc_scheduler::spec::{CatchUpMode, PlansHook, Scope};
use mc_scheduler::{Manager, Options, SchedulerRepo};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 固定时刻（`single()` 保证不存在夏令时歧义）。
pub fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .expect("唯一的本地时刻")
}

/// 固定基准：`2026-03-05T09:00:30Z`（UTC 09:00 那一格刚过 30s）。
pub fn base_now() -> DateTime<Utc> {
    at(2026, 3, 5, 9, 0, 30)
}

/// 一条 schedule trigger。
pub fn trigger_row(
    id: Uuid,
    cron: &str,
    timezone: Option<&str>,
    created_at: DateTime<Utc>,
) -> AutopilotTriggerRow {
    AutopilotTriggerRow {
        id,
        autopilot_id: Uuid::new_v4(),
        kind: "schedule".to_owned(),
        enabled: true,
        cron_expression: Some(cron.to_owned()),
        timezone: timezone.map(str::to_owned),
        next_run_at: None,
        webhook_token: None,
        label: None,
        last_fired_at: None,
        created_at,
        updated_at: created_at,
        provider: "local".to_owned(),
        signing_secret: None,
        event_filters: None,
        published_by_type: None,
        published_by_id: None,
        created_by_type: None,
        created_by_id: None,
    }
}

/// 一条 autopilot（只填 handler 会读的列）。
pub fn autopilot_row(id: Uuid, status: &str) -> AutopilotRow {
    let now = Utc::now();
    AutopilotRow {
        id,
        workspace_id: Uuid::new_v4(),
        title: "nightly".to_owned(),
        description: None,
        assignee_id: Uuid::new_v4(),
        status: status.to_owned(),
        execution_mode: "auto".to_owned(),
        issue_title_template: None,
        created_by_type: "member".to_owned(),
        created_by_id: Uuid::new_v4(),
        last_run_at: None,
        created_at: now,
        updated_at: now,
        assignee_type: "member".to_owned(),
        project_id: None,
        pause_reason: None,
    }
}

/// 一条 wakeup（只填 id 与计数用得到的列）。
pub fn wakeup_row(id: Uuid) -> WakeupRow {
    let now = Utc::now();
    WakeupRow {
        id,
        workspace_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        created_by: Uuid::new_v4(),
        source_task_id: None,
        parent_comment_id: None,
        instruction: "look at the issue".to_owned(),
        kind: "every".to_owned(),
        mode: "continuous".to_owned(),
        event_types: Vec::new(),
        filter_actor_type: None,
        filter_actor_id: None,
        filter_agent_id: None,
        filter_task_id: None,
        interval_seconds: Some(300),
        cron_expression: None,
        timezone: "UTC".to_owned(),
        next_fire_at: Some(now),
        enabled: true,
        disabled_at: None,
        revision: 1,
        last_task_id: None,
        last_error: None,
        created_at: now,
        updated_at: now,
    }
}

/// 「已有一行 SUCCESS」的计划视图。
pub fn latest_success(plan_time: DateTime<Utc>) -> LatestPlanInfo {
    LatestPlanInfo {
        found: true,
        plan_time,
        status: ExecutionStatus::Success,
        attempt: 1,
        max_attempts: 3,
        next_retry_at: None,
    }
}

/// 把若干 trigger 变成「本 tick 的缓存快照 + scope 列表」。
pub fn snapshot(rows: &[AutopilotTriggerRow]) -> (Arc<ScheduleCache>, Vec<Scope>) {
    let (next, scopes) = sched::plan_scopes(rows);
    let cache = Arc::new(ScheduleCache::new());
    cache.replace(next);
    (cache, scopes)
}

/// 「有 cron + 指定时区」的 scope（scope id 就是 trigger id）。
pub fn scope_of(id: Uuid, cron: &str, timezone: &str) -> Scope {
    TriggerConfig::from_row(&trigger_row(id, cron, Some(timezone), base_now()))
        .expect("有 cron ⇒ 有配置")
        .scope()
}

/// 调一次计划钩子。
pub async fn plans_now(
    hook: &PlansHook,
    scope: &Scope,
    now: DateTime<Utc>,
    latest: LatestPlanInfo,
) -> Vec<DateTime<Utc>> {
    hook(scope.clone(), now, latest)
        .await
        .expect("计划钩子成功")
}

// ---------------------------------------------------------------------------
// autopilot handler 的分支
// ---------------------------------------------------------------------------

/// 测试用「库端口」：完全在内存里。
#[derive(Default)]
pub struct StubCatalog {
    pub rows: Vec<AutopilotTriggerRow>,
    pub trigger: Option<AutopilotTriggerRow>,
    pub autopilot: Option<AutopilotRow>,
    pub list_calls: AtomicUsize,
    pub advanced: Mutex<Vec<Option<DateTime<Utc>>>>,
    pub touched: AtomicUsize,
}

impl StubCatalog {
    /// 一条 trigger + 一个 active 的 autopilot（把 autopilot 的 id 对上 `trigger.autopilot_id`）。
    pub fn healthy(trigger: AutopilotTriggerRow, mut autopilot: AutopilotRow) -> Self {
        autopilot.id = trigger.autopilot_id;
        Self {
            rows: vec![trigger.clone()],
            trigger: Some(trigger),
            autopilot: Some(autopilot),
            ..Self::default()
        }
    }
}

impl AutopilotSchedulePort for StubCatalog {
    fn list_schedulable_triggers(
        &self,
    ) -> PortFuture<'_, SchedulerResult<Vec<AutopilotTriggerRow>>> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        let rows = self.rows.clone();
        Box::pin(async move { Ok(rows) })
    }

    fn load_trigger(
        &self,
        trigger_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<AutopilotTriggerRow>>> {
        let row = self.trigger.clone().filter(|row| row.id == trigger_id);
        Box::pin(async move { Ok(row) })
    }

    fn load_autopilot(
        &self,
        autopilot_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<AutopilotRow>>> {
        let row = self.autopilot.clone().filter(|row| row.id == autopilot_id);
        Box::pin(async move { Ok(row) })
    }

    fn advance_next_run(
        &self,
        _trigger_id: Uuid,
        next_run_at: Option<DateTime<Utc>>,
    ) -> PortFuture<'_, SchedulerResult<u64>> {
        self.advanced.lock().expect("lock").push(next_run_at);
        Box::pin(async move { Ok(1) })
    }

    fn touch_fired_at(&self, _trigger_id: Uuid) -> PortFuture<'_, SchedulerResult<u64>> {
        self.touched.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(1) })
    }
}

/// 测试用「派发端口」。
#[derive(Default)]
pub struct StubDispatch {
    pub calls: AtomicUsize,
    pub seen: Mutex<Vec<(Uuid, DateTime<Utc>)>>,
    pub fail: bool,
}

impl ScheduleDispatch for StubDispatch {
    fn dispatch_for_plan(
        &self,
        _autopilot: &AutopilotRow,
        trigger_id: Uuid,
        planned_at: DateTime<Utc>,
    ) -> PortFuture<'_, SchedulerResult<ScheduleRun>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .expect("lock")
            .push((trigger_id, planned_at));
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                return Err(SchedulerError::Handler("dispatch boom".to_owned()));
            }
            Ok(ScheduleRun {
                run_id: Uuid::nil(),
                status: "queued".to_owned(),
            })
        })
    }
}

/// 「一切正常」的一套端口：`(catalog, dispatch, trigger_id)`。
pub fn healthy_ports() -> (Arc<StubCatalog>, Arc<StubDispatch>, Uuid) {
    let trigger = trigger_row(
        Uuid::new_v4(),
        "0 9 * * *",
        Some("UTC"),
        base_now() - Duration::days(1),
    );
    let catalog = Arc::new(StubCatalog::healthy(
        trigger.clone(),
        autopilot_row(Uuid::new_v4(), "active"),
    ));
    (catalog, Arc::new(StubDispatch::default()), trigger.id)
}

// ---------------------------------------------------------------------------
// issue wakeup：Tick 循环
// ---------------------------------------------------------------------------

/// 测试用 wakeup 端口（成功/失败/超时三种行为都可调）。
#[derive(Default)]
pub struct StubWakeup {
    pub candidates: Vec<WakeupRow>,
    pub tick_calls: AtomicUsize,
    pub dispatched: Mutex<Vec<Uuid>>,
    pub noted: Mutex<Vec<(Uuid, String)>>,
    pub touched: Mutex<Vec<Uuid>>,
    /// 第 n 条候选（0 基）派发失败。
    pub failures: Vec<()>,
    /// 让派发慢到超过单规则预算。
    pub slow_dispatch: Option<StdDuration>,
    /// 让收尾慢到超过收尾预算。
    pub slow_touch: Option<StdDuration>,
    /// `tick_candidates` 直接失败。
    pub list_fails: bool,
}

impl StubWakeup {
    pub fn with_candidates(ids: &[Uuid]) -> Self {
        Self {
            candidates: ids.iter().copied().map(wakeup_row).collect(),
            ..Self::default()
        }
    }
}

impl WakeupDispatchPort for StubWakeup {
    fn tick_candidates(&self) -> PortFuture<'_, SchedulerResult<Vec<WakeupRow>>> {
        self.tick_calls.fetch_add(1, Ordering::SeqCst);
        let rows = self.candidates.clone();
        let fails = self.list_fails;
        Box::pin(async move {
            if fails {
                return Err(SchedulerError::Handler("list boom".to_owned()));
            }
            Ok(rows)
        })
    }

    fn dispatch_wakeup(
        &self,
        wakeup: &WakeupRow,
    ) -> PortFuture<'_, SchedulerResult<WakeupOutcome>> {
        let id = wakeup.id;
        let index = {
            let mut seen = self.dispatched.lock().expect("lock");
            seen.push(id);
            seen.len() - 1
        };
        let slow = self.slow_dispatch;
        let fails = self.failures.get(index).is_some();
        Box::pin(async move {
            if let Some(delay) = slow {
                tokio::time::sleep(delay).await;
            }
            if fails {
                return Err(SchedulerError::Handler(format!("wakeup {id} boom")));
            }
            Ok(WakeupOutcome::Dispatched)
        })
    }

    fn note_dispatch_failure(
        &self,
        wakeup_id: Uuid,
        error: &str,
    ) -> PortFuture<'_, SchedulerResult<()>> {
        self.noted
            .lock()
            .expect("lock")
            .push((wakeup_id, error.to_owned()));
        Box::pin(async move { Ok(()) })
    }

    fn touch_dispatch(&self, wakeup_id: Uuid) -> PortFuture<'_, SchedulerResult<()>> {
        self.touched.lock().expect("lock").push(wakeup_id);
        let slow = self.slow_touch;
        Box::pin(async move {
            if let Some(delay) = slow {
                tokio::time::sleep(delay).await;
            }
            Ok(())
        })
    }
}

/// 按「每条候选各自的收场」返回的端口（验四种计数）。
#[derive(Default)]
pub struct OutcomeWakeup {
    pub candidates: Vec<WakeupRow>,
    pub outcomes: Vec<WakeupOutcome>,
    pub dispatched: Mutex<Vec<Uuid>>,
    pub touched: Mutex<Vec<Uuid>>,
}

impl WakeupDispatchPort for OutcomeWakeup {
    fn tick_candidates(&self) -> PortFuture<'_, SchedulerResult<Vec<WakeupRow>>> {
        let rows = self.candidates.clone();
        Box::pin(async move { Ok(rows) })
    }

    fn dispatch_wakeup(
        &self,
        wakeup: &WakeupRow,
    ) -> PortFuture<'_, SchedulerResult<WakeupOutcome>> {
        let index = {
            let mut seen = self.dispatched.lock().expect("lock");
            seen.push(wakeup.id);
            seen.len() - 1
        };
        let outcome = self
            .outcomes
            .get(index)
            .copied()
            .unwrap_or(WakeupOutcome::Dispatched);
        Box::pin(async move { Ok(outcome) })
    }

    fn note_dispatch_failure(
        &self,
        _wakeup_id: Uuid,
        _error: &str,
    ) -> PortFuture<'_, SchedulerResult<()>> {
        Box::pin(async move { Ok(()) })
    }

    fn touch_dispatch(&self, wakeup_id: Uuid) -> PortFuture<'_, SchedulerResult<()>> {
        self.touched.lock().expect("lock").push(wakeup_id);
        Box::pin(async move { Ok(()) })
    }
}

// ---------------------------------------------------------------------------
// M6-8（`LUM-1673`）：hook job 的桩端口
// ---------------------------------------------------------------------------

/// `plugin_hook` job 的桩端口。
///
/// 放在共享夹具里而不是各自的用例文件里：M5 的 `register_all` 用例要**构造**它（端口包是
/// 四实参、没有缺省），M6-8 自己的用例要**驱动**它。两边的默认行为都是「什么都不做」。
#[derive(Default)]
pub struct StubPluginHook {
    pub schedules: Mutex<Vec<HookScheduleRow>>,
    pub dispatched: Mutex<Vec<ScheduleDispatchRequest>>,
    pub advanced: Mutex<Vec<(Uuid, Uuid)>>,
    /// 让 `dispatch_schedule` 回投递成功（默认回 `Skipped`）。
    pub deliver: bool,
}

impl StubPluginHook {
    pub fn with_schedules(rows: &[HookScheduleRow]) -> Self {
        Self {
            schedules: Mutex::new(rows.to_vec()),
            ..Self::default()
        }
    }

    pub fn delivering(rows: &[HookScheduleRow]) -> Self {
        Self {
            deliver: true,
            ..Self::with_schedules(rows)
        }
    }
}

impl PluginHookPort for StubPluginHook {
    fn list_enabled_schedules(&self) -> PortFuture<'_, SchedulerResult<Vec<HookScheduleRow>>> {
        let rows = self.schedules.lock().expect("lock").clone();
        Box::pin(async move { Ok(rows) })
    }

    fn load_schedule(
        &self,
        schedule_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<HookScheduleRow>>> {
        let found = self
            .schedules
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.id == schedule_id)
            .cloned();
        Box::pin(async move { Ok(found) })
    }

    fn dispatch_schedule(
        &self,
        request: ScheduleDispatchRequest,
    ) -> PortFuture<'_, SchedulerResult<ScheduleOutcome>> {
        self.dispatched.lock().expect("lock").push(request);
        let deliver = self.deliver;
        let delivery_id = format!("psd_{}", request.plan_time.timestamp());
        Box::pin(async move {
            Ok(if deliver {
                ScheduleOutcome::Delivered { delivery_id }
            } else {
                ScheduleOutcome::Skipped("installation_disabled".to_owned())
            })
        })
    }

    fn advance_next_run(
        &self,
        schedule_id: Uuid,
        generation: Uuid,
        plan_time: DateTime<Utc>,
    ) -> PortFuture<'_, SchedulerResult<u64>> {
        let _ = plan_time;
        self.advanced
            .lock()
            .expect("lock")
            .push((schedule_id, generation));
        Box::pin(async move { Ok(1) })
    }
}

/// 一行 `plugin_hook_schedule` 夹具（表**没有外键** ⇒ 不需要 workspace / 安装行）。
pub fn hook_schedule_row(
    id: Uuid,
    installation_id: Uuid,
    hook_key: &str,
    cron: &str,
    timezone: &str,
    activated_at: DateTime<Utc>,
) -> HookScheduleRow {
    HookScheduleRow {
        id,
        installation_id,
        workspace_id: Uuid::new_v4(),
        hook_key: hook_key.to_owned(),
        cron_expression: cron.to_owned(),
        timezone: timezone.to_owned(),
        generation: Uuid::new_v4(),
        activated_at,
        next_run_at: None,
        enabled: true,
        created_at: activated_at,
        updated_at: activated_at,
    }
}

// ---------------------------------------------------------------------------
// 真库 e2e（`#[ignore]`）
// ---------------------------------------------------------------------------

/// 连接测试库（没有 URL 就显式失败，不静默跳过）。
pub async fn repo() -> SchedulerRepo {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    SchedulerRepo::connect(&url, 4, 1)
        .await
        .expect("connect scheduler repo")
}

pub fn fresh_trigger(id: Uuid) -> AutopilotTriggerRow {
    trigger_row(
        id,
        "*/5 * * * *",
        Some("UTC"),
        Utc::now() - Duration::hours(1),
    )
}
