//! `/api/activity/*` `/api/attention` `/api/companies/:id/import-paths` 路由契约测试。

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
    let prefix: [u8; 2] = rand::random();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("obs-{id}"))
    .bind(format!("OB{:02X}{:02X}", prefix[0], prefix[1]))
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
async fn activity_emit_and_list_roundtrip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::activity::router().with_state(test_state(db.clone()));

    let subject_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "POST",
        "/api/activity/emit",
        Some(json!({
            "kind": "issue.created",
            "actor_type": "agent",
            "actor_id": Uuid::new_v4(),
            "subject_kind": "issue",
            "subject_id": subject_id,
            "company_id": company_id,
            "payload": { "title": "Test issue" }
        })),
    )
    .await;
    assert_eq!(status, 201, "emit: {body}");
    let event_id = body["id"].as_str().expect("id");

    // List filter by company
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/activity/list?company_id={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    let arr = body["items"].as_array().expect("items array");
    assert!(
        arr.iter().any(|e| e["id"] == event_id),
        "emitted event should appear in list"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn activity_emit_rejects_unknown_kind() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::activity::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/activity/emit",
        Some(json!({
            "kind": "unknown.kind",
            "subject_kind": "issue",
            "subject_id": Uuid::new_v4()
        })),
    )
    .await;
    assert_eq!(status, 400, "unknown kind: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn attention_list_returns_empty_for_company_with_no_signals() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::attention::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/attention"),
        None,
    )
    .await;
    assert_eq!(status, 200, "attention: {body}");
    let arr = body["items"].as_array().expect("items array");
    assert!(arr.is_empty(), "no items expected for empty company");
}

#[tokio::test(flavor = "current_thread")]
async fn attention_list_includes_pending_approval() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;

    // Insert a pending approval
    let approval_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO approvals (id, company_id, type, status, payload) \
         VALUES ($1, $2, 'hire_agent', 'pending', '{\"name\":\"X\"}'::jsonb)",
    )
    .bind(approval_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert approval");

    let app = routes::attention::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/attention"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body["items"].as_array().expect("items array");
    assert!(
        arr.iter().any(|i| {
            let sid = i["subject"]["id"].as_str().unwrap_or("");
            sid == approval_id.to_string() || i["kind"] == "approval"
        }),
        "pending approval should appear in attention queue"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn company_import_paths_returns_empty_default() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::company_import_paths::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/import-paths"),
        None,
    )
    .await;
    assert_eq!(status, 200, "import paths: {body}");
    let paths = body["paths"].as_array().expect("paths array");
    assert!(paths.is_empty());
}
