//! `mc-repos::scheduler` 的**真库**集成测试（门禁 ⑥ 以 `--ignored` 跑）。
//!
//! 为什么在 `tests/` 而不在 `src/scheduler.rs` 里：R7 的**单文件 800 行硬上限**（门禁 ⑩）
//! —— 把租约 SQL 的契约文档写全之后，`scheduler.rs` 再塞 280 行测试就顶线了。集成测试同样
//! 属于 `cargo test -p mc-repos`，所以门禁 ⑥ 的覆盖一点没少。
//!
//! 这里只验 **SQL 层**的租约语义；内核层的四条验收（并发 `try_claim` / 偷租约后终态 0 行 /
//! 心跳超时 / 退避重试）在 `crates/mc-scheduler/tests/lease_db.rs`，两处不重复。
//!
//! 每个测试用**自己的 `job_name`**（带随机后缀）—— `sys_cron_executions` 是全局单例表，
//! 不让并行测试互相踩，也不依赖执行顺序。

use chrono::Utc;
use mc_db::Db;
use mc_repos::scheduler::{
    ExecutionStatus, FailureWrite, PlanKey, SchedulerRepo,
};
use uuid::Uuid;

/// 建库连接 + 本测试专用 job 名。
///
/// 本文件的用例都标了 `#[ignore]`，**只在显式跑真库时执行**，所以变量缺失时直接 panic
/// （而不是静默 skip）：门禁 ⑥ 把「跳过」变成「红」，防止假绿。
async fn setup() -> (Db, String) {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    let db = Db::connect(&url, 4, 1).await.expect("连接测试库");
    let job = format!("itest-m5-7-{}", Uuid::new_v4());
    (db, job)
}

async fn teardown(db: &Db, job_name: &str) {
    let _ = sqlx::query("DELETE FROM sys_cron_executions WHERE job_name = $1")
        .bind(job_name)
        .execute(db.pool())
        .await;
}

/// 把某行的 `stale_after` 推到过去 —— 「心跳断了」的构造方式（不用睡眠）。
async fn force_stale(db: &Db, id: Uuid) {
    sqlx::query(
        "UPDATE sys_cron_executions \
            SET stale_after = now() - interval '1 second' WHERE id = $1",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .expect("force stale");
}

/// ① 并发两个新鲜认领：唯一键 + `ON CONFLICT DO NOTHING` ⇒ 只有一个拿到租约。
#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn concurrent_fresh_claim_has_single_winner() {
    let (db, job) = setup().await;
    let repo = SchedulerRepo::new(db.clone());
    let now = repo.db_now().await.expect("db now");
    let key = PlanKey::new(&job, "global", "global", now);

    let (a, b) = tokio::join!(
        repo.claim_fresh(&key, 3, "r-a", now, 30.0, None),
        repo.claim_fresh(&key, 3, "r-b", now, 30.0, None),
    );
    let a = a.expect("claim a");
    let b = b.expect("claim b");
    assert!(
        a.is_some() ^ b.is_some(),
        "并发认领必须恰好一个赢：a={a:?} b={b:?}"
    );

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sys_cron_executions \
         WHERE job_name = $1 AND status = 'RUNNING'",
    )
    .bind(&job)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(rows, 1, "同一 plan_time 只允许一行 RUNNING");

    teardown(&db, &job).await;
}

/// ② 租约被窃取后，**旧持有者**的终态写入必须影响 0 行（不能覆盖新一轮状态）。
#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn stolen_lease_makes_old_holder_terminal_write_a_noop() {
    let (db, job) = setup().await;
    let repo = SchedulerRepo::new(db.clone());
    let now = repo.db_now().await.expect("db now");
    let key = PlanKey::new(&job, "global", "global", now);

    let old = repo
        .claim_fresh(&key, 3, "r-old", now, 30.0, None)
        .await
        .expect("claim")
        .expect("fresh claim should win");
    force_stale(&db, old.id).await;

    let stolen = repo
        .claim_steal_or_retry(&key, "r-new", now, 30.0, true)
        .await
        .expect("steal")
        .expect("stale lease should be stealable when allow_stale_reentry");
    assert_eq!(stolen.id, old.id, "窃取的是同一行");
    assert_ne!(stolen.lease_token, old.lease_token, "token 必须轮换");
    assert_eq!(stolen.attempt, 2, "attempt 递增");

    // 旧 token：三条守卫路径（心跳 / SUCCESS / FAILED）都必须是 0 行。
    let old_lease = old.lease();
    assert!(
        !repo
            .finish_success(old_lease, now, 1, 0, None)
            .await
            .expect("finish success"),
        "旧持有者不得写 SUCCESS"
    );
    assert!(
        !repo
            .finish_failure(
                old_lease,
                now,
                1,
                &FailureWrite {
                    next_retry_at: None,
                    error_code: "handler_error",
                    error_msg: "y",
                    attempt_override: None,
                },
            )
            .await
            .expect("finish failure"),
        "旧持有者不得写 FAILED"
    );
    assert!(
        !repo.heartbeat(old_lease, 30.0).await.expect("old hb"),
        "旧持有者不得续期"
    );

    // 新 token 仍然有效（守卫只挡旧的那个）。
    let new_lease = stolen.lease();
    assert!(
        repo.heartbeat(new_lease, 30.0).await.expect("new hb"),
        "新持有者可以续期"
    );
    assert!(
        repo.finish_success(new_lease, now, 1, 0, None)
            .await
            .expect("new finish"),
        "新持有者可以写终态"
    );

    teardown(&db, &job).await;
}

/// ③ 心跳超时的 `RUNNING` 行被 `mark_stale_as_failed` 收成 `FAILED`。
///
/// 注意：`allow_stale_reentry = false` 的 job 不能被偷，只能走这条回收路径；
/// 而**每个 job 每个 tick 都会跑这一步**（与 reentrant 无关）。
#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn stale_lease_is_closed_as_failed() {
    let (db, job) = setup().await;
    let repo = SchedulerRepo::new(db.clone());
    let now = repo.db_now().await.expect("db now");
    let key = PlanKey::new(&job, "global", "global", now);

    let lease = repo
        .claim_fresh(&key, 3, "r-1", now, 30.0, None)
        .await
        .expect("claim")
        .expect("fresh claim");
    force_stale(&db, lease.id).await;

    // 非 reentrant：陈旧也不能偷。
    assert!(
        repo.claim_steal_or_retry(&key, "r-2", now, 30.0, false)
            .await
            .expect("steal attempt")
            .is_none(),
        "allow_stale_reentry=false 不得窃取"
    );

    let later = repo.db_now().await.expect("db now 2");
    let closed = repo
        .mark_stale_as_failed(&job, later)
        .await
        .expect("mark stale");
    assert_eq!(closed, 1, "应恰好收掉一行");

    let info = repo.latest_plan(&job, "global", "global").await.expect("latest");
    assert!(info.found);
    assert_eq!(info.status, ExecutionStatus::Failed);
    assert!(info.retry_eligible(later), "退避为 NULL ⇒ 尽快可重试");

    let code: Option<String> =
        sqlx::query_scalar("SELECT error_code FROM sys_cron_executions WHERE id = $1")
            .bind(lease.id)
            .fetch_one(db.pool())
            .await
            .expect("read error_code");
    assert_eq!(code.as_deref(), Some("stale_timeout"));

    teardown(&db, &job).await;
}

/// `latest_plan` 在无历史时给空视图（不是错误），并只取最新一行。
#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn latest_plan_is_empty_then_tracks_newest_bucket() {
    let (db, job) = setup().await;
    let repo = SchedulerRepo::new(db.clone());
    let now = repo.db_now().await.expect("db now");

    let empty = repo.latest_plan(&job, "global", "global").await.expect("latest");
    assert!(!empty.found);
    assert!(!empty.retry_eligible(now));
    assert_eq!(empty.plan_time, chrono::DateTime::<Utc>::MIN_UTC);

    for offset in [0_i64, 3600] {
        let plan_time = now + chrono::Duration::seconds(offset);
        repo.claim_fresh(
            &PlanKey::new(&job, "global", "global", plan_time),
            3,
            "r",
            plan_time,
            30.0,
            None,
        )
        .await
        .expect("claim")
        .expect("fresh");
    }
    let info = repo.latest_plan(&job, "global", "global").await.expect("latest");
    assert!(info.found);
    assert_eq!(info.status, ExecutionStatus::Running);
    assert!(
        !info.retry_eligible(now),
        "RUNNING 不是重试候选（它只是『还在跑』）"
    );

    teardown(&db, &job).await;
}
