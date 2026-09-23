//! `crate::task` 的 PG 集成测试（M3-6 / LUM-1429）。
//!
//! 运行：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_w3b \
//!   cargo test -p mc-repos --lib -- --ignored task::
//! ```
//! 没有该 env 时静默 skip（与 `crate::issue_table::tests` 一致）。
//!
//! 这里的断言全部打在**真库**上：部分唯一索引、`FOR UPDATE` 分支、三路
//! `CASE` 重算、`ON CONFLICT` 合并 —— 都是「内存版看着也对」的语义，
//! 只有真实 PostgreSQL 能给出结论。

use std::env;

use serde_json::json;
use uuid::Uuid;

use mc_core::{Id, Timestamp};
use mc_db::Db;
use mc_task::state::ColumnWrite;

use super::store::NewTask;
use super::TaskRepo;

mod builder;
mod lifecycle;
mod queries;

struct Fixture {
    db: Db,
    workspace_id: Id,
    user_id: Id,
    runtime_id: Id,
    agent_id: Id,
    issue_id: Id,
}

async fn setup() -> Option<Fixture> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1).await.ok()?;
    let pool = db.pool();
    let suffix = Uuid::new_v4();
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m3b', $1) RETURNING id",
    )
    .bind(format!("itest-m3b-{suffix}"))
    .fetch_one(pool)
    .await
    .ok()?;
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m3b', $1) RETURNING id"#,
    )
    .bind(format!("itest-m3b-{suffix}@example.com"))
    .fetch_one(pool)
    .await
    .ok()?;
    let runtime_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, status, \
             last_seen_at) VALUES ($1, 'itest-rt', 'local', 'claude', 'online', now()) \
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .ok()?;
    let agent_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, runtime_id, kind) \
         VALUES ($1, 'itest-agent', 'local', $2, 'user') RETURNING id",
    )
    .bind(workspace_id)
    .bind(runtime_id)
    .fetch_one(pool)
    .await
    .ok()?;
    let issue_id: Uuid = sqlx::query_scalar(
        "INSERT INTO issue (workspace_id, title, creator_type, creator_id) \
         VALUES ($1, 'itest-issue', 'member', $2) RETURNING id",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .ok()?;
    Some(Fixture {
        db,
        workspace_id: Id::from(workspace_id),
        user_id: Id::from(user_id),
        runtime_id: Id::from(runtime_id),
        agent_id: Id::from(agent_id),
        issue_id: Id::from(issue_id),
    })
}

async fn teardown(fx: &Fixture) {
    let pool = fx.db.pool();
    // `agent_builder_draft` 没有 FK（上游就没有），级联删不掉，必须显式清。
    let _ = sqlx::query("DELETE FROM agent_builder_draft WHERE workspace_id = $1")
        .bind(fx.workspace_id.0)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issue_source_context WHERE workspace_id = $1")
        .bind(fx.workspace_id.0)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fx.workspace_id.0)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(fx.user_id.0)
        .execute(pool)
        .await;
}

fn repo(fx: &Fixture) -> TaskRepo {
    TaskRepo::with_pool(fx.db.pool().clone())
}

/// 一条绑定到 fixture 的 issue + agent + runtime 的 `queued` 任务。
///
/// `thread` 不是直接写 `comment_thread_id`（那是触发器推导的派生列），而是
/// 走上游 516 的 `context->>'wakeup_id'` 通道 —— 这正是唯一索引键的来源。
fn issue_task(fx: &Fixture, thread: Option<Id>) -> NewTask {
    let mut new = NewTask::queued(Id::from(Uuid::now_v7()), fx.agent_id);
    new.issue_id = Some(fx.issue_id);
    new.runtime_id = Some(fx.runtime_id);
    new.context = thread.map(|id| json!({ "wakeup_id": id.0.to_string() }));
    new
}

/// 直接把行改成终态（绕过状态机，只用于铺陈 fixture）。
async fn force_terminal(fx: &Fixture, id: Id, status: &str, started: bool, completed: bool) {
    sqlx::query(
        "UPDATE agent_task_queue SET status = $2, \
             started_at = CASE WHEN $3 THEN now() ELSE NULL END, \
             completed_at = CASE WHEN $4 THEN now() ELSE NULL END WHERE id = $1",
    )
    .bind(id.0)
    .bind(status)
    .bind(started)
    .bind(completed)
    .execute(fx.db.pool())
    .await
    .expect("force_terminal");
}

/// 关闭一个任务从 `dispatched` 走到终态所需的两个转移。
fn running_writes() -> Vec<ColumnWrite> {
    vec![ColumnWrite::StartedAt(Timestamp::now())]
}

// ---------------------------------------------------------------------------
// 1. 入队 → 认领 → 完成
// ---------------------------------------------------------------------------
