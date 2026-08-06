//! Health + feature-flags + assets 路由契约测试。

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
        Arc::new(WsState::new(
            realtime.clone(),
            "test".to_string(),
        )),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
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

#[tokio::test(flavor = "current_thread")]
async fn health_returns_ok_with_db_payload() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::health::router().with_state(test_state(db));
    let (status, body) = call(&app, "GET", "/health", None).await;
    assert_eq!(status, 200, "health: {body}");
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string(), "version: {body}");
    assert_eq!(body["db"]["ok"], json!(true));
}

#[tokio::test(flavor = "current_thread")]
async fn feature_flags_list_reflects_registered_flags() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::feature_flags::router().with_state(test_state(db));
    let key = format!("test.list.{}", Uuid::new_v4().simple());
    // List before registration: may be empty
    let (status, body) = call(&app, "GET", "/api/feature-flags", None).await;
    assert_eq!(status, 200, "list: {body}");
    let before = body["items"].as_array().expect("items array").len();
    // Register one
    let (status, body) = call(
        &app,
        "POST",
        "/api/feature-flags",
        Some(json!({ "key": key.clone(), "enabled": true, "rolloutPct": 50 })),
    )
    .await;
    assert_eq!(status, 200, "register: {body}");
    // List should now include it
    let (status, body) = call(&app, "GET", "/api/feature-flags", None).await;
    assert_eq!(status, 200);
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), before + 1, "list should grow after register");
    let found = items.iter().any(|it| it["key"] == key && it["enabled"] == json!(true) && it["hasRollout"] == json!(true));
    assert!(found, "registered flag should appear in list with enabled+hasRollout true: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn feature_flags_register_then_evaluate_then_toggle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::feature_flags::router().with_state(test_state(db));

    // Register a fresh flag with a rollout percentage
    let key = format!("test.flag.{}", Uuid::new_v4().simple());
    let (status, body) = call(
        &app,
        "POST",
        "/api/feature-flags",
        Some(json!({
            "key": key.clone(),
            "enabled": true,
            "rolloutPct": 100
        })),
    )
    .await;
    assert_eq!(status, 200, "register: {body}");
    assert_eq!(body["key"], key);
    assert_eq!(body["registered"], json!(true));

    // Evaluate with an actor id
    let actor_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "POST",
        "/api/feature-flags/evaluate",
        Some(json!({
            "key": key.clone(),
            "actorId": actor_id
        })),
    )
    .await;
    assert_eq!(status, 200, "eval: {body}");
    assert_eq!(body["key"], key);
    assert_eq!(body["actorId"], actor_id.to_string());
    assert_eq!(body["enabled"], json!(true));

    // Toggle enabled=false
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/feature-flags/{key}/enabled"),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, 200, "toggle off: {body}");
    assert_eq!(body["enabled"], json!(false));

    // Now evaluate again — should be false
    let (status, body) = call(
        &app,
        "POST",
        "/api/feature-flags/evaluate",
        Some(json!({
            "key": key.clone(),
            "actorId": actor_id
        })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["enabled"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn feature_flags_set_enabled_unknown_key_returns_not_found() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::feature_flags::router().with_state(test_state(db));
    let key = format!("does.not.exist.{}", Uuid::new_v4().simple());
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/feature-flags/{key}/enabled"),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, 404, "expected 404 for unknown flag: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn feature_flags_register_with_allow_list() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::feature_flags::router().with_state(test_state(db));
    let key = format!("test.allow.{}", Uuid::new_v4().simple());
    let allow_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "POST",
        "/api/feature-flags",
        Some(json!({
            "key": key.clone(),
            "enabled": true,
            "rolloutAllowList": [allow_id]
        })),
    )
    .await;
    assert_eq!(status, 200, "register allow list: {body}");
    assert_eq!(body["registered"], json!(true));

    // Eval should return enabled since we pre-baked an allow list entry
    let (status, body) = call(
        &app,
        "POST",
        "/api/feature-flags/evaluate",
        Some(json!({ "key": key, "actorId": allow_id })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["enabled"], json!(true));
}
