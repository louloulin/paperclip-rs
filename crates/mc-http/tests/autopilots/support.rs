//! `/api/autopilots*` 端到端测试脚手架（M5-1 / LUM-1564）。
//!
//! 与 `tests/squads/support.rs` 同手法：真 PG + 真 axum router（`routes::router`），
//! 不 mock 任何一层。`autopilot` / `autopilot_trigger` / `autopilot_subscriber` /
//! `autopilot_collaborator` / `autopilot_run` / `autopilot_quota_period` 都是**上游形状**
//! （`contracts/upstream-schema.sql`），不是本地 `0001` 的旧形状。
//!
//! 运行：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1563:…@127.0.0.1:5432/multica_lum1563 \
//!   cargo test -p mc-http --test autopilots --features test-util -- --ignored
//! ```
//! `MULTICA_TEST_DATABASE_URL` 未设置 → 每个用例打印跳过并 `return`；**设了却连不上 →
//! panic**（库坏了必须红，不许静默假装绿）。

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

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`；**设了却连不上 → panic**。
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

/// 只有用户头（缺 `X-Workspace-ID`）的请求：`resolve_workspace_id` 的 400 路径。
pub(crate) async fn call_no_workspace(app: &Router, uri: &str, user_id: Uuid) -> StatusCode {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .body(Body::empty())
        .unwrap();
    app.clone()
        .oneshot(request)
        .await
        .expect("router call")
        .status()
}

/// 连用户头都没有：`AuthUser` 的 401 路径。
pub(crate) async fn call_no_user(app: &Router, uri: &str) -> StatusCode {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    app.clone()
        .oneshot(request)
        .await
        .expect("router call")
        .status()
}

/// 错误体里的 `message`（本仓统一 `{"error":{code,message}}`；`cron-preview` 例外，是扁平体）。
pub(crate) fn err_message(body: &Value) -> &str {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// workspace + 一个 `role` 角色的成员。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-ap-ws', $1) RETURNING id",
    )
    .bind(format!("itest-ap-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

/// 往已有 workspace 里加一个 `role` 成员。
pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-ap-user', $1) RETURNING id"#,
    )
    .bind(format!("ap-{}@example.com", Uuid::new_v4()))
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

/// 建 `user` 行但**不入 workspace**（非成员 → 读面应 404 而不是 403）。
pub(crate) async fn seed_outsider(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-ap-out', $1) RETURNING id"#,
    )
    .bind(format!("ap-out-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert outsider user")
}

/// 建 autopilot 行。`created_by_type` 传 `member` / `agent`（`autopilotToResponse` 的
/// `assignee_type` 回退与 `write_by_ownership` 的创建者那条腿都靠它区分）。
pub(crate) async fn seed_autopilot(
    pool: &PgPool,
    workspace_id: Uuid,
    status: &str,
    created_by_type: &str,
    created_by_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot \
            (workspace_id, title, assignee_type, assignee_id, status, execution_mode, \
             created_by_type, created_by_id, description, project_id, pause_reason) \
         VALUES ($1, $2, 'agent', $3, $4, 'run_only', $5, $6, NULL, NULL, NULL) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-{}", Uuid::new_v4()))
    // `assignee_id` 在 `096` 之后没有 FK；给个稳定 uuid 即可。
    .bind(Uuid::new_v4())
    .bind(status)
    .bind(created_by_type)
    .bind(created_by_id)
    .fetch_one(pool)
    .await
    .expect("insert autopilot")
}

/// 建 webhook 触发器。`filters_jsonb` 是**原始 JSON 文本**（`None` = `event_filters` 为 NULL），
/// 这样可以造出「解不开的 `event_filters`」用例。
pub(crate) async fn seed_webhook_trigger(
    pool: &PgPool,
    autopilot_id: Uuid,
    token: &str,
    signing_secret: Option<&str>,
    filters_jsonb: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot_trigger \
            (autopilot_id, kind, enabled, webhook_token, provider, signing_secret, event_filters) \
         VALUES ($1, 'webhook', true, $2, 'github', $3, $4::jsonb) RETURNING id",
    )
    .bind(autopilot_id)
    .bind(token)
    .bind(signing_secret)
    .bind(filters_jsonb)
    .fetch_one(pool)
    .await
    .expect("insert webhook trigger")
}

/// 建启用的 schedule 触发器，`next_in` 是 SQL 相对区间（如 `"1 hour"`）。
pub(crate) async fn seed_schedule_trigger(
    pool: &PgPool,
    autopilot_id: Uuid,
    cron: &str,
    next_in: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot_trigger \
            (autopilot_id, kind, enabled, cron_expression, timezone, next_run_at) \
         VALUES ($1, 'schedule', true, $2, 'UTC', now() + $3::interval) RETURNING id",
    )
    .bind(autopilot_id)
    .bind(cron)
    .bind(next_in)
    .fetch_one(pool)
    .await
    .expect("insert schedule trigger")
}

/// 建**停用**的触发器（列表的 `trigger_kinds` 只收 `enabled` 的）。`kind` 取 `api`。
pub(crate) async fn seed_disabled_trigger(pool: &PgPool, autopilot_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot_trigger (autopilot_id, kind, enabled) \
         VALUES ($1, 'api', false) RETURNING id",
    )
    .bind(autopilot_id)
    .fetch_one(pool)
    .await
    .expect("insert disabled trigger")
}

pub(crate) async fn seed_subscriber(pool: &PgPool, autopilot_id: Uuid, user_id: Uuid) {
    sqlx::query(
        "INSERT INTO autopilot_subscriber (autopilot_id, user_type, user_id) \
         VALUES ($1, 'member', $2)",
    )
    .bind(autopilot_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert subscriber");
}

pub(crate) async fn seed_collaborator(
    pool: &PgPool,
    autopilot_id: Uuid,
    user_id: Uuid,
    granted_by: Uuid,
) {
    sqlx::query(
        "INSERT INTO autopilot_collaborator (autopilot_id, user_type, user_id, granted_by) \
         VALUES ($1, 'member', $2, $3)",
    )
    .bind(autopilot_id)
    .bind(user_id)
    .bind(granted_by)
    .execute(pool)
    .await
    .expect("insert collaborator");
}

/// 建一条 `autopilot_run`（列表的 `last_run_status` 取 `triggered_at` 最新的一条）。
pub(crate) async fn seed_run(
    pool: &PgPool,
    autopilot_id: Uuid,
    status: &str,
    triggered_ago: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot_run (autopilot_id, source, status, triggered_at) \
         VALUES ($1, 'manual', $2, now() - $3::interval) RETURNING id",
    )
    .bind(autopilot_id)
    .bind(status)
    .bind(triggered_ago)
    .fetch_one(pool)
    .await
    .expect("insert autopilot_run")
}

/// 建配额周期行（`get_period` 用 `(workspace_id, period_start, period_end)` 定位）。
pub(crate) async fn seed_quota_period(
    pool: &PgPool,
    workspace_id: Uuid,
    period_start: chrono::DateTime<chrono::Utc>,
    period_end: chrono::DateTime<chrono::Utc>,
    used: i64,
    reserved: i64,
    blocked: Value,
) {
    sqlx::query(
        "INSERT INTO autopilot_quota_period \
            (workspace_id, period_start, period_end, used_count, reserved_count, blocked_counts) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(workspace_id)
    .bind(period_start)
    .bind(period_end)
    .bind(used)
    .bind(reserved)
    .bind(blocked)
    .execute(pool)
    .await
    .expect("insert autopilot_quota_period");
}

/// 直接改状态（铺陈 fixture 用；不走上游写面）。
pub(crate) async fn set_status(pool: &PgPool, autopilot_id: Uuid, status: &str) {
    sqlx::query("UPDATE autopilot SET status = $2 WHERE id = $1")
        .bind(autopilot_id)
        .bind(status)
        .execute(pool)
        .await
        .expect("update autopilot.status");
}

/// 清场：autopilot 先删（trigger/subscriber/collaborator/run 都是它的级联子行）。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    let _ = sqlx::query("DELETE FROM autopilot WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM autopilot_quota_period WHERE workspace_id = $1")
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

/// `autopilots` 数组里第一个（也是唯一一个）元素的 `id`。
pub(crate) fn first_id(body: &Value) -> Option<Uuid> {
    body.get("autopilots")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|row| row.get("id"))
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
}
