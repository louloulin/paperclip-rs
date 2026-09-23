//! `crate::autopilot::run` / `crate::autopilot::delivery` 的真库语义（M5-4 / LUM-1569）。
//!
//! HTTP 面的 e2e 在 `crates/mc-http/tests/autopilots/{execution,deliveries}.rs`，派发编排的三块
//! 在 `dispatch.rs`。这里只测**只有真 PostgreSQL 能回答**的那一半 —— 内存版看着也对：
//!
//! - 幂等唯一索引的**槽位**语义：`(trigger_id, planned_at)` / `webhook_delivery_id` /
//!   `quota_reservation_id` 各占一条；
//! - `recover_partial_run` 只吃「run 写了但下游没建出来」的半成品，且**清空 `planned_at`**
//!   把幂等槽位腾回来、顺带释放预留（`reserved_count` 减 1）；
//! - `update_terminal_with_quota` 的两种结算（`consume=true` 消费 vs `false` 退回）在**同一条
//!   语句**里完成，没有中间态；
//! - `fail_by_issue` 只看见在飞 run、写上游那条 `linked issue was deleted` 原因、并退掉仍
//!   `reserved` 的预留（已消费的不退）；
//! - `create_task` 的归属栅栏 `lock_task_owner_rows`：owner 行缺失时写零行（返回 `None`），
//!   而不是撞 FK 报错 —— 这是「工作区正在拆除」唯一可判别的信号；
//! - 读面的 workspace 收窄（`get_in_workspace` / `delivery::list` 的 `JOIN autopilot`）与
//!   `ORDER BY created_at DESC`；
//! - replay 的 `(replayed_from_delivery_id, replay_idempotency_key)` 部分唯一索引（两列任一
//!   为 NULL 都不参与冲突）与 `dedupe_key` 恒 NULL（绕过 provider 去重）。

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{setup, teardown, Fixture};
use crate::autopilot::delivery::{self as delivery_sql, NewReplayDelivery, WebhookDeliverySlimRow};
use crate::autopilot::quota::{
    ensure_period, get_period, get_reservation_by_key, increment_reserved, reserve,
};
use crate::autopilot::run::{self as run_sql, NewAutopilotRun, NewAutopilotTask};
use crate::RepoError;

/// 本文件自己的种子：一个工作区里的 member + 绑了 runtime 的 agent + 一条 autopilot +
/// 一个 issue + 一个 schedule trigger。
///
/// 不放进公共脚手架（`tests/mod.rs` 只铺 workspace）：run/delivery 两表要的邻表比配额面宽，
/// `write.rs` 已用同样理由自带了一份，两边都懒得把邻表种子推成共享 API。
#[allow(clippy::struct_field_names)] // 六个字段都就是各自表的主键，去掉 `_id` 反而与读表字段错位
struct Seeded {
    user_id: Uuid,
    agent_id: Uuid,
    runtime_id: Uuid,
    autopilot_id: Uuid,
    issue_id: Uuid,
    trigger_id: Uuid,
}

async fn seed(pool: &sqlx::PgPool, workspace_id: Uuid) -> Seeded {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-run', $1) RETURNING id"#,
    )
    .bind(format!("run-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(workspace_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert member");
    let runtime_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, daemon_id, name, runtime_mode, provider, status) \
         VALUES ($1, $2, $3, 'local', 'claude', 'online') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime");
    let agent_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, \
             permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, 'private') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-run-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert agent");
    let autopilot_id: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot (workspace_id, title, description, assignee_type, assignee_id, \
             status, execution_mode, created_by_type, created_by_id) \
         VALUES ($1, $2, 'itest body', 'agent', $3, 'active', 'run_only', 'member', $4) \
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-run-ap-{}", Uuid::new_v4()))
    .bind(agent_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert autopilot");
    let issue_id: Uuid = sqlx::query_scalar(
        "INSERT INTO issue (workspace_id, number, identifier, title, status, creator_type, \
             creator_id, origin_type, origin_id) \
         VALUES ($1, 9001, $2, 'itest issue', 'todo', 'agent', $3, 'autopilot', $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("ITEST-RUN-{}", Uuid::new_v4()))
    .bind(agent_id)
    .bind(autopilot_id)
    .fetch_one(pool)
    .await
    .expect("insert issue");
    let trigger_id: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot_trigger (autopilot_id, kind, cron_expression, timezone, enabled, \
             created_by_type, created_by_id) \
         VALUES ($1, 'schedule', '0 * * * *', 'UTC', true, 'member', $2) RETURNING id",
    )
    .bind(autopilot_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert trigger");
    Seeded {
        user_id,
        agent_id,
        runtime_id,
        autopilot_id,
        issue_id,
        trigger_id,
    }
}

/// 清场：先删引用方（任务 / 投递 / run），再删被引用方，最后交给公共 `teardown` 收 workspace。
///
/// `agent_task_queue` 用 agent / issue / run 三个键删，是因为不同用例挂的键不同。
async fn cleanup(fixture: &Fixture, seeded: &Seeded) {
    let pool = fixture.db.pool();
    for (sql, key) in [
        (
            "DELETE FROM agent_task_queue WHERE agent_id = $1",
            seeded.agent_id,
        ),
        (
            "DELETE FROM agent_task_queue WHERE issue_id = $1",
            seeded.issue_id,
        ),
        (
            "DELETE FROM agent_task_queue WHERE autopilot_run_id IN (SELECT id FROM autopilot_run \
                 WHERE autopilot_id = $1)",
            seeded.autopilot_id,
        ),
        (
            "DELETE FROM agent_task_queue WHERE id IN (SELECT task_id FROM autopilot_run \
                 WHERE autopilot_id = $1 AND task_id IS NOT NULL)",
            seeded.autopilot_id,
        ),
    ] {
        let _ = sqlx::query(sql).bind(key).execute(pool).await;
    }
    let _ = sqlx::query("DELETE FROM webhook_delivery WHERE autopilot_id = $1")
        .bind(seeded.autopilot_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM autopilot_run WHERE autopilot_id = $1")
        .bind(seeded.autopilot_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM autopilot_quota_reservation WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM autopilot_quota_period WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issue WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM autopilot_trigger WHERE autopilot_id = $1")
        .bind(seeded.autopilot_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM autopilot WHERE id = $1")
        .bind(seeded.autopilot_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agent_runtime WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agent WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM member WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(seeded.user_id)
        .execute(pool)
        .await;
    teardown(fixture).await;
}

/// 造一条 run；`planned_at` 默认 `None`（不占幂等槽位）。
fn new_run(seeded: &Seeded, status: &str) -> NewAutopilotRun {
    NewAutopilotRun {
        id: Uuid::new_v4(),
        autopilot_id: seeded.autopilot_id,
        trigger_id: None,
        source: "manual".to_string(),
        status: status.to_string(),
        trigger_payload: None,
        squad_id: None,
        planned_at: None,
        webhook_delivery_id: None,
        quota_reservation_id: None,
        reason_code: None,
    }
}

/// 造一条挂 run 的任务入参（`issue_id` 与 `agent_id` 由调用方给）。
fn new_task(
    seeded: &Seeded,
    agent_id: Uuid,
    issue_id: Option<Uuid>,
    runtime_id: Option<Uuid>,
) -> NewAutopilotTask {
    NewAutopilotTask {
        id: Uuid::new_v4(),
        agent_id,
        runtime_id,
        issue_id,
        priority: 0,
        autopilot_run_id: None,
        trigger_summary: Some("itest".to_string()),
        originator_user_id: Some(seeded.user_id),
        accountable_user_id: Some(seeded.user_id),
        rule_version_id: None,
        originator_source: Some("direct_human".to_string()),
        trigger_evidence_kind: None,
        trigger_evidence_ref_id: None,
    }
}

/// 一条周期行 + 一条 `reserved` 预留，并把 `reserved_count` 记账到位（`reserve` 自己不记账）。
async fn seed_reservation(
    pool: &sqlx::PgPool,
    ws: Uuid,
    key: &str,
    bounds: (DateTime<Utc>, DateTime<Utc>),
) -> Uuid {
    let (start, end) = bounds;
    ensure_period(pool, ws, start, end).await.expect("period");
    let reservation = reserve(pool, ws, start, end, 1, 1, "manual", key)
        .await
        .expect("reserve");
    increment_reserved(pool, ws, start, end)
        .await
        .expect("increment_reserved");
    reservation.id
}

/// `(trigger_id, planned_at)` 上的唯一索引：同槽第二次创建必须 `Conflict`，幂等查询取回第一行。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn create_run_rejects_a_second_run_in_the_same_idempotency_slot() {
    let Some(fixture) = setup().await else {
        println!("skip create_run_rejects_a_second_run_in_the_same_idempotency_slot: no env");
        return;
    };
    let pool = fixture.db.pool();
    let seeded = seed(pool, fixture.workspace_id).await;
    let planned_at = Utc::now();
    let mut conn = pool.acquire().await.expect("conn");

    let mut first = new_run(&seeded, "issue_created");
    first.trigger_id = Some(seeded.trigger_id);
    first.planned_at = Some(planned_at);
    let first = run_sql::create_run(&mut conn, &first)
        .await
        .expect("first run");
    assert_eq!(first.status, "issue_created");
    assert!(first.completed_at.is_none());

    // 同槽第二行 ⇒ 唯一索引冲突（`map_sqlx_err` 把 23505 折成 `Conflict`）。
    let mut second = new_run(&seeded, "issue_created");
    second.trigger_id = Some(seeded.trigger_id);
    second.planned_at = Some(planned_at);
    assert!(matches!(
        run_sql::create_run(&mut conn, &second).await,
        Err(RepoError::Conflict)
    ));
    // 换个 `planned_at` 就是另一个槽位。
    let mut third = new_run(&seeded, "issue_created");
    third.trigger_id = Some(seeded.trigger_id);
    third.planned_at = Some(planned_at + Duration::minutes(1));
    assert!(run_sql::create_run(&mut conn, &third).await.is_ok());

    let found = run_sql::find_by_trigger_and_planned(&mut conn, seeded.trigger_id, planned_at)
        .await
        .expect("lookup")
        .expect("idempotency hit");
    assert_eq!(found.id, first.id);

    // webhook 线与 quota 线各自也有唯一槽位（`autopilot_run` **没有**这两列的外键，
    // 卡住重复的是部分唯一索引）⇒ 同值第二行必须 `Conflict`。
    let delivery_id = Uuid::new_v4();
    let mut webhook_run = new_run(&seeded, "running");
    webhook_run.webhook_delivery_id = Some(delivery_id);
    let webhook_run = run_sql::create_run(&mut conn, &webhook_run)
        .await
        .expect("first webhook run");
    let mut webhook_run2 = new_run(&seeded, "running");
    webhook_run2.webhook_delivery_id = Some(delivery_id);
    assert!(matches!(
        run_sql::create_run(&mut conn, &webhook_run2).await,
        Err(RepoError::Conflict)
    ));
    assert_eq!(
        run_sql::find_by_webhook_delivery(&mut conn, delivery_id)
            .await
            .expect("lookup")
            .expect("webhook idempotency hit")
            .id,
        webhook_run.id
    );

    let reservation_id = Uuid::new_v4();
    let mut quota_run = new_run(&seeded, "running");
    quota_run.quota_reservation_id = Some(reservation_id);
    let quota_run = run_sql::create_run(&mut conn, &quota_run)
        .await
        .expect("first quota run");
    let mut quota_run2 = new_run(&seeded, "running");
    quota_run2.quota_reservation_id = Some(reservation_id);
    assert!(matches!(
        run_sql::create_run(&mut conn, &quota_run2).await,
        Err(RepoError::Conflict)
    ));
    assert_eq!(
        run_sql::find_by_quota_reservation(&mut conn, reservation_id)
            .await
            .expect("lookup")
            .expect("quota idempotency hit")
            .id,
        quota_run.id
    );

    drop(conn);
    cleanup(&fixture, &seeded).await;
}

/// 半成品回收：腾回幂等槽位、释放预留（`reserved_count` 减 1）、写上游那条原因；
/// 对「下游已经建出来」的 run（有 issue / 有任务）一律不碰。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn recover_partial_run_frees_the_slot_and_releases_the_reservation() {
    let Some(fixture) = setup().await else {
        println!("skip recover_partial_run_frees_the_slot_and_releases_the_reservation: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let seeded = seed(pool, ws).await;
    let start = Utc::now() - Duration::hours(1);
    let end = start + Duration::days(30);
    let bounds = (start, end);
    let planned_at = Utc::now();
    let mut conn = pool.acquire().await.expect("conn");

    let reservation_id = seed_reservation(pool, ws, "idem-run-1", bounds).await;
    let mut partial = new_run(&seeded, "issue_created");
    partial.trigger_id = Some(seeded.trigger_id);
    partial.planned_at = Some(planned_at);
    partial.quota_reservation_id = Some(reservation_id);
    let partial = run_sql::create_run(&mut conn, &partial)
        .await
        .expect("partial run");

    assert!(
        run_sql::recover_partial_run(&mut conn, partial.id)
            .await
            .expect("recover"),
        "半成品应当被回收"
    );
    let recovered = run_sql::get(pool, partial.id).await.expect("get");
    assert_eq!(recovered.status, "failed");
    assert_eq!(recovered.reason_code.as_deref(), Some("internal_error"));
    assert_eq!(
        recovered.failure_reason.as_deref(),
        Some("recovered partial dispatch (crashed before downstream creation)")
    );
    assert!(recovered.planned_at.is_none(), "槽位必须腾回来");
    assert!(recovered.completed_at.is_some());

    // 预留转 released（`released` 会把幂等键腾空）⇒ 幂等键查询变 `None`，
    // 而周期 `reserved_count` 回到 0、`used_count` 仍是 0（没消费）。
    assert!(
        get_reservation_by_key(pool, ws, start, end, "idem-run-1")
            .await
            .expect("reservation lookup")
            .is_none(),
        "released 会腾空幂等键（部分唯一索引 WHERE state <> 'released'）"
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM autopilot_quota_reservation WHERE id = $1")
            .bind(reservation_id)
            .fetch_one(pool)
            .await
            .expect("reservation row");
    assert_eq!(state, "released");
    let period = get_period(pool, ws, start, end)
        .await
        .expect("period")
        .expect("row");
    assert_eq!(period.reserved_count, 0);
    assert_eq!(period.used_count, 0);

    // 槽位回来后，同一个 `(trigger_id, planned_at)` 可以再建。
    let mut again = new_run(&seeded, "issue_created");
    again.trigger_id = Some(seeded.trigger_id);
    again.planned_at = Some(planned_at);
    assert!(run_sql::create_run(&mut conn, &again).await.is_ok());

    // 已经挂上 issue 的 run 不是半成品。
    let linked = run_sql::create_run(&mut conn, &new_run(&seeded, "issue_created"))
        .await
        .expect("linked run");
    run_sql::update_issue_created(&mut conn, linked.id, seeded.issue_id)
        .await
        .expect("update_issue_created");
    assert!(!run_sql::recover_partial_run(&mut conn, linked.id)
        .await
        .expect("recover"));

    // 已经入队任务的 `running` run 同理（`agent_task_queue` 里挂着它）。
    let with_task = run_sql::create_run(&mut conn, &new_run(&seeded, "running"))
        .await
        .expect("running run");
    let task_id = run_sql::create_task(
        &mut conn,
        &NewAutopilotTask {
            autopilot_run_id: Some(with_task.id),
            ..new_task(&seeded, seeded.agent_id, None, Some(seeded.runtime_id))
        },
    )
    .await
    .expect("fence allows")
    .expect("task id");
    run_sql::update_running(&mut conn, with_task.id, task_id)
        .await
        .expect("update_running");
    assert!(!run_sql::recover_partial_run(&mut conn, with_task.id)
        .await
        .expect("recover"));

    drop(conn);
    cleanup(&fixture, &seeded).await;
}

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

/// 投递面：瘦行读**不含**详情三列、`(原投递, 幂等键)` 的部分唯一索引、replay 行 `dedupe_key` 恒 NULL、
/// `signature_failed()` 只看 `rejected` / `invalid`、列表 workspace 收窄 + newest first。
#[allow(clippy::too_many_lines)] // 121 行：原投递 + 两条 replay + 三条查询（列表/详情/幂等键）
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn replay_rows_are_idempotent_per_key_and_bypass_provider_dedupe() {
    let Some(fixture) = setup().await else {
        println!("skip replay_rows_are_idempotent_per_key_and_bypass_provider_dedupe: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let seeded = seed(pool, ws).await;

    // 原投递：普通 webhook 投递（有 dedupe_key），直接 SQL 铺（`create_replay` 是 replay 专用）。
    let original_id: Uuid = sqlx::query_scalar(
        "INSERT INTO webhook_delivery (workspace_id, autopilot_id, trigger_id, provider, event, \
             dedupe_key, dedupe_source, signature_status, status, selected_headers, content_type, \
             raw_body) \
         VALUES ($1, $2, $3, 'github', 'issues.opened', 'gh:1', 'header', 'valid', 'queued', \
             '{\"x-github-event\": \"issues\"}', 'application/json', $4) RETURNING id",
    )
    .bind(ws)
    .bind(seeded.autopilot_id)
    .bind(seeded.trigger_id)
    .bind(br#"{"action":"opened"}"#.to_vec())
    .fetch_one(pool)
    .await
    .expect("insert original delivery");

    // replay：绕过去重（`dedupe_key` NULL）、`signature_status='not_required'`、初始 `queued`。
    let mut replay = NewReplayDelivery {
        id: Uuid::new_v4(),
        workspace_id: ws,
        autopilot_id: seeded.autopilot_id,
        trigger_id: seeded.trigger_id,
        provider: "generic".to_string(),
        event: "issues.opened".to_string(),
        selected_headers: json!({}),
        content_type: None,
        raw_body: br#"{"action":"opened"}"#.to_vec(),
        replayed_from_delivery_id: original_id,
        replay_idempotency_key: "replay-key".to_string(),
    };
    let created = delivery_sql::create_replay(pool, &replay)
        .await
        .expect("first replay");
    assert_eq!(created.status, "queued");
    assert_eq!(created.signature_status, "not_required");
    assert!(created.dedupe_key.is_none(), "replay 绕过 provider 去重");
    assert_eq!(created.dedupe_source, None);
    assert_eq!(
        created.replay_idempotency_key.as_deref(),
        Some("replay-key")
    );
    assert_eq!(created.replayed_from_delivery_id, Some(original_id));
    assert!(!created.signature_failed());

    // 同键再插 ⇒ 唯一索引冲突；`find_replay` 取回已有那行（handler 的 202 幂等）。
    replay.id = Uuid::new_v4();
    assert!(matches!(
        delivery_sql::create_replay(pool, &replay).await,
        Err(RepoError::Conflict)
    ));
    let found = delivery_sql::find_replay(pool, original_id, "replay-key")
        .await
        .expect("find_replay")
        .expect("idempotency hit");
    assert_eq!(found.id, created.id);
    assert!(delivery_sql::find_replay(pool, original_id, "another-key")
        .await
        .expect("find_replay")
        .is_none());

    // 瘦行列表：结构里没有详情三列（列被裁剪）+ newest first。
    let slim: Vec<WebhookDeliverySlimRow> =
        delivery_sql::list(pool, seeded.autopilot_id, ws, 10, 0)
            .await
            .expect("list");
    assert_eq!(slim.len(), 2);
    assert_eq!(slim[0].id, created.id, "newest first");
    assert_eq!(slim[1].id, original_id);
    let paged = delivery_sql::list(pool, seeded.autopilot_id, ws, 1, 1)
        .await
        .expect("list paged");
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].id, original_id);

    // 详情读有那三列；workspace 限定，跨工作区一律 NotFound。
    let detail = delivery_sql::get_in_workspace(pool, created.id, ws)
        .await
        .expect("detail");
    assert_eq!(detail.selected_headers, json!({}));
    assert_eq!(
        detail.raw_body.as_deref(),
        Some(&br#"{"action":"opened"}"#[..])
    );
    let other: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-delivery-other', $1) RETURNING id",
    )
    .bind(format!("itest-delivery-other-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("other workspace");
    assert!(matches!(
        delivery_sql::get_in_workspace(pool, created.id, other).await,
        Err(RepoError::NotFound)
    ));
    assert!(delivery_sql::list(pool, seeded.autopilot_id, other, 10, 0)
        .await
        .expect("cross-workspace list")
        .is_empty());

    // 签名没过（`rejected` / `signature_status='invalid'`）⇒ `signature_failed()`，replay 必须被拒。
    sqlx::query("UPDATE webhook_delivery SET status = 'rejected' WHERE id = $1")
        .bind(original_id)
        .execute(pool)
        .await
        .expect("reject original");
    assert!(delivery_sql::get_in_workspace(pool, original_id, ws)
        .await
        .expect("original")
        .signature_failed());
    sqlx::query(
        "UPDATE webhook_delivery SET status = 'queued', signature_status = 'invalid' WHERE id = $1",
    )
    .bind(original_id)
    .execute(pool)
    .await
    .expect("invalidate original");
    assert!(delivery_sql::get_in_workspace(pool, original_id, ws)
        .await
        .expect("original")
        .signature_failed());

    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(other)
        .execute(pool)
        .await;
    cleanup(&fixture, &seeded).await;
}
