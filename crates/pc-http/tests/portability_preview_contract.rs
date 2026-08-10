//! R593: PortabilityService preview 端到端 HTTP contract 测试。

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

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn insert_company(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R593-Contract-{id}"))
    .bind(format!("PC{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &sqlx::PgPool, id: Uuid) {
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_export_preview_returns_aggregates_via_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db.pool()).await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/exports/preview"),
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "preview: {body}");
    assert_eq!(body["version"], "1.0");
    assert_eq!(body["company"]["id"], company_id.to_string());
    assert_eq!(body["counts"]["issues"], 0);
    assert_eq!(body["counts"]["agents"], 0);
    assert_eq!(body["counts"]["pipelines"], 0);
    assert!(body["issues"].is_array());
    assert!(body["agents"].is_array());
    assert!(body["pipelines"].is_array());

    cleanup(&db.pool(), company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_export_preview_missing_returns_404() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::companies::router().with_state(test_state(db));

    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/companies/{}/exports/preview", Uuid::new_v4()),
        json!({}),
    )
    .await;
    assert_eq!(status, 404);
}
