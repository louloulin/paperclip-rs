//! Instance settings 单例路由契约测试。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    routes,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters: AdapterRegistry::new(),
            actors,
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 3100,
            session_cookie: "paperclip_session".into(),
            api_key_header: "x-paperclip-agent-key".into(),
            csrf_header: "x-paperclip-csrf".into(),
        },
        pc_telemetry::TelemetryOptions::default(),
        Arc::new(WsState::new(realtime.clone(), "test".to_string())),
        realtime,
    )
}

async fn insert_user_with_session(db: &Db) -> (String, String) {
    let user_id = format!("is-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, true, now(), now()) ON CONFLICT (id) DO NOTHING",
    )
    .bind(&user_id)
    .bind(format!("User {user_id}"))
    .bind(format!("{user_id}@example.com"))
    .execute(db.pool())
    .await
    .expect("insert user");
    let token = format!("sess_is_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO session (id, expires_at, token, created_at, updated_at, user_id) \
         VALUES ($1, now() + interval '1 hour', $2, now(), now(), $3) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("sess-is-{user_id}-{}", Uuid::new_v4().simple()))
    .bind(&token)
    .bind(&user_id)
    .execute(db.pool())
    .await
    .expect("insert session");
    (user_id, token)
}

async fn call_with_auth(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: &str,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .uri(path)
                .body(Body::from(payload))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

async fn call_no_auth(app: &axum::Router, method: &str, path: &str) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

#[tokio::test(flavor = "current_thread")]
async fn settings_requires_authentication() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::instance_settings::router().with_state(test_state(db));
    let (status, _) = call_no_auth(&app, "GET", "/api/instance/settings").await;
    assert!(
        status == 401 || status == 403,
        "expected auth challenge, got {status}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn settings_get_returns_default_shape() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let (_user_id, token) = insert_user_with_session(&db).await;
    let app = routes::instance_settings::router().with_state(test_state(db));
    let (status, body) = call_with_auth(&app, "GET", "/api/instance/settings", None, &token).await;
    assert_eq!(status, 200, "settings: {body}");
    // Settings singleton returns InstanceSetting with general/experimental fields
    assert!(body.is_object(), "settings should be object");
}

#[tokio::test(flavor = "current_thread")]
async fn settings_general_patch_then_get_round_trip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let (_user_id, token) = insert_user_with_session(&db).await;
    let app = routes::instance_settings::router().with_state(test_state(db));

    let (status, body) = call_with_auth(
        &app,
        "PATCH",
        "/api/instance/settings/general",
        Some(json!({ "theme": "dark", "locale": "zh-CN" })),
        &token,
    )
    .await;
    assert_eq!(status, 200, "patch general: {body}");
    assert_eq!(body["theme"], "dark");
    assert_eq!(body["locale"], "zh-CN");

    let (status, body) =
        call_with_auth(&app, "GET", "/api/instance/settings/general", None, &token).await;
    assert_eq!(status, 200, "get general: {body}");
    assert_eq!(body["theme"], "dark", "should persist: {body}");
}
