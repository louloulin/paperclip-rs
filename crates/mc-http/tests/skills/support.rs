//! `/api/skills*` 端到端测试：support 分片（与 `tests/agents/` 同手法，R7 800 行上限）。
//!
//! 与 `tests/agents/support.rs` 是**两份**文件而不是一份共享：e2e 目录各自
//! `mod support;`（`tests/<dir>/main.rs` 是独立 crate），跨目录共享要走 `#[path]`
//! 或公共 dev-dep —— 两者都比复制这 200 行更难维护。

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

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（用例打印跳过并 return）；
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
        "INSERT INTO workspace(name, slug) VALUES ('itest-skill-ws', $1) RETURNING id",
    )
    .bind(format!("itest-skill-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-skill-user', $1) RETURNING id"#,
    )
    .bind(format!("skill-{}@example.com", Uuid::new_v4()))
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

/// 建标签目录行（`resource_type` 可传 `'agent'` 以覆盖 404 分支）。
pub(crate) async fn seed_label(pool: &PgPool, workspace_id: Uuid, resource_type: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO issue_label (workspace_id, name, color, resource_type) \
         VALUES ($1, $2, '#123456', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("skill-label-{}", Uuid::new_v4()))
    .bind(resource_type)
    .fetch_one(pool)
    .await
    .expect("insert issue_label")
}

/// 清场：workspace 级联删 `skill` / `skill_file`；`skill_to_label` 迁移 173 起**没有外键**
/// （上游靠应用事务清理），所以 orphan 行要显式删，否则每跑一轮都会攒垃圾。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    let _ = sqlx::query(
        "DELETE FROM skill_to_label WHERE skill_id IN (SELECT id FROM skill WHERE workspace_id = $1)",
    )
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
    body: Option<&Value>,
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

/// 发一次请求，返回 `(status, json)`。`body = Some(Value::Null)` 会真的发 `null`
/// （空 body 与 `null` 在上游是**两条**不同分支，测试要能区分）。
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
        .oneshot(req(method, uri, workspace_id, user_id, body.as_ref()))
        .await
        .expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// 建 skill（走真实 HTTP，期望 201），返回响应 JSON。
pub(crate) async fn create_skill(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Value,
) -> Value {
    let (status, json) = call(
        app,
        "POST",
        "/api/skills/",
        workspace_id,
        user_id,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create skill failed: {json}");
    json
}

pub(crate) fn id_of(value: &Value) -> Uuid {
    Uuid::parse_str(value["id"].as_str().expect("id")).expect("uuid")
}

pub(crate) fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

/// 错误正文里**上游原文**那一段（`mc-errors::Error` 的 `Display` 会加内部前缀）。
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

/// 直接数 `skill_to_label` 的行数（验证显式清理，而不是靠外键级联 —— 它没有外键）。
pub(crate) async fn linked_label_count(pool: &PgPool, skill_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM skill_to_label WHERE skill_id = $1")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .expect("count skill_to_label")
}

pub(crate) fn files_of(skill: &Value) -> Vec<(String, String)> {
    skill["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap_or("").to_string(),
                f["content"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}
