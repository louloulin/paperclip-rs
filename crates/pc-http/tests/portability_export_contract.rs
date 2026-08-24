//! R600: PortabilityService.export 端到端 HTTP contract 测试。

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

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R600-Export-{id}"))
    .bind(id.simple().to_string())
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, \
         permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent-{id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, 'todo', 'medium', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Issue-{id}"))
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_pipeline(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("pipe_{id}"))
    .bind(format!("Pipeline {id}"))
    .execute(db.pool())
    .await
    .expect("insert pipeline");
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_empty_company_via_http() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/export"),
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "export: {body}");
    assert_eq!(body["companyId"], company_id.to_string());
    assert_eq!(body["version"], "1.0");
    assert_eq!(body["status"], "exported");
    assert_eq!(body["counts"]["agents"], 0);
    assert_eq!(body["counts"]["issues"], 0);
    assert_eq!(body["counts"]["pipelines"], 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_with_manifest_data() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_agent(&db, company_id).await;
    insert_issue(&db, company_id).await;
    insert_pipeline(&db, company_id).await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/export"),
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "export: {body}");
    assert_eq!(body["counts"]["agents"], 1);
    assert_eq!(body["counts"]["issues"], 1);
    assert_eq!(body["counts"]["pipelines"], 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_missing_company_returns_404() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::companies::router().with_state(test_state(db));

    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/companies/{}/export", Uuid::new_v4()),
        json!({}),
    )
    .await;
    assert_eq!(status, 404);
}
