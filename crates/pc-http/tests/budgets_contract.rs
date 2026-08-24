//! R586: `/api/companies/:company_id/budgets*` HTTP 契约测试。
//!
//! 覆盖：
//! - GET/POST `/api/companies/:company_id/budgets/policies`
//! - GET `/api/companies/:company_id/budget-incidents`
//! - POST `/api/companies/:company_id/budget-incidents/:id/resolve`
//! - GET `/api/companies/:company_id/budgets/overview`
//! - GET `/api/agents/:agent_id/budgets`

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

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("budget-{id}"))
    .bind(id.simple().to_string())
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, \
         adapter_config, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent {id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
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

// =============================================================================
// R586: budget policy / incident / overview / agent endpoints
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn r586_http_upsert_then_list_policies() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let app = routes::budgets::router().with_state(test_state(db.clone()));

    // POST upsert
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/budgets/policies"),
        Some(json!({
            "scopeType": "agent",
            "scopeId": agent_id,
            "amount": 10000,
            "warnPercent": 80,
            "hardStopEnabled": true,
            "notifyEnabled": true
        })),
    )
    .await;
    assert_eq!(status, 200, "upsert: {body}");
    assert_eq!(body["scopeType"], "agent");
    assert_eq!(body["amount"], 10000);
    assert!(body["id"].as_str().is_some());

    // GET list
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/budgets/policies"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["scopeId"], agent_id.to_string());
}

#[tokio::test(flavor = "current_thread")]
async fn r586_http_upsert_policy_rejects_invalid_window_kind() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let app = routes::budgets::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/budgets/policies"),
        Some(json!({
            "scopeType": "agent",
            "scopeId": agent_id,
            "amount": 1000,
            "windowKind": "not_a_real_window"
        })),
    )
    .await;
    assert_eq!(status, 400, "invalid window kind: {body}");
    // ApiError serializes as {"error": "message string"}
    assert!(
        body["error"].as_str().unwrap_or("").contains("invalid window kind"),
        "error should contain 'invalid window kind': {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r586_http_overview_endpoint_returns_summary() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let app = routes::budgets::router().with_state(test_state(db.clone()));

    // 创建 1 个 policy
    call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/budgets/policies"),
        Some(json!({
            "scopeType": "agent",
            "scopeId": agent_id,
            "amount": 5000
        })),
    )
    .await;

    // GET overview
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/budgets/overview"),
        None,
    )
    .await;
    assert_eq!(status, 200, "overview: {body}");
    let summary = &body["summary"];
    assert_eq!(summary["policyCount"], 1);
    assert_eq!(summary["incidentCount"], 0);
    assert_eq!(summary["openIncidentCount"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn r586_http_resolve_incident_endpoint_handles_missing() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::budgets::router().with_state(test_state(db.clone()));

    // 试图 resolve 一个不存在的 incident → 404
    let missing_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/budget-incidents/{missing_id}/resolve"),
        Some(json!({
            "resolutionKind": "dismissed",
            "resolutionNote": "test"
        })),
    )
    .await;
    assert_eq!(status, 404, "missing incident: {body}");
    // ApiError serializes as {"error": "message string"}
    let err_msg = body["error"].as_str().unwrap_or("");
    assert!(
        err_msg.contains(&missing_id.to_string()),
        "error should contain incident id: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r586_http_agent_budgets_endpoint_runs_evaluation() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let app = routes::budgets::router().with_state(test_state(db.clone()));

    // 先创建一个 policy
    call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/budgets/policies"),
        Some(json!({
            "scopeType": "agent",
            "scopeId": agent_id,
            "amount": 1000,
            "warnPercent": 80
        })),
    )
    .await;

    // GET /api/agents/:agent_id/budgets
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/budgets"),
        None,
    )
    .await;
    assert_eq!(status, 200, "agent budgets: {body}");
    assert_eq!(body["agentId"], agent_id.to_string());
    assert_eq!(body["companyId"], company_id.to_string());
    let evals = body["evaluations"].as_array().expect("evaluations array");
    assert_eq!(evals.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r586_http_agent_budgets_endpoint_404_for_missing_agent() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::budgets::router().with_state(test_state(db.clone()));
    let missing_agent = Uuid::new_v4();
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{missing_agent}/budgets"),
        None,
    )
    .await;
    assert_eq!(status, 404);
}
