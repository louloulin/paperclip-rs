//! `/api/agents*` 端到端测试：support 分片（与 `tests/issues/` 同手法，R7 800 行上限）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
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
            invitation_per_workspace_per_hour: Some(50),
            ..Default::default()
        },
        realtime,
        ws,
    );
    Arc::new(state)
}

/// 建 router（与 `tests/issues/` 相同：`router(state).with_state(state)`）。
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

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-agent-ws', $1) RETURNING id",
    )
    .bind(format!("itest-agent-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

/// 往已有 workspace 里加一个 `role` 成员。
pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-agent-user', $1) RETURNING id"#,
    )
    .bind(format!("agent-{}@example.com", Uuid::new_v4()))
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

/// 建 runtime；`owner` 为空表示无主（`canUseRuntimeForAgent` 会拒）。
pub(crate) async fn seed_runtime(
    pool: &PgPool,
    workspace_id: Uuid,
    owner: Option<Uuid>,
    visibility: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, name, runtime_mode, provider, owner_id, visibility, status) \
         VALUES ($1, $2, 'local', 'claude', $3, $4, 'online') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("rt-{}", Uuid::new_v4()))
    .bind(owner)
    .bind(visibility)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 建 agent 标签（`resource_type` 可传 `'issue'` 以覆盖 404 分支）。
pub(crate) async fn seed_label(pool: &PgPool, workspace_id: Uuid, resource_type: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO issue_label (workspace_id, name, color, resource_type) \
         VALUES ($1, $2, '#123456', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("label-{}", Uuid::new_v4()))
    .bind(resource_type)
    .fetch_one(pool)
    .await
    .expect("insert issue_label")
}

/// 建一条 `agent_task_queue` 行（上游 `251_agent_runtime_unbind` 要求：
/// 没有 `completed_at` 就必须有 `runtime_id`，所以 runtime 一律给）。
pub(crate) async fn seed_task(
    pool: &PgPool,
    runtime_id: Uuid,
    agent_id: Uuid,
    status: &str,
    completed_ago: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_task_queue (agent_id, runtime_id, status, completed_at) \
         VALUES ($1, $2, $3, \
                 CASE WHEN $4::text IS NULL THEN NULL ELSE now() - $4::interval END) \
         RETURNING id",
    )
    .bind(agent_id)
    .bind(runtime_id)
    .bind(status)
    .bind(completed_ago)
    .fetch_one(pool)
    .await
    .expect("insert agent_task_queue")
}

pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
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

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

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

/// 建一个 agent（走真实 HTTP，期望 201），返回响应 JSON。
pub(crate) async fn create_agent(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Value,
) -> Value {
    let (status, json) = call(
        app,
        "POST",
        "/api/agents/",
        workspace_id,
        user_id,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create agent failed: {json}");
    json
}

/// 建 agent 的最小请求体。
pub(crate) fn new_agent_body(name: &str, runtime_id: Uuid) -> Value {
    json!({ "name": name, "runtime_id": runtime_id.to_string() })
}

pub(crate) fn id_of(value: &Value) -> Uuid {
    Uuid::parse_str(value["id"].as_str().expect("id")).expect("uuid")
}

/// 断言错误响应体（`{error:{code,message}}`）的 code。
pub(crate) fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

/// 错误正文里**上游原文**那一段。
///
/// `mc-errors::Error` 的 `Display` 会在正文前加一个内部前缀（`thiserror` 的
/// `#[error("validation error: {message}")]` 等），而线上 body 的 message 就带这个前缀。
/// 断言上游文案时剥掉它，避免测试把内部前缀写死。
pub(crate) fn error_message(body: &Value) -> &str {
    const PREFIXES: [&str; 12] = [
        "validation error: ",
        "not found: ",
        "conflict: ",
        "unprocessable entity: ",
        "forbidden: ",
        "unauthorized: ",
        "workspace not found: ",
        "workspace archived: ",
        "database error: ",
        "internal error: ",
        "io error: ",
        "upstream error: ",
    ];
    let raw = body["error"]["message"].as_str().unwrap_or("");
    for prefix in PREFIXES {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest;
        }
    }
    raw
}
