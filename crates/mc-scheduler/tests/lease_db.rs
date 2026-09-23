//! M5-7 内核的**真库**测试：租约语义端到端（认领 / 重试退避 / 永久失败烧预算 /
//! `every_plan` 游标 / 陈旧窃取 / 心跳续期 / 关闭时中止 handler）。
//!
//! 跑法（全部 `#[ignore]`）：
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://… \
//!   cargo test -p mc-scheduler --test lease_db -- --ignored --test-threads=1
//! ```
//!
//! 前置：`cargo run -p mc-migrate -- run --dir migrations`（`sys_cron_executions` 来自迁移 `113`）。
//!
//! 说明：
//!
//! * 每个用例用**唯一** job 名（`itest-m5-7-<用途>-<uuid>`）⇒ 用例之间零干扰，并行也安全。
//! * 跑完的审计行**不删**：`mc-scheduler` 没有 `sqlx` 依赖，写不了裸 SQL（这边不为了清理
//!   去加依赖边）。仓储层的用例在 `mc-repos/tests/scheduler_lease_db.rs`，那边有 sqlx、会清理。
//! * 本文件是**手工**跑的门禁（`scripts/gates.sh` 的 ⑥ 只覆盖 `mc-repos` / `mc-http`），
//!   见 `docs/46` §6 的读数与 §7 的待办。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use mc_repos::scheduler::ExecutionStatus;
use mc_scheduler::db_ops::{self, Claim, ClaimKind, Heartbeat};
use mc_scheduler::error::SchedulerError;
use mc_scheduler::spec::{
    global_scopes, CatchUpMode, Handler, HandlerInput, HandlerResult, JobSpec, Scope,
};
use mc_scheduler::{Manager, Options, SchedulerRepo};

/// handler 记下的 `(plan_time, attempt)` 轨迹。
type Seen = Arc<Mutex<Vec<(DateTime<Utc>, i32)>>>;

/// 连接测试库（没有 URL 就显式失败，不静默跳过）。
async fn repo() -> SchedulerRepo {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    SchedulerRepo::connect(&url, 4, 1)
        .await
        .expect("connect scheduler repo")
}

/// 每个用例一个唯一 job 名（隔离 + 便于事后按名字查审计行）。
fn unique_job(purpose: &str) -> String {
    format!("itest-m5-7-{purpose}-{}", Uuid::new_v4())
}

/// 轮询等待某个 `AtomicBool` 变真。
async fn wait_for_flag(flag: &AtomicBool, timeout: StdDuration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !flag.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "标志位没有在超时前置位"
        );
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
}

/// 记录调用次数与 `(plan_time, attempt)`，然后恒成功。
fn recording_handler(calls: Arc<AtomicUsize>, seen: Seen) -> Handler {
    Arc::new(move |input: HandlerInput| {
        let (calls, seen) = (calls.clone(), seen.clone());
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            seen.lock()
                .expect("lock")
                .push((input.plan_time, input.attempt));
            Ok(HandlerResult::rows(7))
        })
    })
}

/// 恒失败（业务错误 ⇒ 可重试，走 `retry_backoff`）。
fn failing_handler(code: &'static str) -> Handler {
    Arc::new(move |_input: HandlerInput| {
        Box::pin(async move { Err(SchedulerError::Handler(code.to_owned())) })
    })
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn manager_runs_a_claimed_job_exactly_once() {
    let repo = repo().await;
    let name = unique_job("once");
    let calls = Arc::new(AtomicUsize::new(0));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let spec = JobSpec::new(
        name.clone(),
        Duration::minutes(5),
        global_scopes(),
        recording_handler(calls.clone(), seen.clone()),
    )
    .with_timing(
        StdDuration::from_secs(60),
        StdDuration::from_secs(300),
        StdDuration::from_secs(30),
    )
    .with_retry(3, vec![Duration::seconds(10)]);
    let probe = spec.clone();

    let mut manager = Manager::new(repo.clone(), Options::default().with_runner_id("itest-a"));
    manager.register(spec).expect("register");
    manager.run_once().await.expect("tick");

    assert_eq!(calls.load(Ordering::SeqCst), 1, "一个 tick 只认领一次");
    let (plan_time, attempt) = seen.lock().expect("lock")[0];
    assert_eq!(attempt, 1, "首次尝试");

    let info = db_ops::latest_plan(&repo, &name, &Scope::global())
        .await
        .expect("latest plan");
    assert!(info.found);
    assert_eq!(info.status, ExecutionStatus::Success);
    assert_eq!(info.plan_time, plan_time);
    assert_eq!(info.attempt, 1);
    assert!(info.next_retry_at.is_none(), "成功不留待重试");

    // 同一个 plan_time 再抢：SUCCESS 是终态 ⇒ Conflicted（绝不重跑同一个桶）。
    let now = repo.db_now().await.expect("db now");
    let again = db_ops::try_claim(&repo, &probe, &Scope::global(), plan_time, now, "itest-b")
        .await
        .expect("claim");
    assert_eq!(again.kind(), ClaimKind::Conflicted);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn failure_schedules_a_backoff_retry_that_blocks_early_reclaim() {
    let repo = repo().await;
    let name = unique_job("backoff");
    let spec = JobSpec::new(
        name.clone(),
        Duration::minutes(5),
        global_scopes(),
        failing_handler("boom"),
    )
    .with_timing(
        StdDuration::from_secs(60),
        StdDuration::from_secs(300),
        StdDuration::from_secs(30),
    )
    .with_retry(3, vec![Duration::seconds(30)]);
    let probe = spec.clone();

    let mut manager = Manager::new(repo.clone(), Options::default().with_runner_id("itest-a"));
    manager.register(spec).expect("register");
    manager.run_once().await.expect("tick");

    let now = repo.db_now().await.expect("db now");
    let info = db_ops::latest_plan(&repo, &name, &Scope::global())
        .await
        .expect("latest plan");
    assert_eq!(info.status, ExecutionStatus::Failed);
    assert_eq!(info.attempt, 1);
    assert_eq!(info.max_attempts, 3);
    let next_retry = info.next_retry_at.expect("可重试的失败必须给出下次时间");
    assert!(
        next_retry >= now + Duration::seconds(29),
        "退避应≈30s：下次 {next_retry} vs 现在 {now}"
    );
    assert!(!info.retry_eligible(now), "退避未到 ⇒ 本 tick 不能重跑");

    // 退避未到 ⇒ 再抢同一个桶一律输（这是「不重试风暴」的可观测证据）。
    let again = db_ops::try_claim(
        &repo,
        &probe,
        &Scope::global(),
        info.plan_time,
        now,
        "itest-b",
    )
    .await
    .expect("claim");
    assert_eq!(again.kind(), ClaimKind::Conflicted);
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn permanent_failure_burns_the_retry_budget() {
    let repo = repo().await;
    let name = unique_job("permanent");
    let handler: Handler = Arc::new(|_input: HandlerInput| {
        Box::pin(async {
            Err(SchedulerError::Permanent {
                code: "invalid_cron".to_owned(),
                message: "boom".to_owned(),
            })
        })
    });
    let spec = JobSpec::new(name.clone(), Duration::minutes(5), global_scopes(), handler)
        .with_timing(
            StdDuration::from_secs(60),
            StdDuration::from_secs(300),
            StdDuration::from_secs(30),
        )
        .with_retry(5, vec![Duration::seconds(1)]);
    let probe = spec.clone();

    let mut manager = Manager::new(repo.clone(), Options::default().with_runner_id("itest-a"));
    manager.register(spec).expect("register");
    manager.run_once().await.expect("tick");

    let now = repo.db_now().await.expect("db now");
    let info = db_ops::latest_plan(&repo, &name, &Scope::global())
        .await
        .expect("latest plan");
    assert_eq!(info.status, ExecutionStatus::Failed);
    assert_eq!(info.attempt, 5, "不可重试 ⇒ 一次烧满预算");
    assert!(info.next_retry_at.is_none());
    assert!(!info.retry_eligible(now));

    let again = db_ops::try_claim(
        &repo,
        &probe,
        &Scope::global(),
        info.plan_time,
        now,
        "itest-b",
    )
    .await
    .expect("claim");
    assert_eq!(
        again.kind(),
        ClaimKind::Conflicted,
        "预算用尽 ⇒ 永久不再重试"
    );
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn every_plan_returns_to_the_failed_bucket_when_its_backoff_has_burned() {
    let repo = repo().await;
    let name = unique_job("cursor");
    let calls = Arc::new(AtomicUsize::new(0));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let mut manager = Manager::new(repo.clone(), Options::default().with_runner_id("itest-a"));
    // 空退避表 ⇒ `retryDelay` 为 0 ⇒ `next_retry_at = db_time` ⇒ 下一 tick 立刻可重试。
    let handler: Handler = Arc::new({
        let (calls, seen) = (calls.clone(), seen.clone());
        move |input: HandlerInput| {
            let (calls, seen) = (calls.clone(), seen.clone());
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                seen.lock()
                    .expect("lock")
                    .push((input.plan_time, input.attempt));
                Err(SchedulerError::Handler("boom".to_owned()))
            })
        }
    });
    let spec = JobSpec::new(name.clone(), Duration::minutes(1), global_scopes(), handler)
        .with_catch_up(CatchUpMode::EveryPlan, Duration::zero(), 1)
        .with_timing(
            StdDuration::from_secs(60),
            StdDuration::from_secs(300),
            StdDuration::from_secs(30),
        )
        .with_retry(3, Vec::new());
    manager.register(spec).expect("register");

    manager.run_once().await.expect("first tick");
    manager.run_once().await.expect("second tick");

    let attempts = seen.lock().expect("lock").clone();
    assert_eq!(attempts.len(), 2, "两个 tick 各跑一次：{attempts:?}");
    assert_eq!(
        attempts[0].0, attempts[1].0,
        "FAILED 桶还会重试 ⇒ 游标不能跳过它"
    );
    assert_eq!((attempts[0].1, attempts[1].1), (1, 2));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn stale_lease_is_stolen_and_the_old_holder_gets_lease_lost() {
    let repo = repo().await;
    let name = unique_job("steal");
    // `stale_secs` 向下取整到整秒且下限 1 ⇒ 500ms 的窗口实际是 1s。
    let spec = JobSpec::new(
        name.clone(),
        Duration::minutes(5),
        global_scopes(),
        failing_handler("unused"),
    )
    .with_timing(
        StdDuration::from_millis(100),
        StdDuration::from_millis(500),
        StdDuration::from_millis(200),
    )
    .with_retry(3, Vec::new())
    .with_allow_stale_reentry(true);

    let scope = Scope::global();
    let plan_time = repo.db_now().await.expect("db now");
    let first = db_ops::try_claim(&repo, &spec, &scope, plan_time, plan_time, "itest-a")
        .await
        .expect("claim");
    let Claim::Won(holder_a) = first else {
        panic!("首次认领应为 Won：{first:?}");
    };
    let heartbeat_a = Heartbeat::new(repo.clone(), holder_a.lease, StdDuration::from_millis(500));
    heartbeat_a.beat().await.expect("没过期的持有者能续期");

    // 熬过陈旧窗口（1s）后再抢：允许偷 ⇒ 应当抢到，且是**同一行**的 UPDATE。
    tokio::time::sleep(StdDuration::from_millis(1_200)).await;
    let now = repo.db_now().await.expect("db now");
    let second = db_ops::try_claim(&repo, &spec, &scope, plan_time, now, "itest-b")
        .await
        .expect("claim");
    let Claim::Stole(holder_b) = second else {
        panic!("陈旧租约应可被偷：{second:?}");
    };
    assert_eq!(
        holder_b.lease.id, holder_a.lease.id,
        "窃取是同一行的 UPDATE"
    );
    assert_eq!(holder_b.attempt, holder_a.attempt + 1);
    assert_ne!(
        holder_b.lease.lease_token, holder_a.lease.lease_token,
        "每次认领轮换令牌"
    );

    // 旧持有者：心跳与终态写入都影响 0 行 ⇒ 必须报 LeaseLost（而不是静默成功）。
    assert!(matches!(
        heartbeat_a.beat().await,
        Err(SchedulerError::LeaseLost(_))
    ));
    assert!(matches!(
        db_ops::finish_success(&repo, holder_a.lease, now, 5, &HandlerResult::rows(1)).await,
        Err(SchedulerError::LeaseLost(_))
    ));
    assert!(matches!(
        db_ops::finish_failure(
            &repo,
            holder_a.lease,
            now,
            5,
            &mc_repos::scheduler::FailureWrite {
                next_retry_at: None,
                error_code: "stale_timeout",
                error_msg: "old holder",
                attempt_override: None,
            },
        )
        .await,
        Err(SchedulerError::LeaseLost(_))
    ));

    // 新持有者照常写终态。
    db_ops::finish_success(&repo, holder_b.lease, now, 5, &HandlerResult::rows(1))
        .await
        .expect("新持有者写 SUCCESS");
    let info = db_ops::latest_plan(&repo, &name, &scope)
        .await
        .expect("latest plan");
    assert_eq!(info.status, ExecutionStatus::Success);
    assert_eq!(info.attempt, 2);
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn heartbeat_renewal_pushes_the_stale_window_forward() {
    let repo = repo().await;
    let name = unique_job("renew");
    // `stale_secs` 向下取整到整秒且下限 1 ⇒ 500ms 的窗口实际是 1s。
    // 注意：规格校验要求 `run_timeout < stale_timeout`，所以「活着的 handler 比窗口活得久」
    // 这种场景在合法规格下**造不出来**（那种 handler 会先撞 `run_timeout`）。心跳的可观测
    // 效果因此只能这样验：直接持租约续期，看「本应过期」的时点上别人还偷不偷得走。
    let spec = JobSpec::new(
        name.clone(),
        Duration::minutes(5),
        global_scopes(),
        failing_handler("unused"),
    )
    .with_timing(
        StdDuration::from_millis(100),
        StdDuration::from_millis(500),
        StdDuration::from_millis(200),
    )
    .with_retry(3, Vec::new())
    .with_allow_stale_reentry(true);

    let scope = Scope::global();
    let plan_time = repo.db_now().await.expect("db now");
    let claim = db_ops::try_claim(&repo, &spec, &scope, plan_time, plan_time, "itest-a")
        .await
        .expect("claim");
    let Claim::Won(holder) = claim else {
        panic!("首次认领应为 Won：{claim:?}");
    };
    let heartbeat = Heartbeat::new(repo.clone(), holder.lease, StdDuration::from_millis(500));

    // t≈0.5s 续期 ⇒ 窗口推到 ≈1.5s；t≈1.0s 再续 ⇒ ≈2.0s。
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    heartbeat.beat().await.expect("续期");
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    heartbeat.beat().await.expect("续期");

    // t≈1.2s：已越过**未续期**的 1s 窗口（那个对照在
    // `stale_lease_is_stolen_and_the_old_holder_gets_lease_lost` 里验），但续期后仍不算陈旧。
    tokio::time::sleep(StdDuration::from_millis(200)).await;
    let now = repo.db_now().await.expect("db now");
    let stolen = db_ops::try_claim(&repo, &spec, &scope, plan_time, now, "itest-b")
        .await
        .expect("claim");
    assert_eq!(
        stolen.kind(),
        ClaimKind::Conflicted,
        "心跳在续期 ⇒ 租约不该在 1s 陈旧窗口后被偷"
    );

    let info = db_ops::latest_plan(&repo, &name, &scope)
        .await
        .expect("latest plan");
    assert_eq!(info.status, ExecutionStatus::Running);
    assert_eq!(info.attempt, 1, "没有被重跑");
}

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn shutdown_stops_the_loop_and_aborts_the_running_handler() {
    let repo = repo().await;
    let name = unique_job("shutdown");
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let handler: Handler = Arc::new({
        let (started, finished) = (started.clone(), finished.clone());
        move |_input: HandlerInput| {
            let (started, finished) = (started.clone(), finished.clone());
            Box::pin(async move {
                started.store(true, Ordering::SeqCst);
                tokio::time::sleep(StdDuration::from_secs(1)).await;
                // 只有**没被 abort** 才会走到这里。
                finished.store(true, Ordering::SeqCst);
                Ok(HandlerResult::rows(1))
            })
        }
    });
    let spec = JobSpec::new(name.clone(), Duration::minutes(5), global_scopes(), handler)
        .with_timing(
            StdDuration::from_secs(10),
            StdDuration::from_secs(60),
            StdDuration::from_secs(20),
        )
        .with_retry(3, Vec::new());

    let mut manager = Manager::new(
        repo.clone(),
        Options::default()
            .with_runner_id("itest-a")
            .with_tick_interval(StdDuration::from_millis(50)),
    );
    manager.register(spec).expect("register");
    let handle = manager.spawn();

    wait_for_flag(&started, StdDuration::from_secs(5)).await;
    tokio::time::timeout(StdDuration::from_millis(500), handle.shutdown())
        .await
        .expect("shutdown 必须立刻返回，不能等 handler 跑完");

    tokio::time::sleep(StdDuration::from_millis(1_200)).await;
    assert!(
        !finished.load(Ordering::SeqCst),
        "关闭必须 abort 掉在跑的 handler"
    );

    let info = db_ops::latest_plan(&repo, &name, &Scope::global())
        .await
        .expect("latest plan");
    assert_eq!(
        info.status,
        ExecutionStatus::Running,
        "被中止的 handler 不写终态"
    );
    assert_eq!(info.attempt, 1);
    let now = repo.db_now().await.expect("db now");
    assert!(!info.retry_eligible(now), "在飞的行不算「可重试」");
    // 回收只能走陈旧路径，且**只在窗口过了之后**（本 job 的窗口是 60s）。
    let reaped = db_ops::mark_stale_as_failed(&repo, &name, now)
        .await
        .expect("reap");
    assert_eq!(reaped, 0, "窗口没过 ⇒ 不能回收在飞的行");
}
