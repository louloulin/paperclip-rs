//! M6-8（`LUM-1673`）plugin hook 计划投递 job 的测试。
//!
//! 纯逻辑用例覆盖上游 `scheduler/jobs_plugin_hook.go` 的每一步（scope 形状、作用域枚举、
//! 计划折叠与重试复用、handler 的两个分支、分页枚举 `latestPluginHookOccurrence`、
//! job 规格）；真库 e2e 在文件末尾（`#[ignore]`）：`register_all` 登记三个 job、
//! 起循环、以及**同一个 schedule 桶在多次 tick 后只派发一次**。
//! 夹具与桩端口在 `tests/common/mod.rs`（共享，门 ⑩ 的 800 行上限逼出来的拆分）。

mod common;

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, TimeZone, Utc};
use uuid::Uuid;

use mc_repos::scheduler::{ExecutionStatus, LatestPlanInfo};
use mc_scheduler::db_ops;
use mc_scheduler::jobs::plugin_hook::{self as hook_job, ScheduleOutcome};
use mc_scheduler::spec::Scope;
use mc_scheduler::{Manager, Options};

use common::*;

/// 具体桩 → `dyn` 端口（`run_schedule_once` 收的是 trait object）。
fn as_port(port: &Arc<StubPluginHook>) -> Arc<dyn hook_job::PluginHookPort> {
    port.clone()
}

// ---------------------------------------------------------------------------
// 作用域 id
// ---------------------------------------------------------------------------

/// `<schedule_id>:<generation>` 的往返，以及**恰好两段**的形状要求。
#[test]
fn scope_id_round_trips_and_rejects_other_shapes() {
    let schedule = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let id = hook_job::plugin_hook_scope_id(schedule, generation);
    assert_eq!(id, format!("{schedule}:{generation}"));
    assert_eq!(
        hook_job::parse_scope_id(&id).expect("round trip"),
        (schedule, generation)
    );

    for rejected in [
        "",
        "not-a-scope",
        "1111:",
        ":2222",
        "1111:2222:3333",
        &format!("{schedule}:not-a-uuid"),
        &format!("not-a-uuid:{generation}"),
    ] {
        assert!(
            hook_job::parse_scope_id(rejected).is_err(),
            "{rejected} 不该被当成作用域"
        );
    }
}

// ---------------------------------------------------------------------------
// 作用域枚举与计划
// ---------------------------------------------------------------------------

/// 一启用日程一 scope（上游 `pluginHookScheduleScopes`）。
#[tokio::test]
async fn catalog_lists_one_scope_per_enabled_schedule() {
    let now = Utc::now();
    let first = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "sync",
        "*/5 * * * *",
        "UTC",
        now - Duration::hours(1),
    );
    let second = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "digest",
        "0 3 * * *",
        "Asia/Shanghai",
        now - Duration::days(1),
    );
    let port = Arc::new(StubPluginHook::with_schedules(&[
        first.clone(),
        second.clone(),
    ]));
    let cache = Arc::new(hook_job::ScheduleCache::default());
    let provider = hook_job::catalog_scopes(port.clone(), cache.clone());

    let scopes = provider(now).await.expect("scopes");
    assert_eq!(scopes.len(), 2);
    assert_eq!(scopes[0].kind, hook_job::SCOPE_KIND);
    assert_eq!(scopes[0].id, first.scope_id());
    assert_eq!(scopes[1].id, second.scope_id());
    assert!(
        cache.get(&first.scope_id()).is_some(),
        "枚举必须把行留在缓存里（计划钩子只有 scope id）"
    );
}

/// 计划：**最新一格**是唯一计划，且重复调用得到**同一个** `plan_time`
/// （这就是「同一个桶不会被派发两次」的前提：桶必须稳定）。
#[tokio::test]
async fn plans_pick_the_latest_due_occurrence_and_stay_stable() {
    let now = Utc.with_ymd_and_hms(2026, 3, 4, 12, 7, 30).unwrap();
    let row = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "sync",
        "*/5 * * * *",
        "UTC",
        Utc.with_ymd_and_hms(2026, 3, 4, 9, 0, 0).unwrap(),
    );
    let port = Arc::new(StubPluginHook::with_schedules(std::slice::from_ref(&row)));
    let cache = Arc::new(hook_job::ScheduleCache::default());
    hook_job::catalog_scopes(port, cache.clone())(now)
        .await
        .expect("scopes");
    let plans = hook_job::plans_hook(cache.clone());
    let scope = Scope::new(hook_job::SCOPE_KIND, row.scope_id());

    let first = plans(scope.clone(), now, LatestPlanInfo::empty())
        .await
        .expect("plans");
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0],
        Utc.with_ymd_and_hms(2026, 3, 4, 12, 5, 0).unwrap(),
        "取的必须是 (activated_at, now] 里最新的那一格，不是最早的那一格"
    );
    // 12:00→12:05 一共 37 格被折叠成 1 格 ⇒ 折叠计数 = 36。
    assert_eq!(
        cache.take_coalesced(&row.scope_id(), first[0]),
        36,
        "折叠掉的格子数必须能被报出来（上游 coalesced_occurrences）"
    );

    // 同一格重复调用：锚在最新历史行上 ⇒ 仍是同一个桶（**不会**每 tick 现取一个 now()）。
    let latest = LatestPlanInfo {
        found: true,
        plan_time: first[0],
        status: ExecutionStatus::Success,
        attempt: 1,
        max_attempts: 3,
        next_retry_at: None,
    };
    let again = plans(scope.clone(), now, latest).await.expect("plans");
    assert!(
        again.is_empty(),
        "SUCCESS 之后、下一格到点之前不该再有计划：{again:?}"
    );

    // 下一格到点：12:10（`*/5` 网格），且必须是**一个新桶**。
    let later = Utc.with_ymd_and_hms(2026, 3, 4, 12, 10, 30).unwrap();
    let next = plans(scope, later, latest).await.expect("plans");
    assert_eq!(next.len(), 1);
    assert_eq!(
        next[0],
        Utc.with_ymd_and_hms(2026, 3, 4, 12, 10, 0).unwrap(),
        "下一格是 (12:05, 12:10:30] 里最新的那一格"
    );
}

/// `RUNNING` ⇒ 本 tick 无计划（同一代内严格串行）；`FAILED` 且还有预算 ⇒ **复用同一个**
/// `plan_time`（重试不能另起一格）。
#[tokio::test]
async fn plans_serialize_a_generation_and_reuse_the_bucket_for_retries() {
    let now = Utc.with_ymd_and_hms(2026, 3, 4, 12, 7, 30).unwrap();
    let row = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "sync",
        "*/5 * * * *",
        "UTC",
        now - Duration::hours(1),
    );
    let port = Arc::new(StubPluginHook::with_schedules(std::slice::from_ref(&row)));
    let cache = Arc::new(hook_job::ScheduleCache::default());
    hook_job::catalog_scopes(port, cache.clone())(now)
        .await
        .expect("scopes");
    let plans = hook_job::plans_hook(cache);
    let scope = Scope::new(hook_job::SCOPE_KIND, row.scope_id());
    let bucket = Utc.with_ymd_and_hms(2026, 3, 4, 11, 55, 0).unwrap();

    let running = LatestPlanInfo {
        found: true,
        plan_time: bucket,
        status: ExecutionStatus::Running,
        attempt: 1,
        max_attempts: 3,
        next_retry_at: None,
    };
    assert!(
        plans(scope.clone(), now, running)
            .await
            .expect("plans")
            .is_empty(),
        "上一格还在跑 ⇒ 本 tick 不排新格"
    );

    let failed_ready = LatestPlanInfo {
        status: ExecutionStatus::Failed,
        attempt: 1,
        ..running
    };
    let retry = plans(scope.clone(), now, failed_ready)
        .await
        .expect("plans");
    assert_eq!(retry, vec![bucket], "重试必须复用同一个 plan_time");

    let failed_backoff = LatestPlanInfo {
        next_retry_at: Some(now + Duration::minutes(1)),
        ..failed_ready
    };
    assert!(
        plans(scope.clone(), now, failed_backoff)
            .await
            .expect("plans")
            .is_empty(),
        "退避未到点 ⇒ 不排"
    );

    let exhausted = LatestPlanInfo {
        attempt: 3,
        ..failed_backoff
    };
    let next = plans(scope, now, exhausted).await.expect("plans");
    assert_eq!(next.len(), 1, "预算用尽 ⇒ 推进到下一格");
    assert!(next[0] > bucket);
}

/// 长停机：分页枚举把中间那些格子折成**真正最新**的一格（上游 `latestPluginHookOccurrence`）。
#[test]
fn latest_occurrence_pages_past_the_first_batch() {
    let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let until = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
    // `* * * * *` = 每分钟一格 ⇒ 两个月的格子远超一页（1024）。
    let (plan, count) =
        hook_job::latest_occurrence("* * * * *", "UTC", after, until).expect("occurrences");
    assert!(count > 1024, "本用例必须跨过一页：{count}");
    assert_eq!(
        plan,
        Some(until),
        "半开区间 (after, until] 的最新一格就是 until"
    );
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// 投递成功 ⇒ `rows_affected = 1` 且审计 JSON 带 `delivery_id` / 折叠数 / 派发滞后。
#[tokio::test]
async fn handler_reports_a_delivery_with_its_audit_fields() {
    let now = Utc::now();
    let row = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "sync",
        "*/5 * * * *",
        "UTC",
        now - Duration::hours(1),
    );
    let port = Arc::new(StubPluginHook::delivering(std::slice::from_ref(&row)));
    let cache = Arc::new(hook_job::ScheduleCache::default());
    // 计划钩子先在真实枚举里留痕（handler 从同一个缓存读折叠数）。
    hook_job::catalog_scopes(port.clone(), cache.clone())(now)
        .await
        .expect("scopes");
    let plan_time = now - Duration::seconds(2);
    cache.set_coalesced(&row.scope_id(), plan_time, 3);

    let result =
        hook_job::run_schedule_once(&as_port(&port), &cache, &row.scope_id(), plan_time, 1, 3)
            .await
            .expect("handler");
    assert_eq!(result.rows_affected, 1);
    let json = result.result_json.expect("audit json");
    assert!(json.contains("\"coalesced_occurrences\":3"), "{json}");
    assert!(json.contains("dispatch_lag_ms"), "{json}");
    assert!(
        json.contains(&format!(
            "\"delivery_id\":\"psd_{}\"",
            plan_time.timestamp()
        )),
        "{json}"
    );
    let dispatched = port.dispatched.lock().expect("lock").clone();
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].plan_time, plan_time);
    assert_eq!(dispatched[0].attempt, 1);
    assert!(
        !dispatched[0].last_attempt,
        "max_attempts=3 ⇒ 第 1 次不是最后一次"
    );
}

/// 跳过 ⇒ `rows_affected = 0` + `skipped_reason`（终态，不再重试同一个桶）。
#[tokio::test]
async fn handler_reports_a_skip_as_a_terminal_outcome() {
    let now = Utc::now();
    let row = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "sync",
        "*/5 * * * *",
        "UTC",
        now - Duration::hours(1),
    );
    // `deliver = false` ⇒ 桩端口回 `Skipped`。
    let port = Arc::new(StubPluginHook::with_schedules(std::slice::from_ref(&row)));
    let cache = Arc::new(hook_job::ScheduleCache::default());
    hook_job::catalog_scopes(port.clone(), cache.clone())(now)
        .await
        .expect("scopes");

    let result = hook_job::run_schedule_once(&as_port(&port), &cache, &row.scope_id(), now, 3, 3)
        .await
        .expect("handler");
    assert_eq!(result.rows_affected, 0);
    let json = result.result_json.expect("audit json");
    assert!(
        json.contains("\"skipped_reason\":\"installation_disabled\""),
        "{json}"
    );
    let dispatched = port.dispatched.lock().expect("lock").clone();
    assert!(
        dispatched[0].last_attempt,
        "attempt == max_attempts ⇒ 必须告诉端口「这是最后一次」"
    );
}

/// 作用域形状不对 ⇒ 错误（记一行 FAILED），不是静默跳过。
#[tokio::test]
async fn handler_rejects_a_foreign_scope_shape() {
    let port = Arc::new(StubPluginHook::default());
    let cache = hook_job::ScheduleCache::default();
    let error = hook_job::run_schedule_once(&as_port(&port), &cache, "global", Utc::now(), 1, 3)
        .await
        .expect_err("bad scope");
    assert_eq!(error.code(), "handler_error");
    assert!(error.to_string().contains("schedule:generation"));
}

/// job 规格：持久化名、单 tick 一格、只认最新桶、三次尝试与 45s/90s/15s 的预算。
#[test]
fn job_spec_matches_upstream() {
    let spec = hook_job::job(Arc::new(StubPluginHook::default()));
    spec.validate().expect("valid spec");
    assert_eq!(spec.name, "plugin_hook_schedule_dispatch");
    assert_eq!(spec.max_plans_per_tick, 1);
    assert_eq!(
        spec.catch_up_mode,
        mc_scheduler::spec::CatchUpMode::LatestOnly
    );
    assert_eq!(spec.catch_up_window, Duration::zero());
    assert_eq!(spec.max_attempts, 3);
    assert_eq!(spec.run_timeout, StdDuration::from_secs(45));
    assert_eq!(spec.stale_timeout, StdDuration::from_secs(90));
    assert_eq!(spec.heartbeat_interval, StdDuration::from_secs(15));
    assert!(spec.allow_stale_reentry);
    assert_eq!(
        spec.retry_backoff,
        vec![Duration::seconds(30), Duration::seconds(120)]
    );
    assert!(spec.plans_for_scope.is_some(), "本 job 必须用自定义计划器");
}

// ---------------------------------------------------------------------------
// 真库 e2e（`#[ignore]`）
// ---------------------------------------------------------------------------

/// `DoD`：`register_all` 把 hook job 登记成第三个；起循环；**同一个桶只派发一次**。
///
/// 「只派发一次」的屏障是 `sys_cron_executions` 的唯一键 —— 本用例用**多个 tick** 去撞它：
/// 循环的 tick 是 50ms，而计划格是 5 分钟 ⇒ 若桶不稳定（每 tick 现取 `now()`），
/// `dispatched` 会涨到两位数；稳定时恰好 1。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn real_db_the_loop_delivers_each_schedule_bucket_exactly_once() {
    let repo = repo().await;
    // 新造的 uuid ⇒ scope 唯一 ⇒ 桶不可能被别的用例/实例占着。
    let row = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "sync",
        "*/5 * * * *",
        "UTC",
        Utc::now() - Duration::hours(1),
    );
    let hook_port = Arc::new(StubPluginHook::delivering(std::slice::from_ref(&row)));

    let catalog = Arc::new(StubCatalog::healthy(
        fresh_trigger(Uuid::new_v4()),
        autopilot_row(Uuid::new_v4(), "active"),
    ));
    let ports = mc_scheduler::jobs::JobPorts::new(
        catalog,
        Arc::new(StubDispatch::default()),
        Arc::new(StubWakeup::with_candidates(&[])),
        hook_port.clone(),
    );

    let mut manager = Manager::new(
        repo.clone(),
        Options::default()
            .with_runner_id("itest-m6-8-hook")
            .with_tick_interval(StdDuration::from_millis(50)),
    );
    mc_scheduler::jobs::register_all(&mut manager, &ports).expect("register_all");
    let names: Vec<&str> = manager.jobs().iter().map(|job| job.name.as_str()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert_eq!(names[2], hook_job::JOB_NAME, "hook job 是第三个");

    let handle = manager.spawn();
    let scope = Scope::new(hook_job::SCOPE_KIND, row.scope_id());
    let deadline = std::time::Instant::now() + StdDuration::from_secs(10);
    loop {
        let latest = db_ops::latest_plan(&repo, hook_job::JOB_NAME, &scope)
            .await
            .expect("latest plan");
        if latest.found {
            assert_eq!(latest.status, ExecutionStatus::Success);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "10s 内没跑出 hook job 的审计行（循环没带上第三个 job）"
        );
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }

    // 再让它多 tick 一会儿：同一个桶绝不能被派发第二次。
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    let dispatched = hook_port.dispatched.lock().expect("lock").clone();
    assert_eq!(
        dispatched.len(),
        1,
        "同一个 schedule 桶只能派发一次：{dispatched:?}"
    );
    assert_eq!(
        dispatched[0].plan_time,
        db_ops::latest_plan(&repo, hook_job::JOB_NAME, &scope)
            .await
            .expect("latest plan")
            .plan_time,
        "派发的那一格必须就是被认领的那一格"
    );

    // 确定性的第二条证据：同一个 SUCCESS 桶换实例再抢只能拿到 Conflicted。
    let probe = hook_job::job(hook_port.clone());
    let now = repo.db_now().await.expect("db now");
    let again = db_ops::try_claim(
        &repo,
        &probe,
        &scope,
        dispatched[0].plan_time,
        now,
        "itest-m6-8-hook-b",
    )
    .await
    .expect("claim");
    assert_eq!(again.kind(), db_ops::ClaimKind::Conflicted);

    // 端口只在投递路径上被调用过一次（`advance_next_run` 由端口自己在投递成功时做）。
    assert_eq!(hook_port.dispatched.lock().expect("lock").len(), 1);
    // 桩端口的 `dispatch_schedule` 不回推进展示列（真实现在投递成功时自己推进，见
    // `mc-http` 的 `dispatch_scheduled_hook`）⇒ 这里只能断言「内核没有替它推进」：
    // `advance_next_run` 是端口的责任，不是 job 的责任。
    assert!(
        hook_port.advanced.lock().expect("lock").is_empty(),
        "推进展示列由端口负责，job 不替它做"
    );

    tokio::time::timeout(StdDuration::from_secs(5), handle.shutdown())
        .await
        .expect("shutdown 必须返回");
}

/// 「端口没被调用」这条路径也要验：切得掉的桩端口。
#[tokio::test]
async fn a_skip_does_not_advance_the_display_column() {
    let now = Utc::now();
    let row = hook_schedule_row(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "sync",
        "*/5 * * * *",
        "UTC",
        now - Duration::hours(1),
    );
    let port = Arc::new(StubPluginHook::with_schedules(std::slice::from_ref(&row)));
    let cache = Arc::new(hook_job::ScheduleCache::default());
    hook_job::catalog_scopes(port.clone(), cache.clone())(now)
        .await
        .expect("scopes");
    let result = hook_job::run_schedule_once(&as_port(&port), &cache, &row.scope_id(), now, 1, 3)
        .await
        .expect("handler");
    assert_eq!(result.rows_affected, 0);
    assert!(
        port.advanced.lock().expect("lock").is_empty(),
        "跳过由端口自己决定要不要推进展示列（本桩端口不推）"
    );
    assert_eq!(
        port.dispatched.lock().expect("lock")[0]
            .schedule_id
            .to_string(),
        row.id.to_string()
    );
    // 端口回的是跳过而不是投递（桩的默认行为）—— 这条断言让上面那句「跳过」有来源。
    let outcome = as_port(&port)
        .dispatch_schedule(hook_job::ScheduleDispatchRequest {
            schedule_id: row.id,
            generation: row.generation,
            plan_time: now,
            attempt: 1,
            last_attempt: false,
        })
        .await
        .expect("dispatch");
    assert!(matches!(outcome, ScheduleOutcome::Skipped(_)));
    assert_eq!(port.dispatched.lock().expect("lock").len(), 2);
    assert!(row.enabled, "夹具默认启用；本用例只验跳过路径");
}
