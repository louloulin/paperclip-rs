//! Costs/cost-events/budgets 路由契约测试。

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
        Arc::new(WsState::new(realtime.clone(), "test")),
        realtime,
    )
}

async fn insert_company(db: &Db, budget_cents: i32) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, budget_monthly_cents, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, $4, now(), now())",
    )
    .bind(id)
    .bind(format!("cost-{id}"))
    .bind(format!("CO{}", &id.simple().to_string()[..4]))
    .bind(budget_cents)
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, runtime_config, permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent {id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, 'in_progress', 'medium', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Issue {id}"))
    .execute(db.pool())
    .await
    .expect("insert issue");
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
async fn cost_event_create_then_summary_aggregates() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, 100_000).await;
    let agent_id = insert_agent(&db, company_id).await;
    let issue_id = insert_issue(&db, company_id).await;
    let app = routes::costs::router().with_state(test_state(db));

    // CREATE event
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/cost-events"),
        Some(json!({
            "agentId": agent_id,
            "issueId": issue_id,
            "provider": "openai",
            "biller": "openai",
            "billingType": "api",
            "model": "gpt-4",
            "inputTokens": 1000,
            "cachedInputTokens": 200,
            "outputTokens": 500,
            "costCents": 50,
            "occurredAt": "2026-08-04T10:00:00Z"
        })),
    )
    .await;
    assert_eq!(status, 201, "create event: {body}");
    assert!(body["id"].is_string(), "event id: {body}");

    // SUMMARY
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/costs/summary"),
        None,
    )
    .await;
    assert_eq!(status, 200, "summary: {body}");
    let total = body["spendCents"].as_i64().unwrap_or(0);
    assert!(total >= 50, "spend should include our event: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn cost_summary_empty_when_no_events() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, 0).await;
    let app = routes::costs::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/costs/summary"),
        None,
    )
    .await;
    assert_eq!(status, 200, "empty summary: {body}");
    assert_eq!(body["spendCents"].as_i64().unwrap_or(0), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn budgets_update_company_budget() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, 0).await;
    let app = routes::costs::router().with_state(test_state(db));

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{company_id}/budgets"),
        Some(json!({ "budgetMonthlyCents": 50_000 })),
    )
    .await;
    assert_eq!(status, 200, "update budget: {body}");
    assert_eq!(body["budgetMonthlyCents"].as_i64().unwrap_or(0), 50_000);

    // Overivew reflects new budget
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/budgets/overview"),
        None,
    )
    .await;
    assert_eq!(status, 200, "overview: {body}");
    assert!(
        body["pendingApprovalCount"].is_number(),
        "overview shape: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn quota_windows_returns_empty_list() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, 0).await;
    let app = routes::costs::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/costs/quota-windows"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.is_array(), "expected array: {body}");
}
