//! `squad-evaluated` 端到端测试的脚手架（应用状态 / 夹具 / 请求）。
//!
//! 与 `tests/squads/support.rs` 同手法：`MULTICA_TEST_DATABASE_URL` 缺失 → `None`
//! （打印跳过并 return）；**设了却连不上 → panic** —— 库坏了必须红，不能静默跳过假装绿。

#![allow(dead_code)] // 三个用例模块各取所需，未用到的种子 / 断言小工具不算问题

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const WORKSPACE_HEADER: &str = "x-workspace-id";
pub(crate) const TASK_ID_HEADER: &str = "x-task-id";
pub(crate) const AGENT_ID_HEADER: &str = "x-agent-id";

/// 上游 400 文案（本仓信封里带 `validation error: ` 前缀，见 `docs/22` §7 D1）。
pub(crate) const OUTCOME_ERROR: &str = "outcome must be 'action', 'no_action', or 'failed'";
/// 闸门 1 / 闸门 2 共用的 403 文案（上游两句一字不差）。
pub(crate) const ONLY_LEADER_ERROR: &str = "only the squad leader agent can record evaluations";
/// 「任务不属于这个 issue」的 400 前缀。
pub(crate) const TASK_NOT_BELONG: &str = "task does not belong to issue";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

fn build_state_with_db(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    let state = AppState::new(
        db,
        RuntimeHandles { actors, adapters },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            invitation_per_workspace_per_hour: Some(50),
            ..Default::default()
        },
        realtime,
        ws,
    );
    Arc::new(state)
}

pub(crate) fn app_with_db(db: Db) -> Router {
    let state = build_state_with_db(db);
    mc_http::routes::router(state.clone()).with_state(state)
}

pub(crate) async fn connect() -> Option<(PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// 请求构造 / 发送
// ---------------------------------------------------------------------------

/// 发一次 POST：`extra` 是除身份 / workspace / content-type 之外的头（如 `X-Task-ID`）。
///
/// `body = None` 发的是**坏 JSON**（`{`）—— 上游 `json.Decoder` 失败那一档要的就是它。
pub(crate) async fn send(
    app: &Router,
    ws: Uuid,
    user: Uuid,
    extra: &[(&str, &str)],
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header(USER_ID_HEADER, user.to_string())
        .header(WORKSPACE_HEADER, ws.to_string())
        .header("content-type", "application/json");
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let request = match body {
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::from("{".to_string())).unwrap(),
    };
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// 错误体里本仓信封的 `message`（含 thiserror 前缀）。
pub(crate) fn err_message(body: &Value) -> String {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// workspace + 两个 agent + 两条 issue + 两个 squad 的夹具。
pub(crate) struct Fixture {
    pub(crate) pool: PgPool,
    pub(crate) workspace_id: Uuid,
    pub(crate) other_workspace: Uuid,
    pub(crate) owner: Uuid,
    pub(crate) member: Uuid,
    pub(crate) outsider: Uuid,
    /// 真 leader：`squad_id` 的 `leader_id`，也是正常路径里 task 的 `agent_id`。
    pub(crate) leader_agent: Uuid,
    /// 另一个 agent：既是「不是这条任务的 agent」的越权方，也是 `swapped_squad_id` 的次任 leader。
    pub(crate) other_agent: Uuid,
    pub(crate) issue_id: Uuid,
    pub(crate) other_issue_id: Uuid,
    pub(crate) squad_id: Uuid,
    /// 本 workspace 里 leader 已经换成 `other_agent` 的 squad（闸门 2 的负例）。
    pub(crate) swapped_squad_id: Uuid,
    /// 另一个 workspace 的 issue id：**绝不允许**出现在任何响应体里。
    pub(crate) foreign_issue_id: Uuid,
    pub(crate) foreign_agent: Uuid,
    /// 本 workspace 的 runtime（`agent_task_queue.runtime_id` 的 CHECK 需要它）。
    pub(crate) runtime_id: Uuid,
    /// 第二个 workspace 的 runtime。
    pub(crate) foreign_runtime_id: Uuid,
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
    sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-lum1793-user', $1) RETURNING id"#,
    )
    .bind(format!("lum1793-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user")
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
/// 必须有：`251_agent_runtime_unbind` 的 CHECK `agent_task_queue_active_requires_runtime`
/// （`runtime_id IS NOT NULL OR completed_at IS NOT NULL`）不允许一条「还在跑又没有 runtime」
/// 的任务 —— 真实库里也不存在那种行（与 `tests/squads/support.rs` 的 `seed_runtime` 同款）。
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

/// 建一条 `agent_task_queue` 行（`issue_id` 可为空 = chat / quick-create 形态）。
pub(crate) async fn insert_task(
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

pub(crate) async fn seed(pool: &PgPool) -> Fixture {
    let workspace_id = insert_workspace(pool).await;
    let other_workspace = insert_workspace(pool).await;

    let owner = insert_user(pool).await;
    let member = insert_user(pool).await;
    let outsider = insert_user(pool).await;
    add_member(pool, workspace_id, owner, "owner").await;
    add_member(pool, workspace_id, member, "member").await;
    // `outsider` **故意**不是本 workspace 的成员；但 owner 是第二个 workspace 的成员
    // ⇒ 那里的 404 是**租户隔离**的结果，不是「因为不是成员所以看不见」这种弱结论。
    add_member(pool, other_workspace, owner, "owner").await;

    let leader_agent = insert_agent(pool, workspace_id).await;
    let other_agent = insert_agent(pool, workspace_id).await;
    let issue_id = insert_issue(pool, workspace_id, 1, owner).await;
    let other_issue_id = insert_issue(pool, workspace_id, 2, owner).await;
    let squad_id = insert_squad(pool, workspace_id, leader_agent, owner).await;
    let swapped_squad_id = insert_squad(pool, workspace_id, other_agent, owner).await;

    let runtime_id = insert_runtime(pool, workspace_id).await;

    let foreign_agent = insert_agent(pool, other_workspace).await;
    let foreign_issue_id = insert_issue(pool, other_workspace, 1, owner).await;
    let foreign_runtime_id = insert_runtime(pool, other_workspace).await;

    Fixture {
        pool: pool.clone(),
        workspace_id,
        other_workspace,
        owner,
        member,
        outsider,
        leader_agent,
        other_agent,
        issue_id,
        other_issue_id,
        squad_id,
        swapped_squad_id,
        foreign_issue_id,
        foreign_agent,
        runtime_id,
        foreign_runtime_id,
    }
}

/// 清场：squad 先删（`leader_id` 的 RESTRICT 与 workspace 级联打架）→ workspace → users。
pub(crate) async fn cleanup(fixture: &Fixture) {
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
    for user in [fixture.owner, fixture.member, fixture.outsider] {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user)
            .execute(pool)
            .await;
    }
}

/// `POST /api/issues/:id/squad-evaluated` 的路径。
pub(crate) fn path(issue_id: Uuid) -> String {
    format!("/api/issues/{issue_id}/squad-evaluated")
}

/// 已写下的判决行数（按 issue 计）。
pub(crate) async fn written_rows(pool: &PgPool, issue_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM activity_log \
         WHERE issue_id = $1 AND action = 'squad_leader_evaluated'",
    )
    .bind(issue_id)
    .fetch_one(pool)
    .await
    .expect("计数")
}
