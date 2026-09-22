//! `/api/issues*` 端到端测试：support 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use std::env;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const WORKSPACE_HEADER: &str = "x-workspace-id";

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

pub(crate) async fn body_json(body: Body) -> serde_json::Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

pub(crate) async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

/// workspace + member 种子（member role 足够：本切片的读路径只要求成员身份）。
pub(crate) async fn seed_workspace(pool: &sqlx::PgPool) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-issue-ws', $1) RETURNING id",
    )
    .bind(format!("itest-issue-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-issue-user', $1) RETURNING id"#,
    )
    .bind(format!("issue-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(workspace_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert member");

    (workspace_id, user_id)
}

pub(crate) async fn cleanup(pool: &sqlx::PgPool, workspace_id: Uuid, user_id: Uuid) {
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(user_id)
        .execute(pool)
        .await;
}

pub(crate) fn req(
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<serde_json::Value>,
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

/// 建一个 issue 并返回其 JSON。
pub(crate) async fn create_issue(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    body: serde_json::Value,
) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues",
            workspace_id,
            user_id,
            Some(body),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    body_json(res.into_body()).await
}
