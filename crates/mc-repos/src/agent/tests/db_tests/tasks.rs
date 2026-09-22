//! `agent_task_queue` 的读聚合 / 取消（PG 集成测试，拆自 `db_tests.rs`）。
//!
//! 上游不变量逼着 fixture 必须给足字段（也是本片最容易踩的坑）：
//! - `agent_task_queue_active_requires_runtime`（`251_agent_runtime_unbind.up.sql:59`）：
//!   `CHECK (runtime_id IS NOT NULL OR completed_at IS NOT NULL) NOT VALID` —— 在飞
//!   （没有 `completed_at`）的行**必须**有真 runtime。
//! - `set_agent_task_comment_thread` 触发器（`451_agent_task_comment_thread.up.sql:21`）
//!   会把 `context->>'wakeup_id'` 转 uuid ⇒ 假 `wakeup_id` 必须是真 UUID。
//! - `capture_task_wakeup` 触发器在 `issue_id IS NULL` 时直接返回 ⇒ 不建 issue。

use super::*;
// 显式导入胜过 glob：`super::*` 连父模块的 `assert_eq` 宏一起带进来，
// 与 prelude 的同名宏在宏解析上冲突（E0659）。
use pretty_assertions::assert_eq;

/// 插一条任务行。
async fn seed_task(
    db: &Db,
    runtime_id: Uuid,
    agent_id: Uuid,
    status: &str,
    completed_ago_hours: Option<i32>,
) -> Uuid {
    // 终态才有 `completed_at`（在飞行必须留空，约束同上）。
    let completed = match completed_ago_hours {
        Some(hours) => sqlx::query_scalar(&format!("SELECT now() - INTERVAL '{hours} hours'"))
            .fetch_one(db.pool())
            .await
            .expect("now()"),
        None => None,
    };
    let completed_at: Option<chrono::DateTime<chrono::Utc>> = completed;
    sqlx::query_scalar(
        "INSERT INTO agent_task_queue (agent_id, runtime_id, status, completed_at) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(agent_id)
    .bind(runtime_id)
    .bind(status)
    .bind(completed_at)
    .fetch_one(db.pool())
    .await
    .expect("task")
}

/// 插一条「未启动就被取消的委派回退行」（上游 `visibleTaskHistory` 要滤掉它）。
async fn seed_escalation_fallback(db: &Db, runtime_id: Uuid, agent_id: Uuid) -> Uuid {
    let parent = seed_task(db, runtime_id, agent_id, "cancelled", Some(1)).await;
    sqlx::query_scalar(
        "INSERT INTO agent_task_queue (agent_id, runtime_id, status, completed_at, escalation_for_task_id) \
         VALUES ($1, $2, 'cancelled', now(), $3) RETURNING id",
    )
    .bind(agent_id)
    .bind(runtime_id)
    .bind(parent)
    .fetch_one(db.pool())
    .await
    .expect("escalation fallback")
}

// ---------------------------------------------------------------------------
// 场景
// ---------------------------------------------------------------------------

/// 一套任务场景：3 条在飞 + 1 条 deferred 唤醒 + 终态 + 委派回退 + 另一个 agent。
struct TaskScene {
    agent: AgentRow,
    other: AgentRow,
    queued: Uuid,
    running: Uuid,
    waiting: Uuid,
    completed: Uuid,
    failed: Uuid,
    escalated: Uuid,
    deferred: Uuid,
}

async fn seed_task_scene(db: &Db, ws: Id) -> TaskScene {
    let repo = AgentRepo::new(db.clone());
    let runtime_id = seed_runtime(db, ws).await;

    let mut input = new_agent(ws, "tasky");
    input.runtime_id = Some(runtime_id);
    let agent = repo.create(&input).await.expect("a");
    let mut other_input = new_agent(ws, "tasky-2");
    other_input.runtime_id = Some(runtime_id);
    let other = repo.create(&other_input).await.expect("b");

    // 在飞三种（`queued` / `running` / `waiting_local_directory`）+ 终态两种
    let queued = seed_task(db, runtime_id, agent.id().0, "queued", None).await;
    let running = seed_task(db, runtime_id, agent.id().0, "running", None).await;
    let waiting = seed_task(
        db,
        runtime_id,
        agent.id().0,
        "waiting_local_directory",
        None,
    )
    .await;
    let completed = seed_task(db, runtime_id, agent.id().0, "completed", Some(48)).await;
    let failed = seed_task(db, runtime_id, agent.id().0, "failed", Some(24)).await;
    let escalated = seed_escalation_fallback(db, runtime_id, agent.id().0).await;
    // deferred + 真 wakeup（snapshot 的 deferred 分支；uuid 会被触发器 cast）
    let deferred: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_task_queue (agent_id, runtime_id, status, context) \
         VALUES ($1, $2, 'deferred', jsonb_build_object('wakeup_id', gen_random_uuid())) RETURNING id",
    )
    .bind(agent.id().0)
    .bind(runtime_id)
    .fetch_one(db.pool())
    .await
    .expect("deferred");
    // 另一个 agent 的任务（不该出现在 `list_tasks(agent)` 里）
    seed_task(db, runtime_id, other.id().0, "running", None).await;

    TaskScene {
        agent,
        other,
        queued,
        running,
        waiting,
        completed,
        failed,
        escalated,
        deferred,
    }
}

// ---------------------------------------------------------------------------
// 读：任务列表 + workspace 快照
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_task_list_and_snapshot_hide_escalation_fallbacks_and_rank_outcomes() {
    let (db, ws) = fixture!();
    let scene = seed_task_scene(&db, ws).await;
    let repo = AgentRepo::new(db.clone());
    let agent = &scene.agent;

    // `list_tasks`：上游 `ListAgentTasks`（DESC）+ `visibleTaskHistory` 过滤
    let tasks = repo.list_tasks(agent.id()).await.expect("tasks");
    let ids: Vec<Uuid> = tasks.iter().map(|t| t.id).collect();
    assert!(
        !ids.contains(&scene.escalated),
        "委派回退行必须被 visibleTaskHistory 滤掉"
    );
    assert!(tasks.iter().all(|t| t.agent_id == agent.id().0));
    assert!(tasks.iter().all(is_visible_task_history));
    assert!(
        tasks.windows(2).all(|w| w[0].created_at >= w[1].created_at),
        "created_at DESC"
    );
    assert_eq!(
        tasks.iter().filter(|t| t.is_active()).count(),
        3,
        "ACTIVE_TASK_STATUSES 只有 4 个且不含 deferred"
    );
    assert_eq!(tasks.iter().filter(|t| t.status == "deferred").count(), 1);
    let seen = tasks
        .iter()
        .find(|t| t.id == scene.completed)
        .expect("completed row");
    assert!(seen.is_outcome());
    assert!(!seen.is_active());
    assert_eq!(seen.attempt, 1);
    assert_eq!(seen.max_attempts, 2);

    // snapshot：在飞半边 + Top-1 结果半边（cancelled 不算结果）
    let snapshot = repo.task_snapshot(ws).await.expect("snapshot");
    assert!(snapshot
        .iter()
        .all(|t| t.agent_id == agent.id().0 || t.agent_id == scene.other.id().0));
    let mine: Vec<&AgentTaskRow> = snapshot
        .iter()
        .filter(|t| t.agent_id == agent.id().0)
        .collect();
    // 4 条在飞（含 deferred+wakeup）+ 1 条 Top-1 结果（failed 比 completed 新）
    assert_eq!(mine.len(), 5, "{mine:?}");
    assert_eq!(
        mine.iter().filter(|t| t.status == "deferred").count(),
        1,
        "带 wakeup_id 的 deferred 计入在飞半边"
    );
    let outcome = mine
        .iter()
        .find(|t| t.is_outcome())
        .expect("top-1 outcome")
        .to_owned();
    assert_eq!(
        outcome.id, scene.failed,
        "只取最新一条终态（cancelled 不算）"
    );
    assert!(!mine.iter().any(|t| t.id == scene.escalated));

    teardown(&db, ws).await;
}

// ---------------------------------------------------------------------------
// 读聚合 + 取消
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_task_cancel_and_aggregations() {
    let (db, ws) = fixture!();
    let scene = seed_task_scene(&db, ws).await;
    let repo = AgentRepo::new(db.clone());
    let agent = &scene.agent;

    // run counts：30 天内全部任务都算（含在飞 / 取消）
    let counts = repo.run_counts_30d(ws).await.expect("counts");
    let mine_count = counts
        .iter()
        .find(|c| c.agent_id == agent.id().0)
        .expect("count row");
    // queued/running/waiting/completed/failed/deferred + 委派回退的父行与回退行
    assert_eq!(mine_count.run_count, 8);
    assert_eq!(mine_count.agent_id(), agent.id());

    // activity：按 `completed_at` 分桶 + FILTER 计数；deferred 无 completed_at → 无行
    let activity = repo.activity_30d(ws).await.expect("activity");
    let buckets: i32 = activity
        .iter()
        .filter(|b| b.agent_id == agent.id().0)
        .map(|b| b.task_count)
        .sum();
    // completed(1) + failed(1) + 委派回退行 + 它依赖的父行（都已完成）
    assert_eq!(buckets, 4, "{activity:?}");
    let failed_bucket = activity
        .iter()
        .find(|b| b.agent_id == agent.id().0 && b.failed_count > 0)
        .expect("failed bucket");
    assert_eq!(failed_bucket.failed_count, 1);
    assert_eq!(failed_bucket.completed_count, 0);
    let completed_bucket = activity
        .iter()
        .find(|b| b.agent_id == agent.id().0 && b.completed_count > 0)
        .expect("completed bucket");
    assert_eq!(completed_bucket.completed_count, 1);
    assert_eq!(completed_bucket.task_count, 1);

    // 取消：`deferred` 也在 WHERE 里（与 ACTIVE_TASK_STATUSES 不同）
    let cancelled = repo.cancel_tasks(agent.id()).await.expect("cancel");
    assert_eq!(
        cancelled, 4,
        "queued/running/waiting_local_directory/deferred"
    );
    let tasks = repo.list_tasks(agent.id()).await.expect("tasks after");
    for id in [scene.queued, scene.running, scene.waiting, scene.deferred] {
        let row = tasks.iter().find(|t| t.id == id).expect("cancelled row");
        assert_eq!(row.status, "cancelled");
        assert!(row.completed_at.is_some(), "取消要写 completed_at");
    }
    // 幂等：没有在飞行了 → 0
    assert_eq!(repo.cancel_tasks(agent.id()).await.expect("cancel 2"), 0);

    teardown(&db, ws).await;
}
