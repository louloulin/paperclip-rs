//! `/api/runtimes*` + `/api/workspaces/:id/runtime-profiles*` 端到端测试脚手架
//! （与 `tests/agents/support.rs` 同手法，R7 800 行上限）。

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

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-runtime-ws', $1) RETURNING id",
    )
    .bind(format!("itest-runtime-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

/// 往已有 workspace 里加一个 `role` 成员。
pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-runtime-user', $1) RETURNING id"#,
    )
    .bind(format!("runtime-{}@example.com", Uuid::new_v4()))
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

/// runtime 行的可选字段（参数太多会让 clippy 的 `too_many_arguments` 红）。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RuntimeSeed<'a> {
    pub(crate) daemon_id: Option<&'a str>,
    pub(crate) provider: &'a str,
    pub(crate) custom_name: Option<&'a str>,
    pub(crate) profile_id: Option<Uuid>,
    pub(crate) status: &'a str,
}

impl RuntimeSeed<'_> {
    fn provider(&self) -> &str {
        if self.provider.is_empty() {
            "claude"
        } else {
            self.provider
        }
    }

    fn status(&self) -> &str {
        if self.status.is_empty() {
            "online"
        } else {
            self.status
        }
    }
}

/// 建 runtime 行。`owner` 为空 = 无主（只有 admin 编辑得了）。
pub(crate) async fn seed_runtime(
    pool: &PgPool,
    workspace_id: Uuid,
    owner: Option<Uuid>,
    visibility: &str,
    seed: RuntimeSeed<'_>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, owner_id, visibility, \
             status, custom_name, profile_id) \
         VALUES ($1, $2, $3, 'local', $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(workspace_id)
    .bind(seed.daemon_id)
    .bind(format!("rt-{}", Uuid::new_v4()))
    .bind(seed.provider())
    .bind(owner)
    .bind(visibility)
    .bind(seed.status())
    .bind(seed.custom_name)
    .bind(seed.profile_id)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 建一行自定义 runtime profile（`runtime_type` 与 `protocol_family` 都给）。
pub(crate) async fn seed_profile(
    pool: &PgPool,
    workspace_id: Uuid,
    display_name: &str,
    protocol_family: &str,
    runtime_type: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO runtime_profile \
            (workspace_id, display_name, protocol_family, command_name, fixed_args, visibility, \
             enabled, runtime_type) \
         VALUES ($1, $2, $3, 'codex', '[]', 'workspace', true, $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(display_name)
    .bind(protocol_family)
    .bind(runtime_type)
    .fetch_one(pool)
    .await
    .expect("insert runtime_profile")
}

/// 建一个绑在 runtime 上的 agent 行（默认 `kind='user'`、未归档）。
///
/// `system_key` 决定 409 文案里的 `blocker_class`；`kind='user'` 是刻意的 ——
/// 上游的 Mika 就是「product-owned 但 kind=user」，用来验证分类不靠 `kind`。
pub(crate) async fn seed_agent(
    pool: &PgPool,
    workspace_id: Uuid,
    runtime_id: Uuid,
    name: &str,
    system_key: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, runtime_id, system_key, owner_id) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, NULL) RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(runtime_id)
    .bind(system_key)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// 建一条 `agent_task_queue` 行（`251_agent_runtime_unbind` 之后：没有 `completed_at`
/// 就必须有 `runtime_id`）。`issue_id` 是 NOT NULL，所以顺带建一个占位 issue。
pub(crate) async fn seed_task(
    pool: &PgPool,
    workspace_id: Uuid,
    creator: Uuid,
    runtime_id: Uuid,
    agent_id: Uuid,
    status: &str,
) -> Uuid {
    let issue_id: Uuid = sqlx::query_scalar(
        "INSERT INTO issue(workspace_id, title, creator_type, creator_id) \
         VALUES ($1, 'itest runtime task', 'member', $2) RETURNING id",
    )
    .bind(workspace_id)
    .bind(creator)
    .fetch_one(pool)
    .await
    .expect("insert issue");

    sqlx::query_scalar(
        "INSERT INTO agent_task_queue (agent_id, issue_id, runtime_id, status, started_at, completed_at) \
         VALUES ($1, $2, $3, $4, now() - interval '1 hour', \
                 CASE WHEN $4 IN ('completed', 'failed', 'cancelled') THEN now() ELSE NULL END) \
         RETURNING id",
    )
    .bind(agent_id)
    .bind(issue_id)
    .bind(runtime_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("insert agent_task_queue")
}

pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    // `runtime_profile` 没有 workspace 外键（迁移 120 的应用层策略），手动清。
    let _ = sqlx::query("DELETE FROM runtime_profile WHERE workspace_id = $1")
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

/// 不带 `x-multica-user-id` 的请求（401 路径）。
pub(crate) fn anon_req(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
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

/// 发一次请求，只要状态码（DELETE / 404 空体等场景）。
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

/// 只有用户头、**没有** workspace 头的请求（`GET /api/runtimes/` 的 400 路径）。
pub(crate) fn user_only_req(method: &str, uri: &str, user_id: Uuid) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .body(Body::empty())
        .unwrap()
}

/// 空 body 请求（上游 `json.Decode` 对空体也是 400，与 `null` body 不同）。
pub(crate) fn empty_body_req(
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap()
}

/// 发一次手搓请求（需要绕开 `call` 的固定 header 时用）。
pub(crate) async fn call_raw(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// 发一次匿名请求，返回状态码（缺 `x-multica-user-id` → 401）。
pub(crate) async fn call_anon(app: &Router, method: &str, uri: &str) -> StatusCode {
    let res = app
        .clone()
        .oneshot(anon_req(method, uri))
        .await
        .expect("router call");
    res.status()
}

/// 从列表体里挑出 `id` 字符串集合，便于断言可见范围。
pub(crate) fn ids_of(values: &Value) -> Vec<String> {
    values
        .as_array()
        .expect("array body")
        .iter()
        .map(|row| row["id"].as_str().expect("id").to_string())
        .collect()
}

/// profile 路径（workspace 来自 URL 段，不带 header 也成立 —— 这里仍带上，方便复用 `call`）。
pub(crate) fn profile_path(workspace_id: Uuid, suffix: &str) -> String {
    format!("/api/workspaces/{workspace_id}/runtime-profiles{suffix}")
}

pub(crate) fn profile_body(display_name: &str, runtime_type: &str) -> Value {
    json!({
        "display_name": display_name,
        "runtime_type": runtime_type,
        "command_name": "my-agent",
        "fixed_args": ["--mode", "json"],
    })
}

/// 断言错误响应体（`{error:{code,message}}`）的 code。
pub(crate) fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

/// 扁平 409 体（`{error, code, ...}`）的 code。
pub(crate) fn flat_code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or("")
}

/// 错误正文里**上游原文**那一段（剥掉 `mc-errors` 的内部前缀，见 `tests/agents/support.rs`）。
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
