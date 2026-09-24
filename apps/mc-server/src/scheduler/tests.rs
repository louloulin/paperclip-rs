//! M5-9 的**真库**用例：两个生产端口 + 「装配真的把两个 job 挂上循环」。
//!
//! 跑法（全部 `#[ignore]`；门禁 ⑥ 会带上 `-p mc-server`）：
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://… \
//!   cargo test -p mc-server -- --ignored --test-threads=1
//! ```
//!
//! 前置：`MULTICA_DATABASE_URL=… cargo run -p mc-migrate -- run --dir migrations`。
//!
//! # 为什么种子是裸 SQL
//!
//! `apps/mc-server` 是**纯二进制**（没有 lib target、没有 `[dev-dependencies]`），所以
//! `mc-http/tests/**/support.rs` 里那些 `pub(crate)` 夹具**导不进来**；`mc-db` 的
//! `from_pool` 又要 `test-util` feature（端口不想为测试多一条 feature 边）。
//! ⇒ 这里自带最小夹具：`workspace` / `user` / `member` / `agent_runtime` / `agent` / `issue`
//! 六条 INSERT，字段与 `mc-http/tests/issues/support.rs` 的种子逐字同源。
//!
//! # 隔离与清理
//!
//! * 每个用例一个**新 workspace**，结束时删 workspace（级联清掉 agent/runtime/issue/task）
//!   再删 user；断言只对自己造的行做（库里可能同时躺着别的用例的数据）。⚠️ 唤醒两张表没有 FK
//!   （见 [`cleanup`]），必须显式删。
//! * 只有 `sys_cron_executions` 是**无 workspace 键的全局审计表**（唯一键
//!   `job_name, scope_kind, scope_id, plan_time`），而两个 job 名是**常量**：本文件在起循环前
//!   先删掉这两个 job 的旧行，否则「同一 30s 桶已被上一次跑占掉」会让本轮认领不到租约而假红。
//!   这也意味着它**不能并行跑**（`--test-threads=1`）。
//! * 进程级证据（真的起 `multica-server` 看租约行）不在本文件：那是 PR 里附的命令输出。

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, SubsecRound as _, Utc};
use serde_json::json;
use sqlx::postgres::PgPool;
use sqlx::Row as _;
use uuid::Uuid;

use mc_repos::wakeup::issue::NewWakeup;
use mc_repos::wakeup::WakeupRow;
use mc_scheduler::jobs::autopilot::AutopilotSchedulePort as _;
use mc_scheduler::jobs::issue_wakeup::{WakeupDispatchPort as _, WakeupOutcome};
use mc_scheduler::{Options, SchedulerHandle};
use mc_ws::hub::Hub;

use super::schedule_port::McAutopilotSchedulePort;
use super::wakeup_port::McWakeupDispatchPort;

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 测试库（没有 URL 就显式失败，不静默跳过 —— 仓库统一口径）。
async fn pool() -> PgPool {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    PgPool::connect(&url).await.expect("connect test db")
}

/// 一个用例的全部实体。
struct World {
    ws: Uuid,
    user: Uuid,
    runtime: Uuid,
    agent: Uuid,
    issue: Uuid,
}

/// `workspace` + `user` + `member` + `agent_runtime` + `agent`(绑定 runtime) + `issue`。
///
/// `agent.kind='user'` + `owner_id=user` + `permission_mode='public_to'`：
/// 这三条一起才让 `wakeup::authorize` 放行（`service.rs:322` 的 owner 短路），
/// 与 `mc-http/tests/issues/wakeups/support.rs` 的种子一致。
async fn seed_world(pool: &PgPool) -> World {
    let ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m5-9-ws', $1) RETURNING id",
    )
    .bind(format!("itest-m5-9-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m5-9-user', $1) RETURNING id"#,
    )
    .bind(format!("m5-9-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(ws)
        .bind(user)
        .execute(pool)
        .await
        .expect("insert member");

    let runtime: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, status, last_seen_at) \
         VALUES ($1, $2, $3, 'local', 'claude', 'online', now()) RETURNING id",
    )
    .bind(ws)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime");

    let agent: Uuid = sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, 'public_to') RETURNING id",
    )
    .bind(ws)
    .bind(format!("itest-m5-9-agent-{}", Uuid::new_v4()))
    .bind(runtime)
    .bind(user)
    .fetch_one(pool)
    .await
    .expect("insert agent");

    let issue: Uuid = sqlx::query_scalar(
        "INSERT INTO issue(workspace_id, title, status, priority, creator_type, creator_id) \
         VALUES ($1, 'itest-m5-9-issue', 'todo', 'high', 'member', $2) RETURNING id",
    )
    .bind(ws)
    .bind(user)
    .fetch_one(pool)
    .await
    .expect("insert issue");

    World {
        ws,
        user,
        runtime,
        agent,
        issue,
    }
}

/// 删 workspace（级联）+ user。
///
/// ⚠️ `issue_wakeup` / `issue_wakeup_receipt` **没有任何 FK**（迁移 509 逐字如此）⇒ 删 workspace
/// 不会带走它们，留下的是「workspace/issue 都没了的孤儿 wakeup」；而 `ready_wakeups` 仍会把它
/// 当候选，下一轮 `dispatch_wakeup` 就只能报 `wakeup not found`，把每个 wakeup tick 刷成
/// FAILED。上游 `DeleteWorkspace` 正是靠这两条 DELETE 扫尾（`workspace_delete.sql:537/539`），
/// 这里照抄。
async fn cleanup(pool: &PgPool, world: &World) {
    let _ = sqlx::query(
        "DELETE FROM issue_wakeup_receipt WHERE wakeup_id IN \
         (SELECT id FROM issue_wakeup WHERE workspace_id = $1)",
    )
    .bind(world.ws)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM issue_wakeup WHERE workspace_id = $1")
        .bind(world.ws)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(world.ws)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(world.user)
        .execute(pool)
        .await;
}

/// 建一条 autopilot（`status` 取 `active` / `paused`）。
async fn seed_autopilot(pool: &PgPool, world: &World, status: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot(workspace_id, title, assignee_id, status, created_by_type, created_by_id) \
         VALUES ($1, $2, $3, $4, 'member', $5) RETURNING id",
    )
    .bind(world.ws)
    .bind(format!("itest-m5-9-{}", Uuid::new_v4()))
    .bind(world.agent)
    .bind(status)
    .bind(world.user)
    .fetch_one(pool)
    .await
    .expect("insert autopilot")
}

/// 建一条 trigger，返回 id。
async fn seed_trigger(
    pool: &PgPool,
    autopilot: Uuid,
    kind: &str,
    enabled: bool,
    cron: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot_trigger(autopilot_id, kind, enabled, cron_expression, timezone) \
         VALUES ($1, $2, $3, $4, 'UTC') RETURNING id",
    )
    .bind(autopilot)
    .bind(kind)
    .bind(enabled)
    .bind(cron)
    .fetch_one(pool)
    .await
    .expect("insert autopilot_trigger")
}

/// 建一条**已经过了至少一个调度格**的 `* * * * *` trigger（`created_at = now - 10min`）。
///
/// ⚠️ 不能直接用 [`seed_trigger`]：`plans_for_scope` 在「从未触发过」时把枚举锚在
/// `created_at` 上，而 [`mc_autopilot::cron::next_after_utc`] 的口径是
/// `floor(锚点) + 1min`（严格晚于锚点）⇒ **刚出生的 trigger 在当前这一格里没有发生点**，
/// 它的第一格要等到下一个整分。用 `created_at` 造出「十分钟前就出生」的 trigger，
/// 本 tick 就一定能算出一格（迟到闸 5min ⇒ 最近一次整分必然合格）。
async fn seed_due_schedule_trigger(pool: &PgPool, autopilot: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot_trigger \
         (autopilot_id, kind, enabled, cron_expression, timezone, created_at) \
         VALUES ($1, 'schedule', true, '* * * * *', 'UTC', now() - interval '10 minutes') \
         RETURNING id",
    )
    .bind(autopilot)
    .fetch_one(pool)
    .await
    .expect("insert due autopilot_trigger")
}

/// 建一条到点的 `every` 唤醒（`next_fire_at = now - 5s`）⇒ `ready_wakeups` 立刻能取到。
async fn seed_due_wakeup(pool: &PgPool, world: &World, instruction: &str) -> WakeupRow {
    let new = NewWakeup {
        workspace_id: world.ws,
        issue_id: world.issue,
        agent_id: world.agent,
        created_by: world.user,
        source_task_id: None,
        parent_comment_id: None,
        instruction: instruction.to_owned(),
        kind: "every".to_owned(),
        mode: "continuous".to_owned(),
        event_types: Vec::new(),
        filter_agent_id: None,
        filter_task_id: None,
        filter_actor_type: None,
        filter_actor_id: None,
        interval_seconds: Some(60),
        cron_expression: None,
        timezone: "UTC".to_owned(),
        next_fire_at: Some(Utc::now() - Duration::seconds(5)),
    };
    let mut conn = pool.acquire().await.expect("acquire conn");
    mc_repos::wakeup::issue::create(&mut conn, &new)
        .await
        .expect("create issue_wakeup")
}

/// 读最新一版 wakeup 行。
async fn reload(pool: &PgPool, id: Uuid) -> WakeupRow {
    mc_repos::wakeup::issue::lockless(pool, id)
        .await
        .expect("read wakeup")
        .expect("wakeup row exists")
}

/// 该 wakeup 在 `agent_task_queue` 里的行（0 或 1 条）。
async fn wakeup_task(pool: &PgPool, wakeup_id: Uuid) -> Option<sqlx::postgres::PgRow> {
    sqlx::query(
        "SELECT id, status, priority, runtime_id, issue_id, agent_id, trigger_summary, handoff_note, \
                context, originator_user_id, accountable_user_id, originator_source, \
                trigger_evidence_kind, trigger_evidence_ref_id, runtime_mcp_overlay, \
                runtime_connected_apps \
           FROM agent_task_queue WHERE context->>'wakeup_id' = $1",
    )
    .bind(wakeup_id.to_string())
    .fetch_optional(pool)
    .await
    .expect("select wakeup task")
}

/// 轮询到条件成立；超时就把 `what` 写进 panic。
async fn wait_until<F, Fut>(what: &str, timeout: StdDuration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check().await {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: 超时");
        tokio::time::sleep(StdDuration::from_millis(250)).await;
    }
}

// ---------------------------------------------------------------------------
// 1. AutopilotSchedulePort
// ---------------------------------------------------------------------------

/// 5 个方法各跑一遍真库：列表筛选（`kind='schedule'` + `enabled` + `cron <> ''` +
/// 所属 autopilot `status='active'`）、两条读、两条展示列写入。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn schedule_port_filters_and_advances() {
    let pool = pool().await;
    let world = seed_world(&pool).await;

    let active = seed_autopilot(&pool, &world, "active").await;
    let paused = seed_autopilot(&pool, &world, "paused").await;
    let ok = seed_trigger(&pool, active, "schedule", true, Some("0 0 * * *")).await;
    let disabled = seed_trigger(&pool, active, "schedule", false, Some("0 0 * * *")).await;
    let empty_cron = seed_trigger(&pool, active, "schedule", true, Some("")).await;
    let webhook = seed_trigger(&pool, active, "webhook", true, None).await;
    let paused_owner = seed_trigger(&pool, paused, "schedule", true, Some("0 0 * * *")).await;

    let port = McAutopilotSchedulePort::from_pool(pool.clone());

    let rows = port.list_schedulable_triggers().await.expect("list");
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    assert!(ids.contains(&ok), "可调度的 trigger 必须出现: {ids:?}");
    for (label, id) in [
        ("disabled", disabled),
        ("empty cron", empty_cron),
        ("webhook", webhook),
        ("paused autopilot", paused_owner),
    ] {
        assert!(!ids.contains(&id), "{label} 的 trigger 不该出现: {ids:?}");
    }
    let mine = rows.iter().find(|row| row.id == ok).expect("row for ok");
    assert_eq!(mine.autopilot_id, active);
    assert_eq!(mine.cron_expression.as_deref(), Some("0 0 * * *"));
    assert_eq!(mine.timezone.as_deref(), Some("UTC"));

    let loaded = port.load_trigger(ok).await.expect("load_trigger");
    assert_eq!(loaded.map(|row| row.id), Some(ok));
    assert!(port
        .load_trigger(Uuid::new_v4())
        .await
        .expect("load_trigger miss")
        .is_none());
    let ap = port.load_autopilot(active).await.expect("load_autopilot");
    assert_eq!(ap.map(|row| row.id), Some(active));
    assert!(port
        .load_autopilot(Uuid::new_v4())
        .await
        .expect("load_autopilot miss")
        .is_none());

    // `advance_next_run`：三个列一起推（`next_run_at` + `last_fired_at` + `updated_at`）。
    let before: (Option<chrono::DateTime<Utc>>, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT last_fired_at, updated_at FROM autopilot_trigger WHERE id = $1")
            .bind(ok)
            .fetch_one(&pool)
            .await
            .expect("read trigger before");
    // PG 的 `timestamptz` 只到微秒 ⇒ 绑之前先降到微秒，否则读回来会比入参少几百纳秒。
    let stamped = (Utc::now() + Duration::hours(1)).trunc_subsecs(3);
    assert_eq!(
        port.advance_next_run(ok, Some(stamped))
            .await
            .expect("advance"),
        1
    );
    let after: (
        Option<chrono::DateTime<Utc>>,
        Option<chrono::DateTime<Utc>>,
        chrono::DateTime<Utc>,
    ) = sqlx::query_as(
        "SELECT next_run_at, last_fired_at, updated_at FROM autopilot_trigger WHERE id = $1",
    )
    .bind(ok)
    .fetch_one(&pool)
    .await
    .expect("read trigger after");
    assert_eq!(after.0, Some(stamped));
    assert!(after.1.is_some(), "last_fired_at 必须被推");
    assert!(after.2 >= before.1, "updated_at 必须单调");

    assert_eq!(port.touch_fired_at(ok).await.expect("touch"), 1);
    assert_eq!(
        port.advance_next_run(Uuid::new_v4(), None)
            .await
            .expect("advance miss"),
        0
    );

    cleanup(&pool, &world).await;
}

// ---------------------------------------------------------------------------
// 2. WakeupDispatchPort
// ---------------------------------------------------------------------------

/// 派发 → 合并证据 → 幂等收尾 → 失败记账 → 触摸，全部打真库。
///
/// `dispatch_wakeup` 一次覆盖 7 步里的 3/4/5/6 步（`plan_dispatch` / 建队列行 /
/// `consume_dispatch` / `COMMIT`），第 7 步的广播在空 hub 上是 `miss()`（无订阅者），
/// 所以不断言它 —— 这里断言的是**队列行的每一列**与收据的消费结果。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 7 步事务 + 两轮派发 + 收尾，拆开就丢了「同一 wakeup 的行」这条线索
async fn wakeup_port_dispatches_merges_and_consumes() {
    let pool = pool().await;
    let world = seed_world(&pool).await;
    let instruction = "look at the latest state and report back";
    let wakeup = seed_due_wakeup(&pool, &world, instruction).await;
    let port = McWakeupDispatchPort::from_pool(pool.clone(), Arc::new(Hub::new()));

    // `Tick` 的前半：这条到点规则必须在候选里。
    let candidates = port.tick_candidates().await.expect("tick_candidates");
    assert!(
        candidates.iter().any(|row| row.id == wakeup.id),
        "到点的 wakeup 必须是候选"
    );

    let before = Utc::now();
    assert_eq!(
        port.dispatch_wakeup(&wakeup).await.expect("dispatch"),
        WakeupOutcome::Dispatched
    );

    let task = wakeup_task(&pool, wakeup.id).await.expect("queue row");
    let task_id: Uuid = task.try_get("id").unwrap();
    assert_eq!(task.try_get::<String, _>("status").unwrap(), "queued");
    // `issue.priority='high'` ⇒ `priorityToInt` = 3（`task.go:7038`）。
    assert_eq!(task.try_get::<i32, _>("priority").unwrap(), 3);
    assert_eq!(
        task.try_get::<Option<Uuid>, _>("runtime_id").unwrap(),
        Some(world.runtime)
    );
    assert_eq!(task.try_get::<Uuid, _>("issue_id").unwrap(), world.issue);
    assert_eq!(task.try_get::<Uuid, _>("agent_id").unwrap(), world.agent);
    assert_eq!(
        task.try_get::<String, _>("trigger_summary").unwrap(),
        format!("Wakeup: {instruction}")
    );
    assert_eq!(
        task.try_get::<Option<Uuid>, _>("originator_user_id")
            .unwrap(),
        Some(world.user)
    );
    // `agent_task_queue_accountable_matches_originator` 的 CHECK 要求两者相等。
    assert_eq!(
        task.try_get::<Option<Uuid>, _>("accountable_user_id")
            .unwrap(),
        Some(world.user)
    );
    assert_eq!(
        task.try_get::<String, _>("originator_source").unwrap(),
        "trigger_owner"
    );
    assert_eq!(
        task.try_get::<String, _>("trigger_evidence_kind").unwrap(),
        "issue_wakeup"
    );
    assert_eq!(
        task.try_get::<Uuid, _>("trigger_evidence_ref_id").unwrap(),
        wakeup.id
    );
    // 缺口：`buildRuntimeMCPOverlay` 不可得 ⇒ 两列 NULL（合法形态：agent 无 Composio 绑定）。
    assert!(task
        .try_get::<Option<serde_json::Value>, _>("runtime_mcp_overlay")
        .unwrap()
        .is_none());
    assert!(task
        .try_get::<Option<serde_json::Value>, _>("runtime_connected_apps")
        .unwrap()
        .is_none());

    let context: serde_json::Value = task.try_get("context").unwrap();
    assert_eq!(context["wakeup_id"], json!(wakeup.id.to_string()));
    assert_eq!(context["wakeup_revision"], json!(1));
    assert_eq!(context["wakeup_evidence"]["version"], json!(1));
    assert_eq!(
        context["wakeup_evidence"]["facts"][0]["event_type"],
        json!("time.due")
    );
    let note: String = task.try_get("handoff_note").unwrap();
    assert!(
        note.starts_with(&format!(
            "Wakeup {} triggered. Instruction:\n{instruction}\n",
            wakeup.id
        )),
        "handoff note 头: {note}"
    );
    assert!(
        note.contains("time.due "),
        "handoff note 必须带事件行: {note}"
    );

    // `consume_dispatch`：收据认领 + `last_task_id` 写回 + 调度推进。
    let after = reload(&pool, wakeup.id).await;
    assert_eq!(after.last_task_id, Some(task_id));
    assert!(after.enabled, "continuous 规则派发后仍启用");
    assert!(
        after.next_fire_at.expect("next fire") > before,
        "every 规则必须往后推一格（不是停留在过去）"
    );
    let consumed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM issue_wakeup_receipt \
          WHERE wakeup_id = $1 AND processed_at IS NOT NULL AND task_id = $2",
    )
    .bind(wakeup.id)
    .bind(task_id)
    .fetch_one(&pool)
    .await
    .expect("count receipts");
    assert_eq!(consumed, 1, "本轮收据必须被认领到这条 task 上");

    // 合并分支（`previous_task: Some(queued)`）：新收据落进**同一条**队列行。
    let mut conn = pool.acquire().await.expect("acquire");
    mc_repos::wakeup::receipt::record(
        &mut conn,
        wakeup.id,
        after.revision,
        &format!("manual-{}", Uuid::new_v4()),
        "comment.created",
        &json!({"comment_id": Uuid::new_v4()}),
    )
    .await
    .expect("record extra receipt");
    drop(conn);
    let again = reload(&pool, wakeup.id).await;
    assert_eq!(
        port.dispatch_wakeup(&again).await.expect("merge dispatch"),
        WakeupOutcome::Dispatched
    );
    let merged = wakeup_task(&pool, wakeup.id).await.expect("queue row");
    assert_eq!(
        merged.try_get::<Uuid, _>("id").unwrap(),
        task_id,
        "不新建第二条队列行"
    );
    let merged_note: String = merged.try_get("handoff_note").unwrap();
    assert!(
        merged_note.contains("comment.created"),
        "合并后的 note 必须含新证据: {merged_note}"
    );
    let merged_context: serde_json::Value = merged.try_get("context").unwrap();
    // `kind != "event"` 的规则每轮重算证据（M5-6 `merge_wakeup_evidence` 的既有语义）。
    assert_eq!(
        merged_context["wakeup_evidence"]["facts"][0]["event_type"],
        json!("comment.created")
    );
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM issue_wakeup_receipt WHERE wakeup_id = $1 AND processed_at IS NULL",
    )
    .bind(wakeup.id)
    .fetch_one(&pool)
    .await
    .expect("count pending");
    assert_eq!(pending, 0, "合并后不该留下未处理收据");

    // 幂等：没有新收据 + 计时器已在未来 ⇒ 事务内收尾。
    let settled = reload(&pool, wakeup.id).await;
    assert_eq!(
        port.dispatch_wakeup(&settled).await.expect("settle"),
        WakeupOutcome::Settled
    );

    // 失败记账：`last_error` 按 rune 截到 500 + `…`（`LAST_ERROR_MAX_RUNES`）。
    let long = "x".repeat(600);
    port.note_dispatch_failure(wakeup.id, &long)
        .await
        .expect("note failure");
    let failed = reload(&pool, wakeup.id).await;
    let last_error = failed.last_error.unwrap_or_default();
    assert_eq!(last_error.chars().count(), 501, "500 rune + 省略号");
    assert!(last_error.ends_with('…'));

    // 触摸：`updated_at` 前进（每轮「刚被看过」的 UI 信号）。
    let touched_before = reload(&pool, wakeup.id).await.updated_at;
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    port.touch_dispatch(wakeup.id).await.expect("touch");
    assert!(reload(&pool, wakeup.id).await.updated_at > touched_before);

    cleanup(&pool, &world).await;
}

// ---------------------------------------------------------------------------
// 3. 装配（`build` / `spawn` / `shutdown`）
// ---------------------------------------------------------------------------

/// 「两个 job 真的挂在循环上」的两段证据：
/// ① 注册是纯内存的 ⇒ `Manager::jobs()` 里恰好两个（不需要数据库也能查）；
/// ② 起真循环 ⇒ 两个 job 各自在 `sys_cron_executions` 里留下**终态**租约行，
///    并且我的到点 wakeup **由循环自己**（不经端口直调）写进了队列。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 注册 / 起循环 / 等两路租约 / 收尾是一条链，拆开就看不出「注册先于 spawn」
async fn build_registers_both_jobs_and_the_loop_claims_leases() {
    use mc_scheduler::jobs::{autopilot as autopilot_job, issue_wakeup as wakeup_job};

    let pool = pool().await;
    // 唯一键是 (job_name, scope_kind, scope_id, plan_time)：两个 job 名是常量，同 job 的旧行会
    // 让本轮认领不到租约 ⇒ 先清掉（测试库的全局审计表，见模块文档）。
    sqlx::query(
        "DELETE FROM sys_cron_executions \
          WHERE job_name = ANY(ARRAY['autopilot_schedule_dispatch', 'issue_wakeup_dispatch'])",
    )
    .execute(&pool)
    .await
    .expect("clear stale lease rows");

    let world = seed_world(&pool).await;
    // autopilot 侧：`* * * * *` 的 **老** trigger（十分钟前出生）⇒ `plans_hook` 本 tick 必有一格
    //（刚出生的 trigger 要等下一个整分，见 [`seed_due_schedule_trigger`]）。
    let autopilot = seed_autopilot(&pool, &world, "active").await;
    let trigger = seed_due_schedule_trigger(&pool, autopilot).await;
    // wakeup 侧：到点规则 ⇒ 循环一轮就该有队列行。
    let wakeup = seed_due_wakeup(&pool, &world, "loop-driven dispatch").await;

    let db = mc_db::Db::connect(
        &std::env::var("MULTICA_TEST_DATABASE_URL").expect("set MULTICA_TEST_DATABASE_URL"),
        4,
        1,
    )
    .await
    .expect("connect Db");
    let runner = format!("itest-m5-9-{}", Uuid::new_v4());
    let manager = super::build(
        &db,
        Arc::new(Hub::new()),
        Options::default()
            .with_runner_id(runner.clone())
            .with_tick_interval(StdDuration::from_millis(500)),
    )
    .expect("build scheduler");
    let mut names: Vec<&str> = manager.jobs().iter().map(|job| job.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![autopilot_job::JOB_NAME, wakeup_job::JOB_NAME],
        "两个 job 都必须在 spawn 之前注册"
    );

    let handle: SchedulerHandle = manager.spawn();
    let observed = pool.clone();
    let observed_runner = runner.clone();
    let observed_trigger = trigger;
    // 等的条件里带上「autopilot 面租约的 scope 就是我的 trigger」：同库别的可调度 trigger 也
    // 会留下 autopilot 行（本用例的 fixture 只保证**自己**、不保证全库），不写进条件就会
    // 被别的 trigger 提前满足。
    wait_until(
        "两个 job 的终态租约行（含我的 trigger scope）",
        StdDuration::from_secs(60),
        || {
            let pool = observed.clone();
            let runner = observed_runner.clone();
            let trigger = observed_trigger.to_string();
            async move {
                let rows: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
                    "SELECT job_name, status, error_msg, scope_id FROM sys_cron_executions \
                  WHERE runner_id = $1 AND status IN ('SUCCESS','FAILED')",
                )
                .bind(&runner)
                .fetch_all(&pool)
                .await
                .expect("read lease rows");
                let jobs: std::collections::BTreeSet<&str> =
                    rows.iter().map(|row| row.0.as_str()).collect();
                let mine = rows
                    .iter()
                    .any(|row| row.0 == autopilot_job::JOB_NAME && row.3 == trigger);
                if jobs.len() == 2 && mine {
                    return true;
                }
                // 打印出来便于失败时定位（哪个 job / 哪个 scope 没跑成）。
                eprintln!("lease rows so far: {rows:?}");
                false
            }
        },
    )
    .await;

    // 循环自己派发的队列行（不是我直调端口）。
    let observed = pool.clone();
    let observed_wakeup = wakeup.id;
    wait_until(
        "循环派发的队列行",
        StdDuration::from_secs(30),
        || {
            let pool = observed.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM agent_task_queue WHERE context->>'wakeup_id' = $1",
                )
                .bind(observed_wakeup.to_string())
                .fetch_one(&pool)
                .await
                .expect("count queue rows")
                    >= 1
            }
        },
    )
    .await;

    handle.shutdown().await;

    // 两个 job 的终态行里各自的 scope 都在：autopilot 面的 scope 是本用例独有的 trigger id
    // ⇒ 这条断言不可能被同库别处的数据带偏。
    let scopes: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT job_name, scope_kind, scope_id FROM sys_cron_executions \
          WHERE runner_id = $1 AND status IN ('SUCCESS','FAILED')",
    )
    .bind(&runner)
    .fetch_all(&pool)
    .await
    .expect("read scopes");
    assert!(
        scopes
            .iter()
            .any(|(job, kind, id)| job == autopilot_job::JOB_NAME
                && kind == "autopilot_trigger"
                && id == &trigger.to_string()),
        "autopilot job 必须为我的 trigger 留下租约: {scopes:?}"
    );
    assert!(
        scopes.iter().any(|(job, _, _)| job == wakeup_job::JOB_NAME),
        "wakeup job 必须留下租约: {scopes:?}"
    );

    // 收尾：审计行按 runner 清掉（全局表，别留给下一次跑）。
    let _ = sqlx::query("DELETE FROM sys_cron_executions WHERE runner_id = $1")
        .bind(&runner)
        .execute(&pool)
        .await;
    cleanup(&pool, &world).await;
}
