//! R587: `/api/decisions*` HTTP 契约测试。
//!
//! 覆盖：
//! - GET/POST `/api/decisions`
//! - POST `/api/decisions/:id/decide`
//! - POST `/api/decisions/:id/dismiss`
//! - POST `/api/decisions/:id/cancel`

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
use pc_secrets::DecisionSigningService;
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
    .with_decision_signing(Arc::new(
        DecisionSigningService::from_secret("0123456789abcdef0123456789abcdef")
            .expect("test signing secret"),
    ))
}

async fn insert_company_with_agent_issue_run(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("dec-http-{company_id}"))
    .bind(format!("DH{}", &company_id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent {agent_id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, 'Decision test', 'todo', 'medium', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert issue");
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, created_at, updated_at) \
         VALUES ($1, $2, $3, 'queued', 'manual_test', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert run");
    company_id
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
    request
        .extensions_mut()
        .insert(pc_auth::AuthContext::system());
    let response = app.clone().oneshot(request).await.expect("response");
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
async fn r587_http_create_then_list_decisions() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company_with_agent_issue_run(&db).await;
    let app = routes::decisions::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/decisions",
        Some(json!({
            "company_id": company_id,
            "title": "Pick a framework",
            "body": "Should we use Axum or Hyper?"
        })),
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let decision_id = body["id"].as_str().expect("id");
    assert!(!decision_id.is_empty());

    // list by company
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/decisions?company_id={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|d| d["id"] == decision_id));
}

#[tokio::test(flavor = "current_thread")]
async fn r587_http_create_rejects_empty_inputs() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company_with_agent_issue_run(&db).await;
    let app = routes::decisions::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/decisions",
        Some(json!({
            "company_id": company_id,
            "title": "",
            "body": "x"
        })),
    )
    .await;
    assert_eq!(status, 400, "empty title: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r587_http_decide_endpoint_runs_through_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company_with_agent_issue_run(&db).await;
    let app = routes::decisions::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/decisions",
        Some(json!({
            "company_id": company_id,
            "title": "Decide me",
            "body": "Pick A"
        })),
    )
    .await;
    let decision_id = body["id"].as_str().expect("id").to_string();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/decisions/{decision_id}/decide"),
        Some(json!({
            "chosenOptionId": "opt-a",
            "decidedByUserId": "user-1",
            "note": "looks good"
        })),
    )
    .await;
    assert_eq!(status, 200, "decide: {body}");
    assert_eq!(body["status"], "decided");
    assert_eq!(body["chosen_option_id"], "opt-a");
}

#[tokio::test(flavor = "current_thread")]
async fn r587_http_dismiss_endpoint_runs_through_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company_with_agent_issue_run(&db).await;
    let app = routes::decisions::router().with_state(test_state(db.clone()));

    let (_, body) = call(
        &app,
        "POST",
        "/api/decisions",
        Some(json!({
            "company_id": company_id,
            "title": "Dismiss me",
            "body": "n/a"
        })),
    )
    .await;
    let decision_id = body["id"].as_str().expect("id").to_string();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/decisions/{decision_id}/dismiss"),
        Some(json!({
            "reason": "not needed",
            "decidedByUserId": "user-1"
        })),
    )
    .await;
    assert_eq!(status, 200, "dismiss: {body}");
    assert_eq!(body["status"], "dismissed");
}

#[tokio::test(flavor = "current_thread")]
async fn r587_http_cancel_endpoint_runs_through_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company_with_agent_issue_run(&db).await;
    let app = routes::decisions::router().with_state(test_state(db.clone()));

    let (_, body) = call(
        &app,
        "POST",
        "/api/decisions",
        Some(json!({
            "company_id": company_id,
            "title": "Cancel me",
            "body": "n/a"
        })),
    )
    .await;
    let decision_id = body["id"].as_str().expect("id").to_string();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/decisions/{decision_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, 200, "cancel: {body}");
    assert_eq!(body["status"], "cancelled");
}
