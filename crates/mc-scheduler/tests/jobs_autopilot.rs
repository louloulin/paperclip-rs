//! M5-8（`LUM-1571`）autopilot 计划派发 job 的测试：**分桶（时区）/ 过期闸 / 展示列推进**。
//!
//! 纯逻辑用例（门禁 ⑤ 会跑到，不需要库）覆盖 `DoD` ①②③ 与 handler 的每条分支；
//! 真库 e2e 在文件末尾（`#[ignore]`，跑法见 `tests/common/mod.rs`）。
//! 夹具与桩端口在 `tests/common/mod.rs`（共享，门禁 ⑩ 的 800 行上限逼出来的拆分）。

mod common;

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use mc_repos::scheduler::{ExecutionStatus, LatestPlanInfo, Lease};
use mc_scheduler::db_ops;
use mc_scheduler::error::SchedulerError;
use mc_scheduler::jobs::autopilot::{self as sched, ScheduleCache, TriggerConfig};
use mc_scheduler::spec::{CatchUpMode, Scope};
use mc_scheduler::{Manager, Options};

use common::*;

// ---------------------------------------------------------------------------
// autopilot：作用域推导 / 缓存
// ---------------------------------------------------------------------------

#[test]
fn plan_scopes_skips_rows_without_a_cron() {
    let due = trigger_row(Uuid::new_v4(), "*/5 * * * *", None, base_now());
    let mut no_cron = trigger_row(Uuid::new_v4(), "", None, base_now());
    no_cron.cron_expression = None;
    let (cache, scopes) = snapshot(&[due.clone(), no_cron.clone()]);

    assert_eq!(
        scopes.len(),
        1,
        "空 cron 的 trigger 不进 scope 列表（上游 continue）"
    );
    assert_eq!(scopes[0].kind, sched::SCOPE_KIND);
    assert_eq!(scopes[0].id, due.id.to_string());
    assert_eq!(cache.len(), 1, "缓存里也只有可调度的那条");
    assert!(!cache.is_empty());
    assert!(cache.get(no_cron.id).is_none());
    assert!(cache.get(due.id).is_some());
}

#[test]
fn trigger_config_defaults_timezone_to_utc() {
    let id = Uuid::new_v4();
    let null_tz = trigger_row(id, "0 9 * * *", None, base_now());
    assert_eq!(
        TriggerConfig::from_row(&null_tz).expect("cron").timezone,
        sched::DEFAULT_TIMEZONE
    );

    let empty_tz = trigger_row(id, "0 9 * * *", Some(""), base_now());
    assert_eq!(
        TriggerConfig::from_row(&empty_tz).expect("cron").timezone,
        sched::DEFAULT_TIMEZONE,
        "空串也兜底成 UTC"
    );

    let shanghai = trigger_row(id, "0 9 * * *", Some("Asia/Shanghai"), base_now());
    let cfg = TriggerConfig::from_row(&shanghai).expect("cron");
    assert_eq!(cfg.timezone, "Asia/Shanghai");
    assert_eq!(cfg.scope().kind, sched::SCOPE_KIND);
    assert_eq!(cfg.scope().id, id.to_string());
}

// ---------------------------------------------------------------------------
// DoD ①：两个时区 ⇒ 两个桶，且各自与自己的时区对齐
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_timezones_align_their_own_buckets() {
    // 同一个 now、同一张网格上的两个 scope：
    //   * UTC            的 `0 9 * * *`  ⇒ 本 tick 的桶是 09:00Z；
    //   * Asia/Shanghai  的 `0 17 * * *` ⇒ 17:00 CST == 09:00Z，同一个桶。
    // 两个 scope 各自拿到「自己时区里的那一格」，都等于 09:00:00Z ⇒ 本 tick 两个桶各跑一次。
    let utc = Uuid::new_v4();
    let cst = Uuid::new_v4();
    let (cache, scopes) = snapshot(&[
        trigger_row(
            utc,
            "0 9 * * *",
            Some("UTC"),
            base_now() - Duration::days(3),
        ),
        trigger_row(
            cst,
            "0 17 * * *",
            Some("Asia/Shanghai"),
            base_now() - Duration::days(3),
        ),
    ]);
    let hook = sched::plans_hook(cache);
    let now = base_now();
    let expected = at(2026, 3, 5, 9, 0, 0);

    assert_eq!(scopes.len(), 2, "两个 trigger ⇒ 两个 scope ⇒ 两个桶");
    for scope in &scopes {
        assert_eq!(
            plans_now(&hook, scope, now, LatestPlanInfo::empty()).await,
            vec![expected],
            "scope {} 应按自己的时区落在 09:00Z",
            scope.id
        );
    }

    // 时区真的进了计算：把 Shanghai 那条按 UTC 解读 ⇒ 17:00Z 还没到 ⇒ 本 now 无桶。
    let (utc_cache, _) = snapshot(&[trigger_row(
        cst,
        "0 17 * * *",
        Some("UTC"),
        base_now() - Duration::days(3),
    )]);
    let utc_hook = sched::plans_hook(utc_cache);
    let shanghai_five_pm = scope_of(cst, "0 17 * * *", "Asia/Shanghai");
    assert!(
        plans_now(&utc_hook, &shanghai_five_pm, now, LatestPlanInfo::empty())
            .await
            .is_empty(),
        "按 UTC 解读的 17:00 不该在本 tick 出桶"
    );

    // 反向：CST 的 09:00（== 01:00Z）在 09:00:30Z 已过了 8h ⇒ 迟到闸拒掉。
    let (cst_cache, _) = snapshot(&[trigger_row(
        cst,
        "0 9 * * *",
        Some("Asia/Shanghai"),
        base_now() - Duration::days(3),
    )]);
    let cst_hook = sched::plans_hook(cst_cache);
    let shanghai_nine = scope_of(cst, "0 9 * * *", "Asia/Shanghai");
    assert!(
        plans_now(&cst_hook, &shanghai_nine, now, LatestPlanInfo::empty())
            .await
            .is_empty(),
        "同一个 now、同一个表达式，换个时区就该没有桶"
    );
    // 而它在自己的时刻（01:00:30Z）有桶，且恰好是 CST 的 09:00。
    assert_eq!(
        plans_now(
            &cst_hook,
            &shanghai_nine,
            at(2026, 3, 5, 1, 0, 30),
            LatestPlanInfo::empty()
        )
        .await,
        vec![at(2026, 3, 5, 1, 0, 0)]
    );
}

// ---------------------------------------------------------------------------
// DoD ②：过期计划不重投
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_plan_is_not_redelivered() {
    // 每小时整点的 cron：09:00 那一格在 09:04 还能跑，09:06 就过期了（上游 5 分钟闸）。
    let id = Uuid::new_v4();
    let (cache, _) = snapshot(&[trigger_row(
        id,
        "0 * * * *",
        Some("UTC"),
        at(2026, 3, 5, 0, 0, 0),
    )]);
    let hook = sched::plans_hook(cache);
    let scope = scope_of(id, "0 * * * *", "UTC");
    let previous = at(2026, 3, 5, 8, 0, 0);

    assert_eq!(
        plans_now(
            &hook,
            &scope,
            at(2026, 3, 5, 9, 4, 30),
            latest_success(previous)
        )
        .await,
        vec![at(2026, 3, 5, 9, 0, 0)],
        "迟到 4m30s ⇒ 在闸内，照跑"
    );
    assert!(
        plans_now(
            &hook,
            &scope,
            at(2026, 3, 5, 9, 6, 30),
            latest_success(previous)
        )
        .await
        .is_empty(),
        "迟到 6m30s ⇒ 过期，这一格永久跳过（宁可漏跑，也不在停机后补一堆历史桶）"
    );

    // 闸的边界是 `>` 5 分钟：正好 5 分钟仍算在闸内。
    assert!(sched::is_autopilot_schedule_plan_stale(
        at(2026, 3, 5, 9, 6, 0),
        at(2026, 3, 5, 9, 0, 0)
    ));
    assert!(!sched::is_autopilot_schedule_plan_stale(
        at(2026, 3, 5, 9, 5, 0),
        at(2026, 3, 5, 9, 0, 0)
    ));
}

#[tokio::test]
async fn retry_eligible_failed_bucket_is_replayed_unchanged() {
    // 失败桶必须原样回吐同一个 plan_time：否则半开区间 `(latest.plan_time, now]` 会跳过它，
    // 那次触发就永久丢了（上游 #4444 的教训）。
    let id = Uuid::new_v4();
    let plan = at(2026, 3, 5, 9, 0, 0);
    let (cache, _) = snapshot(&[trigger_row(
        id,
        "0 * * * *",
        Some("UTC"),
        at(2026, 3, 5, 0, 0, 0),
    )]);
    let hook = sched::plans_hook(cache);
    let scope = scope_of(id, "0 * * * *", "UTC");
    let now = at(2026, 3, 5, 9, 20, 0);
    let failed = LatestPlanInfo {
        found: true,
        plan_time: plan,
        status: ExecutionStatus::Failed,
        attempt: 1,
        max_attempts: 3,
        next_retry_at: Some(now - Duration::seconds(1)),
    };
    assert_eq!(
        plans_now(&hook, &scope, now, failed).await,
        vec![plan],
        "预算未尽 + 退避已到 ⇒ 重投同一个桶（哪怕它早就过了 5 分钟闸）"
    );

    // 预算用尽 ⇒ 退回正常枚举：09:00 已过期 ⇒ 空（不重投）。
    let exhausted = LatestPlanInfo {
        attempt: 3,
        ..failed
    };
    assert!(plans_now(&hook, &scope, now, exhausted).await.is_empty());
}

#[tokio::test]
async fn cold_start_anchor_is_clamped_and_never_replays_old_buckets() {
    // 三天前创建、每 15 分钟一格的 trigger：锚点被 24h 回看上限夹住，
    // 结果只留**最近一格**（不是三天前那一格）。
    let id = Uuid::new_v4();
    let (cache, _) = snapshot(&[trigger_row(
        id,
        "*/15 * * * *",
        Some("UTC"),
        base_now() - Duration::days(3),
    )]);
    let hook = sched::plans_hook(cache);
    let scope = scope_of(id, "*/15 * * * *", "UTC");
    let now = base_now();

    let plans = plans_now(&hook, &scope, now, LatestPlanInfo::empty()).await;
    assert_eq!(plans.len(), 1, "latest_only 折叠成一格");
    assert_eq!(plans[0], at(2026, 3, 5, 9, 0, 0));
    assert!(now - plans[0] < Duration::minutes(1));
}

#[tokio::test]
async fn bad_cron_is_a_permanent_error() {
    let id = Uuid::new_v4();
    let (cache, _) = snapshot(&[trigger_row(
        id,
        "not a cron",
        Some("UTC"),
        base_now() - Duration::hours(1),
    )]);
    let hook = sched::plans_hook(cache);
    let scope = scope_of(id, "not a cron", "UTC");
    let err = hook(scope, base_now(), LatestPlanInfo::empty())
        .await
        .expect_err("坏表达式必须报错");
    assert_eq!(err.code(), "invalid_cron", "登记为偏差 D3：不可重试");
    assert!(matches!(err, SchedulerError::Permanent { .. }));
}

#[tokio::test]
async fn scope_missing_from_cache_yields_no_plan() {
    let (cache, _) = snapshot(&[]);
    let hook = sched::plans_hook(cache);
    // 缓存为空 ⇒ 静默 no-op（trigger 在「列 scope」与「算计划」之间被删）。
    assert!(plans_now(
        &hook,
        &Scope::new(sched::SCOPE_KIND, Uuid::new_v4().to_string()),
        base_now(),
        LatestPlanInfo::empty()
    )
    .await
    .is_empty());
    // 连 scope id 都不是 uuid：同样只当「没有」（不 panic）。
    assert!(plans_now(
        &hook,
        &Scope::new(sched::SCOPE_KIND, "not-a-uuid"),
        base_now(),
        LatestPlanInfo::empty()
    )
    .await
    .is_empty());
}

// ---------------------------------------------------------------------------
// DoD ③：advancedNextRun
// ---------------------------------------------------------------------------

#[test]
fn advanced_next_run_targets_the_next_slot() {
    let plan = at(2026, 3, 5, 9, 0, 0);
    // 本进程时钟稍晚（正常情形）⇒ 明天的 09:00。
    assert_eq!(
        sched::advanced_next_run("0 9 * * *", "UTC", plan, at(2026, 3, 5, 9, 0, 30)),
        Some(at(2026, 3, 6, 9, 0, 0))
    );
    // 时钟回摆（now < plan_time）⇒ 锚点钉在 plan_time 上，绝不把刚跑完的那一格再算一次。
    assert_eq!(
        sched::advanced_next_run("0 9 * * *", "UTC", plan, at(2026, 3, 5, 8, 59, 0)),
        Some(at(2026, 3, 6, 9, 0, 0))
    );
    // 时区参与：CST 的 09:00 == 01:00Z。
    assert_eq!(
        sched::advanced_next_run(
            "0 9 * * *",
            "Asia/Shanghai",
            at(2026, 3, 5, 1, 0, 0),
            at(2026, 3, 5, 1, 0, 30)
        ),
        Some(at(2026, 3, 6, 1, 0, 0))
    );
    // 坏输入 ⇒ None（调用方退化成只推 last_fired_at）。
    assert_eq!(
        sched::advanced_next_run("not a cron", "UTC", plan, base_now()),
        None
    );
    assert_eq!(
        sched::advanced_next_run("0 9 * * *", "Mars/Olympus", plan, base_now()),
        None
    );
}

#[tokio::test]
async fn handler_dispatches_and_advances_the_display_column() {
    let (catalog, dispatch, trigger_id) = healthy_ports();
    let plan = at(2026, 3, 5, 9, 0, 0);
    let result = sched::handle_scope(&*catalog, &*dispatch, &trigger_id.to_string(), plan)
        .await
        .expect("handler 成功");

    assert_eq!(result.rows_affected, 1);
    assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        dispatch.seen.lock().expect("lock").as_slice(),
        &[(trigger_id, plan)],
        "派发必须收到本 tick 的 plan_time（幂等键的一半）"
    );
    let json = result.result_json.expect("审计 JSON");
    assert!(json.contains("\"run_status\":\"queued\""), "{json}");

    let advanced = catalog.advanced.lock().expect("lock").clone();
    assert_eq!(advanced.len(), 1, "派发成功后推展示列");
    let next = advanced[0].expect("cron 可算 ⇒ 推 next_run_at");
    assert!(next > Utc::now(), "next_run_at 必须落在未来：{next}");
    assert_eq!(catalog.touched.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn handler_skips_missing_disabled_and_paused_targets() {
    let dispatch = Arc::new(StubDispatch::default());
    let plan = base_now();
    let trigger = trigger_row(
        Uuid::new_v4(),
        "0 9 * * *",
        Some("UTC"),
        plan - Duration::days(1),
    );

    // ① trigger 已不存在（列 scope 之后被删）。
    let catalog = Arc::new(StubCatalog::default());
    let result = sched::handle_scope(&*catalog, &*dispatch, &trigger.id.to_string(), plan)
        .await
        .expect("skip 不是错误");
    assert_eq!(result.rows_affected, 0, "skip 一律 SUCCESS + 0 行");
    assert!(result
        .result_json
        .expect("json")
        .contains("trigger_not_found"));

    // ② trigger 被停用。
    let mut disabled =
        StubCatalog::healthy(trigger.clone(), autopilot_row(Uuid::new_v4(), "active"));
    disabled.trigger.as_mut().expect("trigger").enabled = false;
    let catalog = Arc::new(disabled);
    let result = sched::handle_scope(&*catalog, &*dispatch, &trigger.id.to_string(), plan)
        .await
        .expect("skip 不是错误");
    assert!(result
        .result_json
        .expect("json")
        .contains("trigger_disabled"));

    // ③ kind 不是 schedule（webhook 误入 scope 列表 ⇒ 同一个码）。
    let mut webhook =
        StubCatalog::healthy(trigger.clone(), autopilot_row(Uuid::new_v4(), "active"));
    webhook.trigger.as_mut().expect("trigger").kind = "webhook".to_owned();
    let catalog = Arc::new(webhook);
    let result = sched::handle_scope(&*catalog, &*dispatch, &trigger.id.to_string(), plan)
        .await
        .expect("skip 不是错误");
    assert!(result
        .result_json
        .expect("json")
        .contains("trigger_disabled"));

    // ④ autopilot 被删。
    let mut orphan = StubCatalog::healthy(trigger.clone(), autopilot_row(Uuid::new_v4(), "active"));
    orphan.autopilot = None;
    let catalog = Arc::new(orphan);
    let result = sched::handle_scope(&*catalog, &*dispatch, &trigger.id.to_string(), plan)
        .await
        .expect("skip 不是错误");
    assert!(result
        .result_json
        .expect("json")
        .contains("autopilot_not_found"));

    // ⑤ autopilot 被暂停（状态写进 result_json，便于排障）。
    let mut paused = StubCatalog::healthy(trigger.clone(), autopilot_row(Uuid::new_v4(), "paused"));
    paused.autopilot.as_mut().expect("autopilot").status = "paused".to_owned();
    let catalog = Arc::new(paused);
    let result = sched::handle_scope(&*catalog, &*dispatch, &trigger.id.to_string(), plan)
        .await
        .expect("skip 不是错误");
    let json = result.result_json.expect("json");
    assert!(json.contains("autopilot_inactive"), "{json}");
    assert!(json.contains("paused"), "{json}");

    assert_eq!(
        dispatch.calls.load(Ordering::SeqCst),
        0,
        "五条 skip 分支一条都不许派发"
    );
}

#[tokio::test]
async fn handler_rejects_a_scope_that_is_not_a_uuid() {
    let (catalog, dispatch, _) = healthy_ports();
    let err = sched::handle_scope(&*catalog, &*dispatch, "global", base_now())
        .await
        .expect_err("不是 uuid 的 scope 必须报错（不是静默 skip）");
    assert_eq!(err.code(), "handler_error");
    assert_eq!(dispatch.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn handler_degrades_to_touch_when_the_cron_is_broken() {
    // cron 坏掉时派发仍然成功（计划是钩子算的），展示列退化成只推 last_fired_at。
    let (_, dispatch, trigger_id) = healthy_ports();
    let mut broken = StubCatalog::healthy(
        trigger_row(
            trigger_id,
            "not a cron",
            Some("UTC"),
            base_now() - Duration::days(1),
        ),
        autopilot_row(Uuid::new_v4(), "active"),
    );
    broken.trigger.as_mut().expect("trigger").cron_expression = Some("not a cron".to_owned());
    let catalog = Arc::new(broken);

    let result = sched::handle_scope(&*catalog, &*dispatch, &trigger_id.to_string(), base_now())
        .await
        .expect("坏 cron 不该让 handler 失败");
    assert_eq!(result.rows_affected, 1);
    assert!(
        catalog.advanced.lock().expect("lock").is_empty(),
        "cron 坏掉 ⇒ 算不出下一格 ⇒ 不写 next_run_at"
    );
    assert_eq!(catalog.touched.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn handler_reports_dispatch_failures_to_the_kernel() {
    let (catalog, _, trigger_id) = healthy_ports();
    let dispatch = Arc::new(StubDispatch {
        fail: true,
        ..StubDispatch::default()
    });
    let err = sched::handle_scope(&*catalog, &*dispatch, &trigger_id.to_string(), base_now())
        .await
        .expect_err("派发失败要冒泡给内核（由内核判重试）");
    assert_eq!(err.code(), "handler_error");
    assert!(
        catalog.advanced.lock().expect("lock").is_empty(),
        "派发失败不推展示列（重试仍用同一个桶）"
    );
}

// ---------------------------------------------------------------------------
// autopilot job 规格
// ---------------------------------------------------------------------------

#[tokio::test]
async fn autopilot_job_spec_matches_upstream_budgets() {
    let (catalog, dispatch, trigger_id) = healthy_ports();
    let spec = sched::job(catalog.clone(), dispatch);

    assert_eq!(spec.name, sched::JOB_NAME);
    assert_eq!(spec.cadence, Duration::zero(), "计划完全由钩子给（上游同）");
    assert_eq!(spec.schedule_delay, Duration::zero());
    assert_eq!(spec.catch_up_mode, CatchUpMode::LatestOnly);
    assert_eq!(
        spec.catch_up_window,
        Duration::hours(sched::REPLAY_WINDOW_HOURS)
    );
    assert_eq!(spec.max_plans_per_tick, 5);
    assert_eq!(spec.run_timeout, StdDuration::from_secs(120));
    assert_eq!(spec.stale_timeout, StdDuration::from_secs(300));
    assert_eq!(spec.heartbeat_interval, StdDuration::from_secs(30));
    assert!(spec.allow_stale_reentry);
    assert_eq!(spec.max_attempts, 3);
    assert_eq!(
        spec.retry_backoff,
        vec![
            Duration::minutes(1),
            Duration::minutes(5),
            Duration::minutes(15)
        ]
    );
    spec.validate()
        .expect("规格合法（run < stale、心跳 < stale）");

    // 装进去的两个闭包真的接上了端口：
    let scopes = (spec.scopes)(base_now()).await.expect("列 scope");
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].id, trigger_id.to_string());
    assert_eq!(catalog.list_calls.load(Ordering::SeqCst), 1);

    let hook = spec.plans_for_scope.clone().expect("必须装计划钩子");
    assert_eq!(
        plans_now(&hook, &scopes[0], base_now(), LatestPlanInfo::empty()).await,
        vec![at(2026, 3, 5, 9, 0, 0)],
        "同一个 scope 在本 tick 拿到 09:00Z 那一格"
    );
}

#[tokio::test]
async fn catalog_scopes_refreshes_the_cache_every_tick() {
    let (catalog, _, trigger_id) = healthy_ports();
    let cache = Arc::new(ScheduleCache::new());
    let provider = sched::catalog_scopes(catalog.clone(), cache.clone());

    let scopes = provider(base_now()).await.expect("列 scope");
    assert_eq!(scopes.len(), 1);
    assert_eq!(catalog.list_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        cache.len(),
        1,
        "每 tick 重列并整体替换缓存（无进程内定时器表）"
    );
    assert!(cache.get(trigger_id).is_some());
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn real_db_autopilot_dispatches_one_run_for_the_current_bucket() {
    let repo = repo().await;
    let trigger = fresh_trigger(Uuid::new_v4());
    let catalog = Arc::new(StubCatalog::healthy(
        trigger.clone(),
        autopilot_row(Uuid::new_v4(), "active"),
    ));
    let dispatch = Arc::new(StubDispatch::default());
    let spec = sched::job(catalog.clone(), dispatch.clone());

    let mut manager = Manager::new(
        repo.clone(),
        Options::default().with_runner_id("itest-m5-8-a"),
    );
    manager.register(spec).expect("register");
    manager.run_once().await.expect("tick");

    assert_eq!(
        catalog.list_calls.load(Ordering::SeqCst),
        1,
        "每 tick 列一次"
    );
    assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1, "本桶只派发一次");
    let seen = dispatch.seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, trigger.id, "派发的是 scope 指向的那条 trigger");

    let now = repo.db_now().await.expect("db now");
    assert!(
        now - seen[0].1 < Duration::minutes(5),
        "plan_time 落在 5 分钟桶上且未过期：{} vs {}",
        seen[0].1,
        now
    );
    assert_eq!(
        seen[0].1.timestamp() % 300,
        0,
        "plan_time 取整到桶（*/5 的整数倍）"
    );

    let scope = Scope::new(sched::SCOPE_KIND, trigger.id.to_string());
    let info = db_ops::latest_plan(&repo, sched::JOB_NAME, &scope)
        .await
        .expect("latest plan");
    assert!(info.found, "审计行必须留痕");
    assert_eq!(info.status, ExecutionStatus::Success);
    assert_eq!(info.plan_time, seen[0].1);
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn real_db_two_managers_never_dispatch_the_same_bucket_twice() {
    let repo = repo().await;
    let trigger = fresh_trigger(Uuid::new_v4());
    let catalog = Arc::new(StubCatalog::healthy(
        trigger.clone(),
        autopilot_row(Uuid::new_v4(), "active"),
    ));
    let dispatch = Arc::new(StubDispatch::default());

    let make = |runner: &str| {
        let spec = sched::job(catalog.clone(), dispatch.clone());
        let mut manager = Manager::new(repo.clone(), Options::default().with_runner_id(runner));
        manager.register(spec).expect("register");
        manager
    };
    let (a, b) = (make("itest-m5-8-a"), make("itest-m5-8-b"));
    let (ra, rb) = tokio::join!(async { a.run_once().await }, async { b.run_once().await });
    ra.expect("tick a");
    rb.expect("tick b");

    let seen = dispatch.seen.lock().expect("lock").clone();
    assert!(!seen.is_empty(), "至少有一次派发（两个实例里赢的那个）");
    let mut plan_times: Vec<DateTime<Utc>> = seen.iter().map(|(_, plan)| *plan).collect();
    plan_times.sort_unstable();
    plan_times.dedup();
    assert_eq!(
        plan_times.len(),
        seen.len(),
        "同一个 plan_time 绝不被两个实例各跑一次（租约 + plan 桶）：{seen:?}"
    );
    assert!(
        seen.iter().all(|(id, _)| *id == trigger.id),
        "只有那一条 trigger 会被派发"
    );
}

// ---------------------------------------------------------------------------
// 内核公共 API 的编译期锚点
// ---------------------------------------------------------------------------

#[test]
fn core_types_this_slice_depends_on_are_still_public() {
    // 本片一行内核代码都没改：这些构造在编译期证明「依赖的内核面没变」。
    let lease = Lease {
        id: Uuid::new_v4(),
        lease_token: Uuid::new_v4(),
    };
    assert_ne!(lease.id, lease.lease_token);
    let empty: HashMap<Uuid, TriggerConfig> = HashMap::new();
    let cache = ScheduleCache::new();
    cache.replace(empty);
    assert!(cache.is_empty());
}
