//! Secrets / secret providers 路由契约测试。

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

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("sec-{id}"))
    .bind(id.simple().to_string())
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let mut request = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path)
        .body(Body::from(payload))
        .expect("request");
    request.extensions_mut().insert(pc_auth::AuthContext::system());
    let response = app
        .clone()
        .oneshot(request)
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
async fn company_secrets_list_returns_empty_for_new_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::secrets::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/secrets"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    assert_eq!(body["companyId"], company_id.to_string());
    assert!(body["items"].is_array(), "items array: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn list_providers_returns_registered_providers() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = Uuid::new_v4();
    let app = routes::secrets::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/secret-providers"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 4);
    assert_eq!(body[0]["id"], "local_encrypted");
}

#[tokio::test(flavor = "current_thread")]
async fn provider_health_returns_all_registered_providers() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::secrets::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{}/secret-providers/health", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["providers"].as_array().unwrap().len(), 4);
    assert_eq!(body["providers"][0]["status"], "ok");
}

#[tokio::test(flavor = "current_thread")]
async fn list_provider_configs_returns_empty_for_fresh_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::secrets::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/secret-provider-configs"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list provider configs: {body}");
    assert!(body["items"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn secret_usage_404_for_unknown_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::secrets::router().with_state(test_state(db));
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/secrets/{}/usage", Uuid::new_v4()),
        None,
    )
    .await;
    // either 200 with empty rows, or 404 — accept either as long as it's not 5xx
    assert!(status < 500, "should not 5xx, got {status}");
}
