//! `crate::autopilot::run` 的真库语义（续）：终态结算 / issue 链 / 任务栅栏 / 读面收窄。
//!
//! 从 `run.rs` 拆出来的：门 ⑩ 的单文件 800 行上限。夹具（`Seeded` / `cleanup` / `new_run` /
//! `new_task` / `seed_reservation`）留在 `run.rs` 并以 `pub(super)` 暴露，不复制第二份。

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use uuid::Uuid;

use super::run::{cleanup, new_run, new_task, seed, seed_reservation};
use super::setup;
use crate::autopilot::quota::{get_period, get_reservation_by_key};
use crate::autopilot::run::{self as run_sql, NewAutopilotTask};
use crate::RepoError;

/// 终态结算在**同一条语句**里完成：`consume=true` ⇒ 预留 consumed 且 `used_count+1`；
/// `consume=false` ⇒ released 且 `used_count` 不动（失败分支不写 `result`）。
#[allow(clippy::too_many_lines)] // 103 行：消费 / 退回两条分支各自要断言 run + 周期 + 预留三处读数
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn terminal_settlement_consumes_or_releases_the_reservation() {
    let Some(fixture) = setup().await else {
        println!("skip terminal_settlement_consumes_or_releases_the_reservation: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let seeded = seed(pool, ws).await;
    let start = Utc::now() - Duration::hours(1);
    let end = start + Duration::days(30);
    let bounds = (start, end);
    let mut conn = pool.acquire().await.expect("conn");

    // ① 成功：`completed` 消费预留。
    let consumed = seed_reservation(pool, ws, "idem-consume", bounds).await;
    let mut run = new_run(&seeded, "running");
    run.quota_reservation_id = Some(consumed);
    let run = run_sql::create_run(&mut conn, &run).await.expect("run");
    let updated = run_sql::update_terminal_with_quota(
        &mut conn,
        run.id,
        "completed",
        Some(&json!({"ok": true})),
        None,
        None,
        true,
    )
    .await
    .expect("settle completed");
    assert_eq!(updated.status, "completed");
    assert_eq!(updated.result, Some(json!({"ok": true})));
    assert!(updated.completed_at.is_some());
    assert_eq!(
        get_reservation_by_key(pool, ws, start, end, "idem-consume")
            .await
            .expect("lookup")
            .expect("reservation")
            .state,
        "consumed"
    );

    // ② 失败：预留退回、`failure_reason` / `reason_code` 落库、`result` 保持 NULL。
    let released = seed_reservation(pool, ws, "idem-release", bounds).await;
    let mut run = new_run(&seeded, "running");
    run.quota_reservation_id = Some(released);
    let run = run_sql::create_run(&mut conn, &run).await.expect("run");
    let updated = run_sql::update_terminal_with_quota(
        &mut conn,
        run.id,
        "failed",
        Some(&json!({"ignored": true})),
        Some("boom"),
        Some("internal_error"),
        false,
    )
    .await
    .expect("settle failed");
    assert_eq!(updated.status, "failed");
    assert_eq!(updated.failure_reason.as_deref(), Some("boom"));
    assert_eq!(updated.reason_code.as_deref(), Some("internal_error"));
    assert!(
        updated.result.is_none(),
        "失败分支不写 result（即使调用方传了）"
    );
    assert!(
        get_reservation_by_key(pool, ws, start, end, "idem-release")
            .await
            .expect("lookup")
            .is_none(),
        "退回 ⇒ state='released' ⇒ 幂等键腾空"
    );
    let released_state: String =
        sqlx::query_scalar("SELECT state FROM autopilot_quota_reservation WHERE id = $1")
            .bind(released)
            .fetch_one(pool)
            .await
            .expect("reservation row");
    assert_eq!(released_state, "released");

    // 周期账：一格被消费、一格退回 ⇒ `used=1`、`reserved=0`。
    let period = get_period(pool, ws, start, end)
        .await
        .expect("period")
        .expect("row");
    assert_eq!(period.used_count, 1);
    assert_eq!(period.reserved_count, 0);

    // ③ 没有预留的 run（配额面关闭时的正常路径）：照常结算，不碰配额表。
    let plain = run_sql::create_run(&mut conn, &new_run(&seeded, "running"))
        .await
        .expect("run");
    let updated = run_sql::update_terminal_with_quota(
        &mut conn,
        plain.id,
        "skipped",
        None,
        Some("no agent"),
        Some("target_unavailable"),
        false,
    )
    .await
    .expect("settle skipped");
    assert_eq!(updated.status, "skipped");
    assert_eq!(updated.reason_code.as_deref(), Some("target_unavailable"));
    let period = get_period(pool, ws, start, end)
        .await
        .expect("period")
        .expect("row");
    assert_eq!(period.used_count, 1);
    assert_eq!(period.reserved_count, 0);

    drop(conn);
    cleanup(&fixture, &seeded).await;
}

/// `find_active_by_issue` 的 `status IN ('issue_created','running')` 是「重复回调天然幂等」那道闸；
/// `fail_by_issue` 只吃在飞 run，并退掉**仍 reserved** 的预留（已消费的不退）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn issue_scoped_queries_only_see_in_flight_runs() {
    let Some(fixture) = setup().await else {
        println!("skip issue_scoped_queries_only_see_in_flight_runs: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let seeded = seed(pool, ws).await;
    let start = Utc::now() - Duration::hours(1);
    let end = start + Duration::days(30);
    let bounds = (start, end);
    let mut conn = pool.acquire().await.expect("conn");

    // 终态 run 挂在这个 issue 上 ⇒ `find_active_by_issue` 看不见它。
    let done = run_sql::create_run(&mut conn, &new_run(&seeded, "completed"))
        .await
        .expect("run");
    run_sql::update_issue_created(&mut conn, done.id, seeded.issue_id)
        .await
        .expect("link issue");
    run_sql::update_terminal_with_quota(&mut conn, done.id, "completed", None, None, None, true)
        .await
        .expect("complete");
    assert!(
        run_sql::find_active_by_issue(&mut conn, seeded.issue_id)
            .await
            .expect("lookup")
            .is_none(),
        "终态 run 不该被 find_active_by_issue 看见"
    );

    // 在飞 run + 仍 reserved 的预留 ⇒ 命中，且失败原因逐字是上游文案。
    let reservation_id = seed_reservation(pool, ws, "idem-issue", bounds).await;
    let mut live = new_run(&seeded, "issue_created");
    live.quota_reservation_id = Some(reservation_id);
    let live = run_sql::create_run(&mut conn, &live)
        .await
        .expect("live run");
    run_sql::update_issue_created(&mut conn, live.id, seeded.issue_id)
        .await
        .expect("link issue");
    assert_eq!(
        run_sql::find_active_by_issue(&mut conn, seeded.issue_id)
            .await
            .expect("lookup")
            .expect("active run")
            .id,
        live.id
    );

    let failed = run_sql::fail_by_issue(&mut conn, seeded.issue_id)
        .await
        .expect("fail_by_issue");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].id, live.id);
    assert_eq!(failed[0].status, "failed");
    assert_eq!(
        failed[0].failure_reason.as_deref(),
        Some("linked issue was deleted")
    );
    let period = get_period(pool, ws, start, end)
        .await
        .expect("period")
        .expect("row");
    assert_eq!(period.reserved_count, 0, "仍 reserved 的预留必须退掉");
    assert_eq!(period.used_count, 0, "退预留不算消费");

    // 再调一次没有在飞 run ⇒ 空数组（幂等收尾）。
    assert!(run_sql::fail_by_issue(&mut conn, seeded.issue_id)
        .await
        .expect("second call")
        .is_empty());
    // `find_active_by_issue` 收窄用的状态集合就是这两态（写面唯一的真相）。
    assert_eq!(run_sql::ACTIVE_RUN_STATUSES, "'issue_created', 'running'");

    drop(conn);
    cleanup(&fixture, &seeded).await;
}

/// 归属栅栏 `lock_task_owner_rows`：owner 行缺失 ⇒ 写零行（`None`）而**不是** FK 报错；
/// owner 齐备 ⇒ 正常入队（`queued`、落在 leader 名下），且 run↔task 双向对得上。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn create_task_is_refused_when_an_owner_row_is_gone() {
    let Some(fixture) = setup().await else {
        println!("skip create_task_is_refused_when_an_owner_row_is_gone: no env");
        return;
    };
    let pool = fixture.db.pool();
    let seeded = seed(pool, fixture.workspace_id).await;
    let mut conn = pool.acquire().await.expect("conn");

    // ① 不存在的 agent ⇒ 栅栏拒写（`None`），而不是撞 `agent_id` 外键报 500。
    assert!(
        run_sql::create_task(&mut conn, &new_task(&seeded, Uuid::new_v4(), None, None))
            .await
            .expect("fence query")
            .is_none(),
        "agent 行缺失必须是可判别的 None"
    );
    // ② 不存在的 issue 同理（`create_issue` 线挂着 issue 那一半）。
    assert!(
        run_sql::create_task(
            &mut conn,
            &new_task(
                &seeded,
                seeded.agent_id,
                Some(Uuid::new_v4()),
                Some(seeded.runtime_id)
            )
        )
        .await
        .expect("fence query")
        .is_none(),
        "issue 行缺失必须是可判别的 None"
    );

    // ③ owner 齐备 ⇒ 落库。
    let task_id = run_sql::create_task(
        &mut conn,
        &new_task(
            &seeded,
            seeded.agent_id,
            Some(seeded.issue_id),
            Some(seeded.runtime_id),
        ),
    )
    .await
    .expect("fence query")
    .expect("fence allows the happy path");
    let row: (Uuid, String, Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT agent_id, status, issue_id, originator_source FROM agent_task_queue WHERE id = $1",
    )
    .bind(task_id)
    .fetch_one(&mut *conn)
    .await
    .expect("task row");
    assert_eq!(row.0, seeded.agent_id);
    assert_eq!(row.1, "queued");
    assert_eq!(row.2, Some(seeded.issue_id));
    assert_eq!(row.3.as_deref(), Some("direct_human"));

    // ④ `has_active_task_for_issue` 认 `queued`。
    assert!(
        run_sql::has_active_task_for_issue(&mut conn, seeded.issue_id)
            .await
            .expect("has_active")
    );

    // ⑤ run_only 线（任务挂 run、不挂 issue）⇒ 双向查询对得上。
    //    `agent_task_queue` 的 CHECK 要求 `queued` 任务必须绑 runtime。
    let run = run_sql::create_run(&mut conn, &new_run(&seeded, "running"))
        .await
        .expect("run");
    let linked = run_sql::create_task(
        &mut conn,
        &NewAutopilotTask {
            autopilot_run_id: Some(run.id),
            ..new_task(&seeded, seeded.agent_id, None, Some(seeded.runtime_id))
        },
    )
    .await
    .expect("fence query")
    .expect("fence allows");
    assert_eq!(
        run_sql::find_task_id_by_run(pool, run.id)
            .await
            .expect("by run"),
        Some(linked)
    );
    assert_eq!(
        run_sql::find_run_id_by_task(pool, linked)
            .await
            .expect("by task"),
        Some(run.id)
    );
    assert!(run_sql::find_task_id_by_run(pool, Uuid::new_v4())
        .await
        .expect("unknown run")
        .is_none());

    drop(conn);
    cleanup(&fixture, &seeded).await;
}

/// 读面的 workspace 收窄与排序：`get`/`get_in_workspace` 跨工作区一律 `NotFound`，
/// `list` 是 `created_at DESC` 且 limit/offset 真的生效。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn run_reads_are_workspace_scoped_and_newest_first() {
    let Some(fixture) = setup().await else {
        println!("skip run_reads_are_workspace_scoped_and_newest_first: no env");
        return;
    };
    let pool = fixture.db.pool();
    let seeded = seed(pool, fixture.workspace_id).await;
    let mut conn = pool.acquire().await.expect("conn");

    let mut ids = Vec::new();
    for index in 0..3_i32 {
        let run = run_sql::create_run(&mut conn, &new_run(&seeded, "issue_created"))
            .await
            .expect("run");
        // 拉开 `created_at`：同一个事务里 `now()` 是同一个时间戳。
        sqlx::query(
            "UPDATE autopilot_run SET created_at = now() - $2 * interval '1 second' WHERE id = $1",
        )
        .bind(run.id)
        .bind(index)
        .execute(&mut *conn)
        .await
        .expect("shift created_at");
        ids.push(run.id);
    }

    let listed = run_sql::list(pool, seeded.autopilot_id, 10, 0)
        .await
        .expect("list");
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].id, ids[0], "offset 0 秒 ⇒ 最新，排最前");
    assert_eq!(listed[2].id, ids[2]);
    let paged = run_sql::list(pool, seeded.autopilot_id, 1, 1)
        .await
        .expect("list paged");
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].id, ids[1]);

    // 别的 workspace 读不到（`get_in_workspace` 的 `AND workspace_id`）。
    let other: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-run-other', $1) RETURNING id",
    )
    .bind(format!("itest-run-other-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("other workspace");
    assert!(matches!(
        run_sql::get_in_workspace(pool, ids[0], other).await,
        Err(RepoError::NotFound)
    ));
    assert!(matches!(
        run_sql::get(pool, Uuid::new_v4()).await,
        Err(RepoError::NotFound)
    ));
    assert_eq!(
        run_sql::get_autopilot(pool, seeded.autopilot_id)
            .await
            .expect("get_autopilot")
            .id,
        seeded.autopilot_id
    );

    // `update_autopilot_last_run_at` 是跳过与派发两条线共用的收尾写。
    assert!(
        sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT last_run_at FROM autopilot WHERE id = $1"
        )
        .bind(seeded.autopilot_id)
        .fetch_one(pool)
        .await
        .expect("last_run_at before")
        .is_none(),
        "还没跑过 ⇒ NULL"
    );
    run_sql::update_autopilot_last_run_at(pool, seeded.autopilot_id)
        .await
        .expect("touch last_run_at");
    assert!(sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT last_run_at FROM autopilot WHERE id = $1"
    )
    .bind(seeded.autopilot_id)
    .fetch_one(pool)
    .await
    .expect("last_run_at after")
    .is_some());

    drop(conn);
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(other)
        .execute(pool)
        .await;
    cleanup(&fixture, &seeded).await;
}
