//! `crate::autopilot::run` 的真库语义（M5-4 / LUM-1569）：幂等槽位 + 半成品回收。
//!
//! HTTP 面的 e2e 在 `crates/mc-http/tests/autopilots/{execution,deliveries}.rs`，派发编排的三块
//! 在 `dispatch.rs`。这里只测**只有真 PostgreSQL 能回答**的那一半 —— 内存版看着也对：
//!
//! - 幂等唯一索引的**槽位**语义：`(trigger_id, planned_at)` / `webhook_delivery_id` /
//!   `quota_reservation_id` 各占一条；
//! - `recover_partial_run` 只吃「run 写了但下游没建出来」的半成品，且**清空 `planned_at`**
//!   把幂等槽位腾回来、顺带释放预留（`reserved_count` 减 1）。
//!
//! 其余 run/delivery 语义在兄弟文件里（门 ⑩ 单文件 800 行上限的拆法）：
//! `run_settle.rs`（终态结算 / issue 链 / 任务栅栏 / 读面收窄）与
//! `run_delivery.rs`（投递瘦行 + replay 幂等键）。本文件的夹具以 `pub(super)` 暴露给那两个文件。
//!
//! 夹具：一个工作区 + member + 绑了 runtime 的 agent + autopilot + issue + schedule trigger；
//! `write.rs` 用同样理由自带了一份，两边都不把邻表种子推成共享 API。

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use super::{setup, teardown, Fixture};
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
pub(super) struct Seeded {
    pub(super) user_id: Uuid,
    pub(super) agent_id: Uuid,
    pub(super) runtime_id: Uuid,
    pub(super) autopilot_id: Uuid,
    pub(super) issue_id: Uuid,
    pub(super) trigger_id: Uuid,
}

pub(super) async fn seed(pool: &sqlx::PgPool, workspace_id: Uuid) -> Seeded {
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
pub(super) async fn cleanup(fixture: &Fixture, seeded: &Seeded) {
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
pub(super) fn new_run(seeded: &Seeded, status: &str) -> NewAutopilotRun {
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
pub(super) fn new_task(
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
pub(super) async fn seed_reservation(
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
