//! `mc_repos::squad_evaluation` 的**真库**契约测试（门禁 ⑥ 以 `--ignored` 跑）。
//!
//! 上游 `squad.go:976 RecordSquadLeaderEvaluation` 落的是**一条 `activity_log` 行**
//! （不是专用表），所以本文件要钉住两件事：
//!
//! 1. task 查询是**租户收窄**的（`JOIN agent` + `a.workspace_id`），且只投影 handler 读的 5 列；
//! 2. 写进去的 7 列逐列正确 —— 特别是 `actor_id = task.agent_id`（**不是** `squad.leader_id`），
//!    因为 `089_squad_no_action_activity_index` 的局部索引就是拿 `actor_id` 与
//!    `details->>'task_id'` 配对的；本条用**照抄索引谓词**的 `EXISTS` 查询来证明它可命中。
//!
//! 为什么在 `tests/` 而不在 `src/squad_evaluation.rs` 里：R7 单文件 800 行硬上限（门禁 ⑩）
//! —— 模块的文档已经写得比较满（与 `issue_view_pin_stats.rs` 同款理由）。
//!
//! 每个用例自己造 workspace + user 并在末尾清理，互不依赖执行顺序。

use mc_core::Id;
use mc_db::Db;
use mc_repos::squad_evaluation::{
    evaluation_details, SquadEvaluationRepo, ACTION_SQUAD_LEADER_EVALUATED,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// 建库连接（本文件用例全部 `#[ignore]`，只在显式跑真库时执行 ⇒ 缺变量直接 panic，
/// 门禁 ⑥ 把「静默跳过」变成「红」，防止假绿）。
async fn setup() -> (Db, PgPool) {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    let db = Db::connect(&url, 4, 1).await.expect("连接测试库");
    let pool = db.pool().clone();
    (db, pool)
}

/// 一个 workspace 的最小夹具：owner（人类）+ 两个 agent + 一条 issue + 一个 squad。
struct Fixture {
    pool: PgPool,
    workspace_id: Uuid,
    other_workspace: Uuid,
    users: Vec<Uuid>,
    leader_agent: Uuid,
    worker_agent: Uuid,
    issue_id: Uuid,
    other_issue_id: Uuid,
    squad_id: Uuid,
    /// 本 workspace 的 runtime（`agent_task_queue.runtime_id` 的 CHECK 需要它）。
    runtime_id: Uuid,
    /// 第二个 workspace 里的一条 leader 任务（用第一个 workspace 的 id 查不到）。
    foreign_task: Uuid,
}

async fn insert_workspace(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-lum1793', $1) RETURNING id",
    )
    .bind(format!("itest-lum1793-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace")
}

async fn insert_user(pool: &PgPool) -> Uuid {
    let user: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-lum1793-user', $1) RETURNING id"#,
    )
    .bind(format!("lum1793-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");
    user
}

async fn add_member(pool: &PgPool, workspace_id: Uuid, user: Uuid, role: &str) {
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(workspace_id)
        .bind(user)
        .bind(role)
        .execute(pool)
        .await
        .expect("insert member");
}

/// `agent_runtime` 不必建：head schema 里 `agent_task_queue.runtime_id` 可为空
/// （只有 `agent_id` 是 `NOT NULL`），而这条链只用得上 task 的 5 列。
async fn insert_agent(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', 'private') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-lum1793-agent-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

async fn insert_issue(pool: &PgPool, workspace_id: Uuid, number: i32, creator: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO issue(workspace_id, title, number, status, creator_type, creator_id) \
         VALUES ($1, 'itest-lum1793 issue', $2, 'todo', 'member', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(number)
    .bind(creator)
    .fetch_one(pool)
    .await
    .expect("insert issue")
}

async fn insert_squad(pool: &PgPool, workspace_id: Uuid, leader: Uuid, creator: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO squad(workspace_id, name, description, leader_id, creator_id) \
         VALUES ($1, $2, '', $3, $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-lum1793-squad-{}", Uuid::new_v4()))
    .bind(leader)
    .bind(creator)
    .fetch_one(pool)
    .await
    .expect("insert squad")
}

/// 建一条 `agent_runtime` 行。
///
/// 必须有：`251_agent_runtime_unbind` 的 CHECK
/// `agent_task_queue_active_requires_runtime`（`runtime_id IS NOT NULL OR completed_at IS NOT NULL`）
/// 不允许一条「还没完工又没有 runtime」的排队/运行中任务 —— 真实库里那种行也不存在。
async fn insert_runtime(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, status, last_seen_at) \
         VALUES ($1, $2, $3, 'local', 'claude', 'online', now()) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 建一条 `agent_task_queue` 行：`issue_id` 可为空（chat / quick-create 形态），
/// `is_leader_task` / `squad_id` 是这条 handler 的判据来源。
async fn insert_task(
    pool: &PgPool,
    agent_id: Uuid,
    runtime_id: Uuid,
    issue_id: Option<Uuid>,
    is_leader_task: bool,
    squad_id: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_task_queue \
            (agent_id, runtime_id, issue_id, status, is_leader_task, squad_id) \
         VALUES ($1, $2, $3, 'running', $4, $5) RETURNING id",
    )
    .bind(agent_id)
    .bind(runtime_id)
    .bind(issue_id)
    .bind(is_leader_task)
    .bind(squad_id)
    .fetch_one(pool)
    .await
    .expect("insert agent_task_queue")
}

async fn seed(pool: &PgPool) -> Fixture {
    let workspace_id = insert_workspace(pool).await;
    let other_workspace = insert_workspace(pool).await;

    let owner = insert_user(pool).await;
    add_member(pool, workspace_id, owner, "owner").await;

    let leader_agent = insert_agent(pool, workspace_id).await;
    let worker_agent = insert_agent(pool, workspace_id).await;
    let issue_id = insert_issue(pool, workspace_id, 1, owner).await;
    let other_issue_id = insert_issue(pool, workspace_id, 2, owner).await;
    let squad_id = insert_squad(pool, workspace_id, leader_agent, owner).await;
    // 251 的 CHECK 要求运行中的任务带 runtime ⇒ 夹具造一条（与 tests/squads/support.rs 同款）。
    let runtime_id = insert_runtime(pool, workspace_id).await;

    // 第二个 workspace：它的 agent / issue / task 只用来证明「查不到」是**租户收窄**的结果
    // （不是「因为那条行根本不存在」这种弱结论）。
    let foreign_agent = insert_agent(pool, other_workspace).await;
    let foreign_issue = insert_issue(pool, other_workspace, 1, owner).await;
    let _foreign_squad = insert_squad(pool, other_workspace, foreign_agent, owner).await;
    let foreign_runtime_id = insert_runtime(pool, other_workspace).await;
    let foreign_task = insert_task(
        pool,
        foreign_agent,
        foreign_runtime_id,
        Some(foreign_issue),
        true,
        None,
    )
    .await;

    Fixture {
        pool: pool.clone(),
        workspace_id,
        other_workspace,
        users: vec![owner],
        leader_agent,
        worker_agent,
        issue_id,
        other_issue_id,
        squad_id,
        runtime_id,
        foreign_task,
    }
}

/// 清场：squad 先删（`leader_id` 的 RESTRICT 会与 workspace 级联打架）→ workspace → users。
/// `activity_log` / `issue` / `agent_task_queue` 都有 `workspace_id` 级联（或经 `agent` / `issue`
/// 间接级联）⇒ 不需要逐个清。
async fn cleanup(fixture: &Fixture) {
    let pool = &fixture.pool;
    for ws in [fixture.workspace_id, fixture.other_workspace] {
        let _ = sqlx::query("DELETE FROM squad WHERE workspace_id = $1")
            .bind(ws)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(ws)
            .execute(pool)
            .await;
    }
    for user in &fixture.users {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user)
            .execute(pool)
            .await;
    }
}

/// 上游 `HasSquadLeaderNoActionEvaluationForTask`（`activity.sql:35`）的查询体，
/// 谓词与 `089_squad_no_action_activity_index` 的局部索引逐字一致。
async fn suppression_hit(pool: &PgPool, issue: Uuid, agent: Uuid, task: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS ( \
            SELECT 1 FROM activity_log \
             WHERE issue_id = $1 AND actor_type = 'agent' AND actor_id = $2 \
               AND action = 'squad_leader_evaluated' \
               AND details->>'outcome' = 'no_action' \
               AND details->>'task_id' = $3::text)",
    )
    .bind(issue)
    .bind(agent)
    .bind(task.to_string())
    .fetch_one(pool)
    .await
    .expect("抑制查询")
}

// ---------------------------------------------------------------------------
// ① task 查询：租户收窄 + 5 列投影
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 5 列 + 三条租户负例平铺，拆函数就看不出对照关系
async fn leader_task_lookup_is_tenant_scoped_and_projects_five_columns() {
    let (db, pool) = setup().await;
    let fixture = seed(&pool).await;
    let repo = SquadEvaluationRepo::new(db);

    let task_id = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        true,
        Some(fixture.squad_id),
    )
    .await;

    // 正常路径：5 列逐列正确。
    let row = repo
        .leader_task_in_workspace(Id::from(task_id), Id::from(fixture.workspace_id))
        .await
        .expect("查询 task")
        .expect("task 应当查得到");
    assert_eq!(row.id(), Id::from(task_id));
    assert_eq!(row.agent_id(), Id::from(fixture.leader_agent));
    assert_eq!(row.issue_id(), Some(Id::from(fixture.issue_id)));
    assert!(row.is_leader_task());
    assert_eq!(row.squad_id(), Some(Id::from(fixture.squad_id)));

    // 同一条 task 换一个 workspace 查 ⇒ `None`（租户闸门在 `JOIN agent` 上）。
    assert!(
        repo.leader_task_in_workspace(Id::from(task_id), Id::from(fixture.other_workspace))
            .await
            .expect("查询 task")
            .is_none(),
        "跨 workspace 必须查不到"
    );
    // 另一条 task（本来就在第二个 workspace 里）用第一个 workspace 的 id 查 ⇒ 同样 `None`
    // —— 这一发证明上一条不是「因为行不存在」这种弱结论。
    assert!(
        repo.leader_task_in_workspace(
            Id::from(fixture.foreign_task),
            Id::from(fixture.workspace_id)
        )
        .await
        .expect("查询 task")
        .is_none(),
        "别的 workspace 的 task 用本 workspace 的 id 查不到"
    );
    assert!(
        repo.leader_task_in_workspace(
            Id::from(fixture.foreign_task),
            Id::from(fixture.other_workspace)
        )
        .await
        .expect("查询 task")
        .is_some(),
        "它在本 workspace 里查得到（对照）"
    );
    // 不存在的 id ⇒ `None`（不是错误）。
    assert!(repo
        .leader_task_in_workspace(Id::from(Uuid::new_v4()), Id::from(fixture.workspace_id))
        .await
        .expect("查询 task")
        .is_none());

    // chat / quick-create 形态：`issue_id` 为空，行仍查得到（上游在那里回 400，不是 404）。
    let chat_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        None,
        true,
        Some(fixture.squad_id),
    )
    .await;
    let chat_row = repo
        .leader_task_in_workspace(Id::from(chat_task), Id::from(fixture.workspace_id))
        .await
        .expect("查询 task")
        .expect("chat 任务也查得到");
    assert_eq!(chat_row.issue_id(), None);
    assert!(chat_row.is_leader_task());

    // 非 leader 任务 / 没盖 squad 的任务：行照样返回，判据由 handler 读这两列决定。
    let worker_task = insert_task(
        &pool,
        fixture.worker_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        false,
        None,
    )
    .await;
    let worker_row = repo
        .leader_task_in_workspace(Id::from(worker_task), Id::from(fixture.workspace_id))
        .await
        .expect("查询 task")
        .expect("查得到");
    assert!(!worker_row.is_leader_task());
    assert_eq!(worker_row.squad_id(), None);
    assert_eq!(worker_row.agent_id(), Id::from(fixture.worker_agent));

    // 另一条 issue 上的 leader 任务（handler 拿它回带 issue id 的 400）。
    let other_issue_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.other_issue_id),
        true,
        Some(fixture.squad_id),
    )
    .await;
    let other_row = repo
        .leader_task_in_workspace(Id::from(other_issue_task), Id::from(fixture.workspace_id))
        .await
        .expect("查询 task")
        .expect("查得到");
    assert_eq!(other_row.issue_id(), Some(Id::from(fixture.other_issue_id)));

    cleanup(&fixture).await;
}

// ---------------------------------------------------------------------------
// ② 落库：一条 activity_log 行 + `actor_id` 必须是 task 的 agent
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 七列回读 + 四条抑制谓词负例平铺
async fn record_evaluation_writes_activity_row_that_the_no_action_lookup_can_find() {
    let (db, pool) = setup().await;
    let fixture = seed(&pool).await;
    let repo = SquadEvaluationRepo::new(db);

    let task_id = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        true,
        Some(fixture.squad_id),
    )
    .await;
    // 一个**不是** leader 的 agent：用来证明 `actor_id` 列里放的是 task 的 agent
    // 而不是 squad 的 leader（两者在这里刻意不同）。
    assert_ne!(fixture.leader_agent, fixture.worker_agent);

    let details = evaluation_details(
        Id::from(fixture.squad_id),
        Id::from(task_id),
        "no_action",
        "nothing to do",
    );
    let row = repo
        .record_evaluation(
            Id::from(fixture.workspace_id),
            Id::from(fixture.issue_id),
            Id::from(fixture.leader_agent),
            &details,
        )
        .await
        .expect("落库");

    assert_eq!(row.action, ACTION_SQUAD_LEADER_EVALUATED);
    assert_eq!(
        row.id().as_uuid().get_version_num(),
        7,
        "上游显式写 dbid.NewV7()；默认值 gen_random_uuid() 给的是 v4"
    );

    // 七列逐列回读：activity_log 的列名与上游 001_init 一致。
    let (workspace_id, issue_id, actor_type, actor_id, action, stored): (
        Uuid,
        Option<Uuid>,
        Option<String>,
        Option<Uuid>,
        String,
        serde_json::Value,
    ) = sqlx::query_as(
        "SELECT workspace_id, issue_id, actor_type, actor_id, action, details \
         FROM activity_log WHERE id = $1",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .expect("回读 activity_log");

    assert_eq!(workspace_id, fixture.workspace_id);
    assert_eq!(issue_id, Some(fixture.issue_id));
    assert_eq!(actor_type.as_deref(), Some("agent"), "actor_type 恒 agent");
    assert_eq!(
        actor_id,
        Some(fixture.leader_agent),
        "actor_id = task.agent_id（不是 squad.leader_id）"
    );
    assert_eq!(action, ACTION_SQUAD_LEADER_EVALUATED);
    assert_eq!(
        stored,
        json!({
            "squad_id": fixture.squad_id.to_string(),
            "task_id": task_id.to_string(),
            "outcome": "no_action",
            "reason": "nothing to do",
        }),
        "四个字符串键逐字（reason 非空）"
    );

    // 抑制查询：逐字照 `089_squad_no_action_activity_index` 的谓词与
    // `HasSquadLeaderNoActionEvaluationForTask` 的查询体。

    assert!(
        suppression_hit(&pool, fixture.issue_id, fixture.leader_agent, task_id).await,
        "按 (issue, task 的 agent, task_id) 必须命中 —— 这正是 actor_id 不能放 leader 的原因"
    );
    assert!(
        !suppression_hit(&pool, fixture.issue_id, fixture.worker_agent, task_id).await,
        "换一个 agent 不命中（否则抑制会对别的 agent 生效）"
    );
    assert!(
        !suppression_hit(
            &pool,
            fixture.issue_id,
            fixture.leader_agent,
            Uuid::new_v4()
        )
        .await,
        "换一个 task_id 不命中（抑制是按 task 计的）"
    );
    assert!(
        !suppression_hit(&pool, fixture.other_issue_id, fixture.leader_agent, task_id).await,
        "换一个 issue 不命中"
    );

    // 没有去重/幂等：上游每次调用都插一条（`CreateActivity` 无唯一约束）。
    let second = repo
        .record_evaluation(
            Id::from(fixture.workspace_id),
            Id::from(fixture.issue_id),
            Id::from(fixture.leader_agent),
            &evaluation_details(
                Id::from(fixture.squad_id),
                Id::from(task_id),
                "failed",
                "boom",
            ),
        )
        .await
        .expect("第二次落库");
    assert_ne!(second.id, row.id, "两次调用两条独立的行");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM activity_log WHERE issue_id = $1 AND action = 'squad_leader_evaluated'",
    )
    .bind(fixture.issue_id)
    .fetch_one(&pool)
    .await
    .expect("计数");
    assert_eq!(count, 2);

    cleanup(&fixture).await;
}
