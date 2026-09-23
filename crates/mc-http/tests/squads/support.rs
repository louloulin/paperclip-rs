//! `/api/squads*` 端到端测试脚手架（与 `tests/runtimes/support.rs` 同手法，
//! R7 800 行上限）。
//!
//! 说明：squad 面现在有完整的 axum 集成测试（10 条路由 + 4 条尾斜杠别名），
//! 而 M4-0b 的 golden fixture 还没产出 —— 按 `docs/42` §6.4 的口径，本切片用这
//! 组测试代替 fixture 做自证，并在交付评论里记账。

#![allow(dead_code)] // 各测试文件各取所需，未用到的种子不算问题

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
use std::env;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const WORKSPACE_HEADER: &str = "x-workspace-id";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

pub(crate) fn build_state_with_db(db: Db) -> Arc<AppState> {
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

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（测试打印跳过并 return）；
/// **设了却连不上 → panic**：库坏了必须红，不能静默跳过假装绿。
pub(crate) async fn connect() -> Option<(PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

pub(crate) async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// 发一次请求，返回 `(status, json)`。
pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(req(method, uri, workspace_id, user_id, body))
        .await
        .expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

pub(crate) async fn call_status(
    app: &Router,
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> StatusCode {
    call(app, method, uri, workspace_id, user_id, body).await.0
}

pub(crate) fn req(
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header("content-type", "application/json");
    match body {
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// 只有用户头、没有 workspace 头（`resolve_workspace_id` 的 400 路径）。
pub(crate) fn user_only_req(method: &str, uri: &str, user_id: Uuid) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .body(Body::empty())
        .unwrap()
}

/// 发一次「只有用户头」的请求（缺 workspace 头）。
pub(crate) async fn call_no_workspace(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Uuid,
) -> StatusCode {
    app.clone()
        .oneshot(user_only_req(method, uri, user_id))
        .await
        .expect("router call")
        .status()
}

/// 错误体里的 `message`（本仓统一 `{"error":{code,message}}`）。
pub(crate) fn err_message(body: &Value) -> &str {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-squad-ws', $1) RETURNING id",
    )
    .bind(format!("itest-squad-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

/// 往已有 workspace 里加一个 `role` 成员。
pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-squad-user', $1) RETURNING id"#,
    )
    .bind(format!("squad-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(workspace_id)
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .expect("insert member");

    user_id
}

/// 冻结 `workspace.issue_prefix`（成员状态里的 `identifier` 用它；空串时回退 slug 派生）。
pub(crate) async fn set_issue_prefix(pool: &PgPool, workspace_id: Uuid, prefix: &str) {
    sqlx::query("UPDATE workspace SET issue_prefix = $2 WHERE id = $1")
        .bind(workspace_id)
        .bind(prefix)
        .execute(pool)
        .await
        .expect("update workspace.issue_prefix");
}

/// 建 runtime 行。`last_seen` 传 SQL 相对区间（如 `"-2 minutes"`），`None` = NULL。
pub(crate) async fn seed_runtime(
    pool: &PgPool,
    workspace_id: Uuid,
    status: &str,
    last_seen: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, status, last_seen_at) \
         VALUES ($1, $2, $3, 'local', 'claude', $4, \
                 CASE WHEN $5::text IS NULL THEN NULL ELSE now() + $5::interval END) \
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .bind(status)
    .bind(last_seen)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 建 agent 行（`kind='user'`，与上游 squad 面的 `GetAgentInWorkspace` 一致：不过滤 kind）。
pub(crate) async fn seed_agent(
    pool: &PgPool,
    workspace_id: Uuid,
    runtime_id: Option<Uuid>,
    owner_id: Option<Uuid>,
    permission_mode: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, $5) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(owner_id)
    .bind(permission_mode)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

pub(crate) async fn archive_agent(pool: &PgPool, agent_id: Uuid) {
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .expect("archive agent");
}

/// `agent_invocation_target` 的 workspace 目标行（`public_to` + workspace ⇒ 全员可 @）。
pub(crate) async fn allow_workspace(pool: &PgPool, agent_id: Uuid, workspace_id: Uuid) {
    sqlx::query(
        "INSERT INTO agent_invocation_target(agent_id, target_type, target_id) \
         VALUES ($1, 'workspace', $2)",
    )
    .bind(agent_id)
    .bind(workspace_id)
    .execute(pool)
    .await
    .expect("insert agent_invocation_target (workspace)");
}

/// 只给某个成员放行的白名单行。
pub(crate) async fn allow_member(pool: &PgPool, agent_id: Uuid, user_id: Uuid) {
    sqlx::query(
        "INSERT INTO agent_invocation_target(agent_id, target_type, target_id) \
         VALUES ($1, 'member', $2)",
    )
    .bind(agent_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert agent_invocation_target (member)");
}

/// 建 squad 行（`087` 之后重名合法）。
pub(crate) async fn seed_squad(
    pool: &PgPool,
    workspace_id: Uuid,
    name: &str,
    leader_id: Uuid,
    creator_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO squad(workspace_id, name, description, leader_id, creator_id) \
         VALUES ($1, $2, '', $3, $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(leader_id)
    .bind(creator_id)
    .fetch_one(pool)
    .await
    .expect("insert squad")
}

pub(crate) async fn add_squad_member(
    pool: &PgPool,
    squad_id: Uuid,
    member_type: &str,
    member_id: Uuid,
    role: &str,
) {
    sqlx::query(
        "INSERT INTO squad_member(squad_id, member_type, member_id, role) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(squad_id)
    .bind(member_type)
    .bind(member_id)
    .bind(role)
    .execute(pool)
    .await
    .expect("insert squad_member");
}

/// 建一条 issue，返回 `(issue_id, number)`（`number` 由调用方给，用于 identifier 断言）。
pub(crate) async fn seed_issue(
    pool: &PgPool,
    workspace_id: Uuid,
    number: i32,
    title: &str,
    status: &str,
    creator_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO issue(workspace_id, title, number, status, creator_type, creator_id) \
         VALUES ($1, $2, $3, $4, 'member', $5) RETURNING id",
    )
    .bind(workspace_id)
    .bind(title)
    .bind(number)
    .bind(status)
    .bind(creator_id)
    .fetch_one(pool)
    .await
    .expect("insert issue")
}

/// 把 issue 挂到 squad 名下（`084` 扩展了 `issue_assignee_type_check` 支持 `squad`）。
pub(crate) async fn assign_issue_to_squad(pool: &PgPool, issue_id: Uuid, squad_id: Uuid) {
    sqlx::query("UPDATE issue SET assignee_type = 'squad', assignee_id = $2 WHERE id = $1")
        .bind(issue_id)
        .bind(squad_id)
        .execute(pool)
        .await
        .expect("assign issue to squad");
}

/// 建 autopilot 行（`096` 删掉了 `assignee_id → agent` 的 FK 并加了 `assignee_type`）。
pub(crate) async fn seed_autopilot(
    pool: &PgPool,
    workspace_id: Uuid,
    assignee_type: &str,
    assignee_id: Uuid,
    created_by: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot \
            (workspace_id, title, assignee_type, assignee_id, status, execution_mode, \
             created_by_type, created_by_id) \
         VALUES ($1, 'itest-squad-ap', $2, $3, 'active', 'run_only', 'member', $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(assignee_type)
    .bind(assignee_id)
    .bind(created_by)
    .fetch_one(pool)
    .await
    .expect("insert autopilot")
}

/// 建一条 `agent_task_queue` 行。`dispatched` 传 SQL 区间（`None` = 不设 `dispatched_at`）。
pub(crate) async fn seed_task(
    pool: &PgPool,
    agent_id: Uuid,
    issue_id: Uuid,
    runtime_id: Uuid,
    status: &str,
    dispatched: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_task_queue (agent_id, issue_id, runtime_id, status, dispatched_at) \
         VALUES ($1, $2, $3, $4, \
                 CASE WHEN $5::text IS NULL THEN NULL ELSE now() + $5::interval END) \
         RETURNING id",
    )
    .bind(agent_id)
    .bind(issue_id)
    .bind(runtime_id)
    .bind(status)
    .bind(dispatched)
    .fetch_one(pool)
    .await
    .expect("insert agent_task_queue")
}

pub(crate) async fn issue_assignee(
    pool: &PgPool,
    issue_id: Uuid,
) -> (Option<String>, Option<Uuid>) {
    sqlx::query_as("SELECT assignee_type, assignee_id FROM issue WHERE id = $1")
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("select issue assignee")
}

pub(crate) async fn autopilot_assignee(
    pool: &PgPool,
    autopilot_id: Uuid,
) -> (String, Uuid, String) {
    sqlx::query_as("SELECT assignee_type, assignee_id, status FROM autopilot WHERE id = $1")
        .bind(autopilot_id)
        .fetch_one(pool)
        .await
        .expect("select autopilot assignee")
}

pub(crate) async fn squad_archived(pool: &PgPool, squad_id: Uuid) -> (bool, Option<Uuid>) {
    sqlx::query_as("SELECT archived_at IS NOT NULL, archived_by FROM squad WHERE id = $1")
        .bind(squad_id)
        .fetch_one(pool)
        .await
        .expect("select squad archive state")
}

/// 清场：squad（先，避免 `leader_id` 的 RESTRICT 与 workspace 级联打架）→ workspace → users。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    let _ = sqlx::query("DELETE FROM autopilot WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM squad WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for user_id in user_ids {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(pool)
            .await;
    }
}

/// 便捷断言：`GET /api/squads/:id/` 的 200 响应里取字段。
pub(crate) fn member_preview_len(squad: &Value) -> usize {
    squad
        .get("member_preview")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

pub(crate) fn member_count(squad: &Value) -> u64 {
    squad
        .get("member_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
