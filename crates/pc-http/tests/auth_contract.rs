//! Auth routes 契约测试 (sign-in / get-session / sign-out).

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
        Arc::new(WsState {
            realtime: realtime.clone(),
            server_name: "test".into(),
        }),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("request"))
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
async fn sign_in_creates_new_user_and_session() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::auth::router().with_state(test_state(db));
    let email = format!("auth-test-{}@example.com", Uuid::new_v4().simple());
    let (status, body) = call(
        &app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": email, "name": "Auth Test" })),
        None,
    )
    .await;
    assert_eq!(status, 200, "sign-in: {body}");
    assert!(body["user_id"].is_string(), "user_id: {body}");
    let token = body["session_token"].as_str().expect("session_token");
    assert!(token.starts_with("tok_"), "token should start with tok_: {token}");

    // Use the session token to fetch session
    let (status, body) = call(
        &app,
        "GET",
        "/api/auth/get-session",
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, 200, "get-session: {body}");
    assert!(body["user_id"].as_str().unwrap_or("").len() > 0, "user_id: {body}");
    assert_eq!(body["method"], "session");
}

#[tokio::test(flavor = "current_thread")]
async fn sign_in_with_explicit_user_id_reuses_account() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::auth::router().with_state(test_state(db));
    let user_id = format!("u_explicit_{}", Uuid::new_v4().simple());
    let email = format!("explicit-{user_id}@example.com");
    let (status, body1) = call(
        &app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": email, "user_id": user_id })),
        None,
    )
    .await;
    assert_eq!(status, 200, "first sign-in: {body1}");
    assert_eq!(body1["user_id"], user_id);

    // Sign-in again with same user_id returns a new session token for the same user
    let (status, body2) = call(
        &app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": email, "user_id": user_id })),
        None,
    )
    .await;
    assert_eq!(status, 200, "second sign-in: {body2}");
    assert_eq!(body2["user_id"], user_id);
    assert_ne!(
        body1["session_token"], body2["session_token"],
        "different sessions on each sign-in"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sign_in_rejects_request_without_email_or_user_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::auth::router().with_state(test_state(db));
    // Empty email, no user_id → 400
    let (status, _) = call(&app, "POST", "/api/auth/sign-in", Some(json!({"email": ""})), None).await;
    assert_eq!(status, 400, "expected 400 for empty email+no user_id: got {status}");
}

#[tokio::test(flavor = "current_thread")]
async fn get_session_401_without_token() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::auth::router().with_state(test_state(db));
    let (status, _) = call(&app, "GET", "/api/auth/get-session", None, None).await;
    assert_eq!(status, 401, "expected 401 without auth");
}

#[tokio::test(flavor = "current_thread")]
async fn sign_out_returns_2xx_and_invalidates_session() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::auth::router().with_state(test_state(db));
    let email = format!("signout-{}@example.com", Uuid::new_v4().simple());
    let (_, body) = call(
        &app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    let token = body["session_token"].as_str().expect("token");

    // sign-out
    let (status, _) = call(
        &app,
        "POST",
        "/api/auth/sign-out",
        None,
        Some(token),
    )
    .await;
    assert!(
        status == 200 || status == 204,
        "sign-out status={status}"
    );

    // After sign-out, session lookup should fail
    let (status, _) = call(
        &app,
        "GET",
        "/api/auth/get-session",
        None,
        Some(token),
    )
    .await;
    assert!(
        status == 401,
        "expected 401 after sign-out: got {status}"
    );
}
