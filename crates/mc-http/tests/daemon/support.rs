//! daemon 面（`/api/daemon/*` + `/api/runtimes/:id` 异步往返）端到端测试脚手架。
//!
//! 与 `tests/runtimes/support.rs` 同手法（同一套种子 + `oneshot` 调用），差别只有两处：
//!
//! 1. 身份用 dev-mode 头：`x-multica-user-id`（必有）+ `x-daemon-id`（`mdt_` token 的
//!    等价物，偏离 D-1）—— 生产路径是 `Authorization: Bearer mdt_/mul_`，那些分支在
//!    `scope.rs` 的单元测试里覆盖；
//! 2. 额外提供 [`spawn_server`]：WS 的握手只能走真 socket（`oneshot` 拿不到 upgraded
//!    连接），所以 WS 用例起一个监听 `127.0.0.1:0` 的真 axum 服务。

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
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
/// dev-mode 的机器标识（`mdt_` token 里 `daemon_id` 的本地等价物，见 `scope.rs`）。
pub(crate) const DAEMON_ID_HEADER: &str = "x-daemon-id";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

pub(crate) fn app_with_db(db: Db) -> Router {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let state = AppState::new(
        db,
        RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
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
    let state = Arc::new(state);
    mc_http::routes::router(state.clone()).with_state(state)
}

/// 起一个真 axum 服务（只给 WS 用例用），返回 `(ws base url, handle)`。
pub(crate) async fn spawn_server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("ws://{addr}"), handle)
}

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（打印跳过）；**设了却连不上 → panic**。
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

/// workspace + 一个 `role` 成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-daemon-ws', $1) RETURNING id",
    )
    .bind(format!("itest-daemon-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-daemon-user', $1) RETURNING id"#,
    )
    .bind(format!("daemon-{}@example.com", Uuid::new_v4()))
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

/// 建一台绑在 `daemon_id` 上、`online` 且有主的 runtime（可被 claim）。
///
/// `visibility` 只能是 `'private'` / `'public'`（`083_runtime_visibility.up.sql`：
/// `CHECK (visibility IN ('private','public'))`，默认 `'private'`）。`'workspace'` 是
/// **另一张表**的词汇（`runtime_profile.visibility`，`120_runtime_profile.up.sql`），
/// 写到 `agent_runtime` 上会被 check 约束拒掉（23514）。
///
/// 这里用上游默认值 `'private'`：本套件的调用者全是这台 runtime 的 `owner_id`，
/// `AgentRuntimeRepo::list(Visible)` 的口径 `owner_id = $1 OR visibility = 'public'`
/// 与 daemon 面 `require_*_access` 的 workspace 门都不受影响；
/// `async_face::local_skill_list_is_readable_but_import_is_owner_only` 这类「非主不可」的
/// 用例更是只有在 `'private'` 下才真的有判别力（它自己再翻成 `'public'`）。
pub(crate) async fn seed_runtime(
    pool: &PgPool,
    workspace_id: Uuid,
    owner: Uuid,
    daemon_id: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, owner_id, visibility, \
             status, last_seen_at) \
         VALUES ($1, $2, $3, 'local', 'claude', $4, 'private', 'online', now()) RETURNING id",
    )
    .bind(workspace_id)
    .bind(daemon_id)
    .bind(format!("rt-{}", Uuid::new_v4()))
    .bind(owner)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 建一个绑在 runtime 上的 agent（claim 要求 `a.runtime_id = atq.runtime_id`）。
pub(crate) async fn seed_agent(pool: &PgPool, workspace_id: Uuid, runtime_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, NULL) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("ag-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// 建一条 `status` 状态的 `agent_task_queue` 行（顺带建占位 issue），返回 task id。
///
/// issue 的 `number` 必须显式分配：`agent_runtime` 之外，`issue` 也带
/// `UNIQUE(workspace_id, number)`（`020_issue_number.up.sql:33`）而列的默认值是 `0`
/// ⇒ 同一 workspace 里建第二条占位 issue 会撞 23505。这里按生产口径推 workspace 的
/// `issue_counter`，而不是自己编一个序号。
pub(crate) async fn seed_task(
    pool: &PgPool,
    workspace_id: Uuid,
    creator: Uuid,
    runtime_id: Uuid,
    agent_id: Uuid,
    status: &str,
) -> Uuid {
    let number: i32 = sqlx::query_scalar(
        "UPDATE workspace SET issue_counter = issue_counter + 1 \
         WHERE id = $1 RETURNING issue_counter",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("bump issue_counter");
    let issue_id: Uuid = sqlx::query_scalar(
        "INSERT INTO issue(workspace_id, title, creator_type, creator_id, number) \
         VALUES ($1, 'itest daemon task', 'member', $2, $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(creator)
    .bind(number)
    .fetch_one(pool)
    .await
    .expect("insert issue");

    sqlx::query_scalar(
        "INSERT INTO agent_task_queue \
            (agent_id, issue_id, runtime_id, status, started_at, completed_at) \
         VALUES ($1, $2, $3, $4, \
                 CASE WHEN $4 IN ('running', 'completed', 'failed', 'cancelled') \
                      THEN now() - interval '1 hour' ELSE NULL END, \
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

/// 一步建齐「runtime + agent + queued task」，返回 `(runtime_id, agent_id, task_id)`。
pub(crate) async fn seed_ready_task(
    pool: &PgPool,
    workspace_id: Uuid,
    owner: Uuid,
    daemon_id: &str,
) -> (Uuid, Uuid, Uuid) {
    let runtime_id = seed_runtime(pool, workspace_id, owner, daemon_id).await;
    let agent_id = seed_agent(pool, workspace_id, runtime_id).await;
    let task_id = seed_task(pool, workspace_id, owner, runtime_id, agent_id, "queued").await;
    (runtime_id, agent_id, task_id)
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

/// daemon 面请求：dev-mode 身份（用户 + 可选机器标识）。
pub(crate) fn daemon_req(
    method: &str,
    uri: &str,
    user_id: Uuid,
    daemon_id: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header("content-type", "application/json");
    if let Some(daemon_id) = daemon_id {
        builder = builder.header(DAEMON_ID_HEADER, daemon_id);
    }
    match body {
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Uuid,
    daemon_id: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(daemon_req(method, uri, user_id, daemon_id, body))
        .await
        .expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// 无 `Authorization` 且无 `x-multica-user-id` ⇒ 401（dev-mode 兜底的拒绝路径）。
pub(crate) async fn call_anon(app: &Router, method: &str, uri: &str) -> StatusCode {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    app.clone()
        .oneshot(request)
        .await
        .expect("router call")
        .status()
}

/// 批量 claim 一条任务（返回 `tasks[0]`，空则 `Value::Null`）。
pub(crate) async fn claim(
    app: &Router,
    user_id: Uuid,
    daemon_id: &str,
    runtime_ids: &[Uuid],
    max_tasks: i64,
) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/api/daemon/tasks/claim",
        user_id,
        Some(daemon_id),
        Some(json!({
            "daemon_id": daemon_id,
            "runtime_ids": runtime_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "max_tasks": max_tasks,
        })),
    )
    .await
}

/// 错误正文里**上游原文**那一段（剥掉 `mc-errors` 的内部前缀）。
pub(crate) fn error_message(body: &Value) -> &str {
    const PREFIXES: [&str; 13] = [
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
        // `mc_errors::Error::RuntimeOffline` 的 Display 前缀（`mc-errors/src/lib.rs:56`）。
        // daemon 面把它的状态码改写成 503，但错误体统一带前缀（见 `docs/32` 偏离表）。
        "runtime not connected: ",
    ];
    let raw = body["error"]["message"].as_str().unwrap_or("");
    for prefix in PREFIXES {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest;
        }
    }
    raw
}
