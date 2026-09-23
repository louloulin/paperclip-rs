//! 派发**服务层**三块的 e2e（M5-4 / LUM-1569）：① `create_issue`、② `run_only`、③ 同步回写。
//!
//! # 为什么这三块在这个 target 里测
//!
//! 它们没有 HTTP 面（`/runs` 只是读；`/trigger` 只覆盖「跳过」那条），得直接调
//! `mc_autopilot::dispatch::*`。而 `mc-autopilot` **没有** `tokio` 依赖，且本波（`docs/44` §5.2）
//! 禁止新增第三方依赖 ⇒ 那个 crate 里写不出 `#[tokio::test]`。本 target 已经同时有
//! `tokio` 与真库夹具，于是服务层的测试宿主在这里（与 `usage.rs` 借同一套 `support` 同源）。
//!
//! # 三块的判据
//!
//! ① `create_issue`：issue 与 task **同事务**落库、run 回链两列（`issue_id` + `task_id`）；
//!    重复守卫窗口内同标题第二次派发 ⇒ run `skipped` + `already_active`（不是 500、不是新 issue）。
//! ② `run_only`：**没有 issue**，任务只挂 `autopilot_run_id`，run 直接 `running`。
//! ③ 同步回写：任务终态 → run 终态（`result` 带回来；失败带 `failure_reason`）；
//!    issue 终态 → `create_issue` 链的 run 终态；`sync_from_linked_issue_task` 在还有活跃任务时
//!    **等**（返回 `None`），且 `run_only` 的 run 归 `sync_from_task` 管（返回 `None`）。
//!
//! ⚠️ 这三块**都不发** daemon 唤醒（`NotifyTaskEnqueued` / `task:queued`，本切片 `known_gap`），
//! 所以任务行落 `queued` 后没人取走：本文件只钉「行与字段」，不钉「被执行」。

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use mc_autopilot::dispatch::{AutopilotDispatcher, DispatchOutcome, DispatchRequest, ReasonCode};
use mc_repos::autopilot::run::get_autopilot;
use mc_repos::autopilot::AutopilotRow;

use super::support::{cleanup, connect, seed_workspace};

/// 铺一个 `kind='user'` + 绑定 runtime 的 agent（`lock_task_owner_rows` 要求 agent 与 runtime
/// 都能解析到同一个工作区，否则 `create_task` 返回 `None` ⇒ 整条派发被栅栏拒掉）。
async fn seed_agent(pool: &PgPool, workspace_id: Uuid, owner_id: Uuid) -> Uuid {
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
    sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, \
             permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, 'private') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// 铺一个 **未绑 runtime** 的 agent（`agent.runtime_id IS NULL`）：上游 `AgentReadiness`
/// （`agent_ready.go:143`）判它 `agent_runtime_required`，本地 schema 又不允许无 runtime 的
/// `queued` 任务 ⇒ 派发必须**提前成 skip**，不能变成 500。
async fn seed_unbound_agent(pool: &PgPool, workspace_id: Uuid, owner_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, \
             permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', NULL, $3, 'private') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-unbound-{}", Uuid::new_v4()))
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert unbound agent")
}

/// 铺一条可派发的 autopilot（`support::seed_autopilot` 把 `execution_mode` 钉死在 `run_only`，
/// 这一条是那个函数的本地加宽版；`support.rs` 是 M5-1 的写集，不能改）。
async fn seed_dispatchable_autopilot(
    pool: &PgPool,
    workspace_id: Uuid,
    execution_mode: &str,
    assignee_id: Uuid,
    owner_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot \
            (workspace_id, title, description, assignee_type, assignee_id, status, execution_mode, \
             created_by_type, created_by_id) \
         VALUES ($1, $2, $3, 'agent', $4, 'active', $5, 'member', $6) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-{}", Uuid::new_v4()))
    .bind("autopilot e2e body")
    .bind(assignee_id)
    .bind(execution_mode)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert autopilot")
}

/// 读回 `AutopilotRow`（派发入参要整行）。
async fn load_autopilot(pool: &PgPool, id: Uuid) -> AutopilotRow {
    get_autopilot(pool, id).await.expect("load autopilot row")
}

/// 清场：任务 → issue 订阅/收件箱 → issue → agent(runtime) → autopilot/workspace/user。
async fn cleanup_all(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    for sql in [
        "DELETE FROM agent_task_queue WHERE agent_id IN (SELECT id FROM agent WHERE workspace_id = $1)",
        "DELETE FROM issue_subscriber WHERE issue_id IN (SELECT id FROM issue WHERE workspace_id = $1)",
        "DELETE FROM inbox_item WHERE workspace_id = $1",
        "DELETE FROM issue WHERE workspace_id = $1",
        "DELETE FROM agent_runtime WHERE workspace_id = $1",
        "DELETE FROM agent WHERE workspace_id = $1",
    ] {
        let _ = sqlx::query(sql).bind(workspace_id).execute(pool).await;
    }
    cleanup(pool, workspace_id, user_ids).await;
}

/// ① `create_issue`：issue + task 同事务、run 回链两列、标题模板渲染。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn create_issue_dispatch_links_the_issue_and_the_task() {
    let Some((pool, _db)) = connect().await else {
        println!("skip create_issue_dispatch_links_the_issue_and_the_task: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_dispatchable_autopilot(&pool, ws, "create_issue", agent, owner).await;
    let autopilot = load_autopilot(&pool, autopilot_id).await;

    let dispatcher = AutopilotDispatcher::new(pool.clone());
    let outcome = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch create_issue");
    assert!(!outcome.is_skipped(), "{outcome:?}");
    assert!(!outcome.reused);
    assert_eq!(outcome.run.status, "issue_created", "{outcome:?}");
    let issue_id = outcome.run.issue_id.expect("run.issue_id");
    // 上游 `EnqueueTaskForIssue` 不认识 run ⇒ `create_issue` 的 run 与 task **不互相挂**：
    // run.task_id 与 task.autopilot_run_id 都是 NULL，两者只经 `issue_id` 相连。
    assert!(outcome.run.task_id.is_none(), "{outcome:?}");

    // issue：`origin_type='autopilot'` + 回指 autopilot，标题来自模板（无 `{date}` 变量 ⇒ 原样）。
    let issue: (String, String, String, Uuid) =
        sqlx::query_as("SELECT origin_type, title, status, origin_id FROM issue WHERE id = $1")
            .bind(issue_id)
            .fetch_one(&pool)
            .await
            .expect("load issue");
    assert_eq!(issue.0, "autopilot");
    assert_eq!(issue.1, autopilot.title);
    assert_eq!(issue.2, "todo");
    assert_eq!(issue.3, autopilot_id);

    // task：只挂 issue（`autopilot_run_id` 留 NULL），且 `originator_source` 是 `direct_human`
    // （手动触发带了人）。
    let task: (Uuid, Option<Uuid>, Option<Uuid>, String, Option<String>) = sqlx::query_as(
        "SELECT id, issue_id, autopilot_run_id, status, originator_source FROM agent_task_queue \
         WHERE issue_id = $1",
    )
    .bind(issue_id)
    .fetch_one(&pool)
    .await
    .expect("load task");
    assert_eq!(task.1, Some(issue_id));
    assert!(
        task.2.is_none(),
        "create_issue 的任务不该挂 run（上游挂法）"
    );
    assert_eq!(task.3, "queued");
    assert_eq!(task.4.as_deref(), Some("direct_human"));
    // 任务确实落在 leader（= assignee 解析出来的那个 agent）名下。
    let task_agent: Uuid =
        sqlx::query_scalar("SELECT agent_id FROM agent_task_queue WHERE id = $1")
            .bind(task.0)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(task_agent, agent);

    // 重复守卫：60s 窗口内同标题第二次派发 ⇒ run 落 `skipped` + `already_active`，
    // 且**不**产生第二个 issue。
    let again = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("second dispatch");
    assert!(again.is_skipped(), "{again:?}");
    assert_eq!(again.reason_code, Some(ReasonCode::AlreadyActive));
    assert_eq!(again.run.status, "skipped");
    assert!(again.run.issue_id.is_none(), "{again:?}");
    let issue_count: i64 = sqlx::query_scalar("SELECT count(*) FROM issue WHERE workspace_id = $1")
        .bind(ws)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(issue_count, 1, "重复守卫必须挡住第二个 issue");

    cleanup_all(&pool, ws, &[owner]).await;
}

/// ② `run_only`：不建 issue，任务只挂 `autopilot_run_id`，run 直接 `running`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn run_only_dispatch_enqueues_a_task_without_an_issue() {
    let Some((pool, _db)) = connect().await else {
        println!("skip run_only_dispatch_enqueues_a_task_without_an_issue: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_dispatchable_autopilot(&pool, ws, "run_only", agent, owner).await;
    let autopilot = load_autopilot(&pool, autopilot_id).await;

    let dispatcher = AutopilotDispatcher::new(pool.clone());
    let outcome = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch run_only");
    assert!(!outcome.is_skipped(), "{outcome:?}");
    assert_eq!(outcome.run.status, "running");
    assert!(outcome.run.issue_id.is_none(), "{outcome:?}");
    let task_id = outcome.run.task_id.expect("run.task_id");

    let task: (Option<Uuid>, Option<Uuid>, String, Option<String>) = sqlx::query_as(
        "SELECT issue_id, autopilot_run_id, status, trigger_summary FROM agent_task_queue \
         WHERE id = $1",
    )
    .bind(task_id)
    .fetch_one(&pool)
    .await
    .expect("load task");
    assert!(task.0.is_none(), "run_only 不该挂 issue");
    assert_eq!(task.1, Some(outcome.run.id));
    assert_eq!(task.2, "queued");
    assert_eq!(task.3.as_deref(), Some(autopilot.title.as_str()));

    // issue 与 workspace 都没变多。
    let issues: i64 = sqlx::query_scalar("SELECT count(*) FROM issue WHERE workspace_id = $1")
        .bind(ws)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(issues, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

/// ③a 任务终态回写：`completed` 带 `result`、`failed` 带 `failure_reason`、再次回写不改终态。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn sync_from_task_settles_the_run_once() {
    let Some((pool, _db)) = connect().await else {
        println!("skip sync_from_task_settles_the_run_once: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_dispatchable_autopilot(&pool, ws, "run_only", agent, owner).await;
    let autopilot = load_autopilot(&pool, autopilot_id).await;
    let dispatcher = AutopilotDispatcher::new(pool.clone());

    // 成功：`result` 原样落库。
    let first = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch #1");
    let task_id = first.run.task_id.expect("task_id");
    let settled = dispatcher
        .sync_from_task(task_id, "completed", Some(&json!({"ok": true})), None)
        .await
        .expect("sync completed")
        .expect("run found");
    assert_eq!(settled.status, "completed");
    assert_eq!(settled.result, Some(json!({"ok": true})));
    assert!(settled.completed_at.is_some(), "{settled:?}");
    assert!(settled.failure_reason.is_none(), "{settled:?}");

    // 重复回写：上游没有「已是终态就别再改」的闸门（裸 UPDATE），本地照抄 ⇒ 后到的回调会覆盖。
    let again = dispatcher
        .sync_from_task(task_id, "failed", None, Some("late failure"))
        .await
        .expect("sync again")
        .expect("run found");
    assert_eq!(again.status, "failed");
    assert_eq!(again.failure_reason.as_deref(), Some("late failure"));

    // 失败：`failure_reason` 取任务给的错；`result` 不写。
    let second = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch #2");
    let task2 = second.run.task_id.expect("task_id #2");
    let failed = dispatcher
        .sync_from_task(task2, "failed", None, Some("boom"))
        .await
        .expect("sync failed")
        .expect("run found");
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.failure_reason.as_deref(), Some("boom"));
    assert!(failed.result.is_none(), "{failed:?}");

    // 还在飞的状态（`queued` / `running`）：回 `None` 且不改库。
    let third = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch #3");
    let task3 = third.run.task_id.expect("task_id #3");
    assert!(dispatcher
        .sync_from_task(task3, "running", None, None)
        .await
        .expect("sync running")
        .is_none());
    let still: (String,) = sqlx::query_as("SELECT status FROM autopilot_run WHERE id = $1")
        .bind(third.run.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(still.0, "running");

    cleanup_all(&pool, ws, &[owner]).await;
}

/// ③b issue 终态回写：`done`/`in_review` → completed；`cancelled`/`blocked` → failed；
/// 非 autopilot 来源的 issue 一律不管。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn sync_from_issue_status_settles_the_create_issue_run() {
    let Some((pool, _db)) = connect().await else {
        println!("skip sync_from_issue_status_settles_the_create_issue_run: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_dispatchable_autopilot(&pool, ws, "create_issue", agent, owner).await;
    let autopilot = load_autopilot(&pool, autopilot_id).await;
    let dispatcher = AutopilotDispatcher::new(pool.clone());

    // `done` → completed。
    let done = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch done");
    let done_issue = done.run.issue_id.expect("issue_id");
    sqlx::query("UPDATE issue SET status = 'done' WHERE id = $1")
        .bind(done_issue)
        .execute(&pool)
        .await
        .unwrap();
    let settled = dispatcher
        .sync_from_issue_status(done_issue)
        .await
        .expect("sync done")
        .expect("run found");
    assert_eq!(settled.status, "completed");
    assert_eq!(settled.id, done.run.id);

    // 终态 run 不再被回调改写（`find_active_by_issue` 只看见在飞的）。
    assert!(dispatcher
        .sync_from_issue_status(done_issue)
        .await
        .expect("sync done twice")
        .is_none());

    // `cancelled` → failed，`failure_reason` 里保留**原始** issue.status。
    let cancelled = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch cancelled");
    let cancelled_issue = cancelled.run.issue_id.expect("issue_id");
    sqlx::query("UPDATE issue SET status = 'cancelled' WHERE id = $1")
        .bind(cancelled_issue)
        .execute(&pool)
        .await
        .unwrap();
    let failed = dispatcher
        .sync_from_issue_status(cancelled_issue)
        .await
        .expect("sync cancelled")
        .expect("run found");
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.failure_reason.as_deref(), Some("issue cancelled"));

    // 非终态：`todo` 回 `None`（上游 switch 没有 default 分支）。
    let open = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch open");
    let open_issue = open.run.issue_id.expect("issue_id");
    assert!(dispatcher
        .sync_from_issue_status(open_issue)
        .await
        .expect("sync todo")
        .is_none());

    // 非 autopilot 来源的 issue：立刻 `None`（连 run 都不查）。
    let foreign_issue: Uuid = sqlx::query_scalar(
        "INSERT INTO issue (workspace_id, number, identifier, title, status, creator_type, \
             creator_id) \
         VALUES ($1, 9999, 'ITEST-9999', 'manual issue', 'done', 'member', $2) RETURNING id",
    )
    .bind(ws)
    .bind(owner)
    .fetch_one(&pool)
    .await
    .expect("insert foreign issue");
    assert!(dispatcher
        .sync_from_issue_status(foreign_issue)
        .await
        .expect("sync foreign")
        .is_none());

    cleanup_all(&pool, ws, &[owner]).await;
}

/// ③c `sync_from_linked_issue_task`：还有活跃任务时**等**；`run_only` 的 run 归另一条链；
/// 只有 `failed` 才处理。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn sync_from_linked_issue_task_waits_for_active_tasks() {
    let Some((pool, _db)) = connect().await else {
        println!("skip sync_from_linked_issue_task_waits_for_active_tasks: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_dispatchable_autopilot(&pool, ws, "create_issue", agent, owner).await;
    let autopilot = load_autopilot(&pool, autopilot_id).await;
    let dispatcher = AutopilotDispatcher::new(pool.clone());

    let outcome = dispatcher
        .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
        .await
        .expect("dispatch");
    let issue_id = outcome.run.issue_id.expect("issue_id");
    let task_id: Uuid = sqlx::query_scalar("SELECT id FROM agent_task_queue WHERE issue_id = $1")
        .bind(issue_id)
        .fetch_one(&pool)
        .await
        .expect("create_issue task");

    // 任务还在 `queued` ⇒ 有活跃任务 ⇒ 等（`None`），run 不动。
    assert!(dispatcher
        .sync_from_linked_issue_task(issue_id, task_id, "failed", Some("early"))
        .await
        .expect("sync with active task")
        .is_none());
    let open: (String,) = sqlx::query_as("SELECT status FROM autopilot_run WHERE id = $1")
        .bind(outcome.run.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(open.0, "issue_created");

    // 非 `failed` 一律不管（`cancelled` 不走这条）。
    assert!(dispatcher
        .sync_from_linked_issue_task(issue_id, task_id, "cancelled", None)
        .await
        .expect("sync cancelled")
        .is_none());

    // 任务转终态失败 ⇒ 没有活跃任务了 ⇒ run 判失败。
    sqlx::query("UPDATE agent_task_queue SET status = 'failed' WHERE id = $1")
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();
    let failed = dispatcher
        .sync_from_linked_issue_task(issue_id, task_id, "failed", Some("infra blew up"))
        .await
        .expect("sync failed")
        .expect("run found");
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.failure_reason.as_deref(), Some("infra blew up"));

    // `run_only` 的 run 没有 issue ⇒ 这条链看不见它（由 `sync_from_task` 收口）。
    let run_only_ap = seed_dispatchable_autopilot(&pool, ws, "run_only", agent, owner).await;
    let run_only = load_autopilot(&pool, run_only_ap).await;
    let ro = dispatcher
        .dispatch(DispatchRequest::manual(&run_only, None, None, Some(owner)))
        .await
        .expect("dispatch run_only");
    assert!(dispatcher
        .sync_from_linked_issue_task(issue_id, ro.run.task_id.expect("task"), "failed", None)
        .await
        .expect("sync linked for run_only")
        .is_none());

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 无 runtime 绑定的 agent：两条线都必须在建任务之前跳过（`agent_runtime_required`），
/// 而不是把 `agent_task_queue` 的 CHECK 撞成 500。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn unbound_agent_is_skipped_before_the_task_insert() {
    let Some((pool, _db)) = connect().await else {
        println!("skip unbound_agent_is_skipped_before_the_task_insert: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_unbound_agent(&pool, ws, owner).await;
    let dispatcher = AutopilotDispatcher::new(pool.clone());

    for mode in ["run_only", "create_issue"] {
        let autopilot_id = seed_dispatchable_autopilot(&pool, ws, mode, agent, owner).await;
        let autopilot = load_autopilot(&pool, autopilot_id).await;
        let outcome = dispatcher
            .dispatch(DispatchRequest::manual(&autopilot, None, None, Some(owner)))
            .await
            .expect("dispatch");
        assert!(outcome.is_skipped(), "{mode}: {outcome:?}");
        assert_eq!(outcome.reason_code, Some(ReasonCode::AgentRuntimeRequired));
        assert_eq!(outcome.run.status, "skipped", "{mode}: {outcome:?}");
        assert_eq!(
            outcome.run.failure_reason.as_deref(),
            Some("assignee agent has no runtime bound"),
            "{mode}"
        );
        assert!(outcome.run.task_id.is_none(), "{mode}: {outcome:?}");
        assert!(outcome.run.issue_id.is_none(), "{mode}: {outcome:?}");
        let tasks: i64 =
            sqlx::query_scalar("SELECT count(*) FROM agent_task_queue WHERE agent_id = $1")
                .bind(agent)
                .fetch_one(&pool)
                .await
                .expect("count tasks");
        assert_eq!(tasks, 0, "{mode}: 不该留下任务行");
    }

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 派发结果里的 `reused` 只在计划快路径出现；`DispatchOutcome` 的字段是本波对外契约的一部分。
#[allow(dead_code)]
fn _outcome_shape(outcome: &DispatchOutcome) -> (&str, Option<ReasonCode>, bool, Value) {
    (
        outcome.run.status.as_str(),
        outcome.reason_code,
        outcome.reused,
        json!({ "id": outcome.run.id }),
    )
}
