//! M5-8（`LUM-1571`）issue wakeup 派发 job 的测试：**`Tick` 循环 + 审计 JSON**。
//!
//! 纯逻辑用例覆盖上游 `service/issue_wakeup.go:513` `Tick` 的每一步（四种收场计数、
//! 失败写 `last_error`、单规则 2s 预算、收尾 100ms 预算不中断整批、列候选失败立即终止）；
//! 真库 e2e 在文件末尾（`#[ignore]`）：一次 tick ⇒ 每条候选一行，且同一个桶不会被重跑。
//! 夹具与桩端口在 `tests/common/mod.rs`（共享，门禁 ⑩ 的 800 行上限逼出来的拆分）。

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use uuid::Uuid;

use mc_repos::scheduler::ExecutionStatus;
use mc_scheduler::db_ops;
use mc_scheduler::jobs::autopilot as sched;
use mc_scheduler::jobs::issue_wakeup::{self as wakeup, WakeupDispatchPort, WakeupOutcome};
use mc_scheduler::jobs::{json_object, JsonObject};
use mc_scheduler::spec::{CatchUpMode, Scope};
use mc_scheduler::{Manager, Options};

use common::*;

#[tokio::test]
async fn tick_counts_every_outcome_and_touches_every_candidate() {
    let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    let port = Arc::new(OutcomeWakeup {
        candidates: ids.iter().copied().map(wakeup_row).collect(),
        outcomes: vec![
            WakeupOutcome::Dispatched,
            WakeupOutcome::Waiting,
            WakeupOutcome::Settled,
            WakeupOutcome::Removed,
        ],
        ..OutcomeWakeup::default()
    });
    let port_dyn: Arc<dyn WakeupDispatchPort> = port.clone();

    let result = wakeup::run_tick(&port_dyn).await.expect("全部成功 ⇒ Ok");
    assert_eq!(result.rows_affected, 1, "只有一条真写了队列行");
    let json = result.result_json.expect("审计 JSON");
    for expected in [
        "\"candidates\":4",
        "\"dispatched\":1",
        "\"waiting\":1",
        "\"settled\":1",
        "\"removed\":1",
        "\"errors\":0",
    ] {
        assert!(json.contains(expected), "缺 {expected}：{json}");
    }

    let touched = port.touched.lock().expect("lock").clone();
    assert_eq!(touched.len(), 4, "每条候选都要 touch（成功与否都一样）");
    assert_eq!(touched, ids, "顺序与候选一致");
    assert_eq!(
        port.dispatched.lock().expect("lock").len(),
        4,
        "每条候选都真的进了派发"
    );
}

#[tokio::test]
async fn tick_records_failures_and_keeps_going() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let port = Arc::new(StubWakeup {
        failures: vec![(), ()],
        ..StubWakeup::with_candidates(&[a, b])
    });
    let port_dyn: Arc<dyn WakeupDispatchPort> = port.clone();

    let err = wakeup::run_tick(&port_dyn)
        .await
        .expect_err("有失败 ⇒ FAILED 审计行（max_attempts=1 ⇒ 不重试）");
    assert_eq!(err.code(), "handler_error");
    let message = err.to_string();
    assert!(message.contains(&a.to_string()), "{message}");
    assert!(message.contains(&b.to_string()), "{message}");
    assert_eq!(
        port.noted.lock().expect("lock").len(),
        2,
        "每条失败都要写 last_error（上游 NoteWakeupFailure）"
    );
    assert_eq!(
        port.touched.lock().expect("lock").len(),
        2,
        "失败也要 touch：否则 UI 里这条规则永远看不到调度器看过它"
    );
    assert_eq!(port.dispatched.lock().expect("lock").len(), 2);
}

#[tokio::test]
async fn per_rule_budget_trips_and_still_writes_last_error() {
    let id = Uuid::new_v4();
    let port = Arc::new(StubWakeup {
        slow_dispatch: Some(StdDuration::from_millis(200)),
        ..StubWakeup::with_candidates(&[id])
    });
    let port_dyn: Arc<dyn WakeupDispatchPort> = port.clone();

    let err = wakeup::run_tick_with_budgets(
        &port_dyn,
        StdDuration::from_millis(10),
        StdDuration::from_millis(50),
    )
    .await
    .expect_err("超预算 ⇒ 有错");
    assert!(err.to_string().contains("budget"), "{err}");
    let noted = port.noted.lock().expect("lock").clone();
    assert_eq!(noted.len(), 1);
    assert!(noted[0].1.contains("budget"), "{}", noted[0].1);
    assert_eq!(port.touched.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn outcome_budget_does_not_abort_the_batch() {
    // 收尾慢掉：只记一条错，后续候选照跑（上游「一条忙规则不拖住整批」）。
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let port = Arc::new(StubWakeup {
        slow_touch: Some(StdDuration::from_millis(200)),
        ..StubWakeup::with_candidates(&[a, b])
    });
    let port_dyn: Arc<dyn WakeupDispatchPort> = port.clone();

    let err = wakeup::run_tick_with_budgets(
        &port_dyn,
        StdDuration::from_millis(500),
        StdDuration::from_millis(10),
    )
    .await
    .expect_err("收尾超时 ⇒ 有错");
    assert!(err.to_string().contains("touch dispatch"), "{err}");
    assert_eq!(
        port.touched.lock().expect("lock").len(),
        2,
        "两条都被尝试过（没有因为第一条超时就 break）"
    );
    assert_eq!(port.tick_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn candidate_listing_failure_aborts_the_tick() {
    let port = Arc::new(StubWakeup {
        list_fails: true,
        ..StubWakeup::with_candidates(&[Uuid::new_v4()])
    });
    let port_dyn: Arc<dyn WakeupDispatchPort> = port.clone();

    let err = wakeup::run_tick(&port_dyn)
        .await
        .expect_err("列候选失败 ⇒ 整 tick 立刻失败（上游不吞）");
    assert_eq!(err.code(), "handler_error");
    assert_eq!(port.touched.lock().expect("lock").len(), 0);
    assert_eq!(port.noted.lock().expect("lock").len(), 0);
}

#[tokio::test]
async fn wakeup_job_spec_matches_upstream_budgets() {
    let port: Arc<dyn WakeupDispatchPort> = Arc::new(StubWakeup::default());
    let spec = wakeup::job(port);

    assert_eq!(spec.name, wakeup::JOB_NAME);
    assert_eq!(spec.cadence, Duration::seconds(30));
    assert!(spec.plans_for_scope.is_none(), "无钩子：由 cadence 取整");
    assert_eq!(spec.catch_up_mode, CatchUpMode::LatestOnly);
    assert_eq!(spec.catch_up_window, Duration::hours(1));
    assert_eq!(spec.max_plans_per_tick, 1);
    assert_eq!(spec.run_timeout, StdDuration::from_secs(45));
    assert_eq!(spec.stale_timeout, StdDuration::from_secs(60));
    assert_eq!(spec.heartbeat_interval, StdDuration::from_secs(10));
    assert!(spec.allow_stale_reentry);
    assert_eq!(spec.max_attempts, 1, "上游 MaxAttempts: 1 ⇒ 不重试");
    assert!(spec.retry_backoff.is_empty(), "空退避表 = 不重试");
    spec.validate().expect("规格合法");

    let scopes = (spec.scopes)(Utc::now()).await.expect("列 scope");
    assert_eq!(
        scopes,
        vec![Scope::global()],
        "全局作用域（上游 StaticScopes）"
    );

    assert_eq!(wakeup::PER_RULE_BUDGET, StdDuration::from_secs(2));
    assert_eq!(wakeup::OUTCOME_BUDGET, StdDuration::from_millis(100));
}

// ---------------------------------------------------------------------------
// 手搓 JSON（本 crate 没有 serde_json）
// ---------------------------------------------------------------------------

#[test]
fn json_object_escapes_and_keeps_order() {
    let json = json_object(&[
        ("alpha", "one"),
        ("quote", "a\"b"),
        ("slash", "a\\b"),
        ("lines", "a\nb\tc\rd"),
        ("control", "a\u{1}b"),
        ("unicode", "中文·ok"),
    ]);
    assert!(json.starts_with("{\"alpha\":\"one\""), "{json}");
    assert!(json.ends_with('}'), "{json}");
    assert!(json.contains("\"quote\":\"a\\\"b\""), "{json}");
    assert!(json.contains("\"slash\":\"a\\\\b\""), "{json}");
    assert!(json.contains("\"lines\":\"a\\nb\\tc\\rd\""), "{json}");
    assert!(json.contains("\"control\":\"a\\u0001b\""), "{json}");
    assert!(json.contains("中文·ok"), "非 ASCII 原样输出（合法 JSON）");

    let mut numbers = JsonObject::new();
    numbers
        .text("code", "autopilot_inactive")
        .number("rows", 3)
        .number("negative", -1);
    assert_eq!(
        numbers.finish(),
        "{\"code\":\"autopilot_inactive\",\"rows\":3,\"negative\":-1}"
    );
    assert_eq!(JsonObject::new().finish(), "{}", "空对象仍是合法 JSON");

    // 计数汇总走同一条转义路径。
    let summary = wakeup::WakeupTickSummary {
        candidates: 2,
        dispatched: 1,
        waiting: 0,
        settled: 1,
        removed: 0,
        errors: 0,
    };
    assert_eq!(
        summary.to_json(),
        "{\"candidates\":2,\"dispatched\":1,\"waiting\":0,\"settled\":1,\"removed\":0,\"errors\":0}"
    );
}

// ---------------------------------------------------------------------------
// 真库 e2e（`#[ignore]`）
// ---------------------------------------------------------------------------
//
// **为什么只有一个用例**：wakeup job 的 (job, scope) 是全局固定的
// （`issue_wakeup_dispatch` + `Scope::global()`，上游同）⇒ 同一个 30s 桶在**同一 binary 内**
// 只能被一个用例跑。拆成两个用例并行跑会互相把对方的桶抢成 Conflicted，
// 于是「派发了几次」就成了竞态产物。这里合成一个用例：它独占这个桶，分两个半边断言
// （autopilot 那半边的 scope 是本用例新造的 uuid ⇒ 天然独占）。

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn real_db_register_all_wires_both_jobs_and_the_loop_starts_and_stops() {
    // 本片**碰不到** `apps/mc-server/src/main.rs`（P0：那里没有 `mc-scheduler` 依赖边），
    // 所以「两行注册 + 起停」这条 DoD 在这里用**真 `Manager` + 真后台循环**兑现：
    // `register_all` 就是 main.rs 里那两行的本体，`spawn` / `shutdown` 就是起停本体。
    let repo = repo().await;
    let trigger = fresh_trigger(Uuid::new_v4());
    let catalog = Arc::new(StubCatalog::healthy(
        trigger.clone(),
        autopilot_row(Uuid::new_v4(), "active"),
    ));
    let dispatch = Arc::new(StubDispatch::default());
    let candidates: Vec<Uuid> = (0..2).map(|_| Uuid::new_v4()).collect();
    let port = Arc::new(StubWakeup::with_candidates(&candidates));
    let port_dyn: Arc<dyn WakeupDispatchPort> = port.clone();
    let ports = mc_scheduler::jobs::JobPorts::new(catalog.clone(), dispatch.clone(), port.clone());

    let mut manager = Manager::new(
        repo.clone(),
        Options::default()
            .with_runner_id("itest-m5-8-register")
            .with_tick_interval(StdDuration::from_millis(50)),
    );
    mc_scheduler::jobs::register_all(&mut manager, &ports).expect("两行注册");
    // 两行注册的**直接证据**（不依赖时钟）：登记表里恰好是这两个 job，顺序与 `register_all` 一致。
    let names: Vec<&str> = manager.jobs().iter().map(|job| job.name.as_str()).collect();
    assert_eq!(names, vec![sched::JOB_NAME, wakeup::JOB_NAME]);
    // 同名二次注册必须被拒 —— 否则「接线时多写一行」会静默变成两个 job 抢同一个桶。
    assert!(
        mc_scheduler::jobs::register_all(&mut manager, &ports).is_err(),
        "重复注册同 job 名必须报错"
    );

    // wakeup 那半边的账户基线：spawn 前的最后一个桶（可能是上一次运行留下的 SUCCESS）。
    let before = db_ops::latest_plan(&repo, wakeup::JOB_NAME, &Scope::global())
        .await
        .expect("latest plan");

    let handle = manager.spawn();

    // autopilot 半边（确定性）：trigger id 是本用例新造的 ⇒ scope 唯一 ⇒ 桶不可能被别人占。
    let trigger_scope = Scope::new(sched::SCOPE_KIND, trigger.id.to_string());
    let deadline = std::time::Instant::now() + StdDuration::from_secs(10);
    loop {
        let auto = db_ops::latest_plan(&repo, sched::JOB_NAME, &trigger_scope)
            .await
            .expect("latest plan");
        if auto.found {
            assert_eq!(auto.status, ExecutionStatus::Success);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "10s 内后台循环没跑出 autopilot 的审计行（起循环失败）"
        );
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }
    assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1, "本桶一次派发");

    // wakeup 半边：要么本 tick 抢到了新桶（⇒ 每条候选恰好一次），要么桶已被跑成 SUCCESS
    // （⇒ 租约判 Conflicted，handler 一次都不该被调用）。两种结局都断言到位，不赌谁赢。
    let after = db_ops::latest_plan(&repo, wakeup::JOB_NAME, &Scope::global())
        .await
        .expect("latest plan");
    assert!(after.found, "wakeup 的审计行必须留痕");
    assert_eq!(after.status, ExecutionStatus::Success);
    let dispatched = port.dispatched.lock().expect("lock").clone();
    if after.plan_time > before.plan_time {
        assert_eq!(dispatched, candidates, "新桶 ⇒ 每条候选恰好派发一次");
        assert_eq!(port.tick_calls.load(Ordering::SeqCst), 1, "一次 tick");
        assert_eq!(port.touched.lock().expect("lock").len(), candidates.len());
    } else {
        assert!(
            dispatched.is_empty(),
            "旧桶（已 SUCCESS）⇒ 租约判 Conflicted，handler 不该被调用：{dispatched:?}"
        );
    }

    // DoD ④ 的确定性证据：同一个 SUCCESS 桶换实例再抢只能拿到 Conflicted（永不重跑）。
    let probe = wakeup::job(port_dyn);
    let now = repo.db_now().await.expect("db now");
    let again = db_ops::try_claim(
        &repo,
        &probe,
        &Scope::global(),
        after.plan_time,
        now,
        "itest-m5-8-b",
    )
    .await
    .expect("claim");
    assert_eq!(
        again.kind(),
        db_ops::ClaimKind::Conflicted,
        "SUCCESS 是终态 ⇒ 同一个桶绝不重跑"
    );

    // 停：`shutdown()` 的语义是「取消并等循环真的退出」⇒ 超时就说明停不下来。
    tokio::time::timeout(StdDuration::from_secs(5), handle.shutdown())
        .await
        .expect("shutdown 必须返回");
}
