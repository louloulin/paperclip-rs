//! Dashboard 路由契约测试：empty summary, recovery_observability stub, with issues/cost/approvals 数据。

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

async fn insert_company(db: &Db, budget: i32) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, budget_monthly_cents, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, $4, now(), now())",
    )
    .bind(id)
    .bind(format!("dash-{id}"))
    .bind(id.simple().to_string())
    .bind(budget)
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, status: &str) {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'medium', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Issue {id}"))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("insert issue");
}

async fn insert_agent(db: &Db, company_id: Uuid, status: &str) {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, runtime_config, permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', $4, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent {id}"))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("insert agent");
}

async fn call(app: &axum::Router, method: &str, path: &str) -> (u16, Value) {
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
async fn dashboard_summary_returns_zero_counts_for_empty_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, 100_000).await;
    let app = routes::dashboard::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/dashboard"),
    )
    .await;
    assert_eq!(status, 200, "dashboard: {body}");
    assert_eq!(body["companyId"], company_id.to_string());
    assert_eq!(body["agents"]["running"], 0);
    assert_eq!(body["tasks"]["open"], 0);
    assert_eq!(body["tasks"]["done"], 0);
    assert_eq!(body["costs"]["monthSpendCents"], 0);
    assert_eq!(body["costs"]["monthBudgetCents"], 100_000);
    assert_eq!(body["pendingApprovals"], 0);
    assert_eq!(body["budgets"]["pausedProjects"], 0);
    let run_activity = body["runActivity"].as_array().expect("runActivity");
    assert_eq!(run_activity.len(), 14, "14 day window");
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_summary_aggregates_agents_and_issues() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, 0).await;
    insert_agent(&db, company_id, "running").await;
    insert_agent(&db, company_id, "running").await;
    insert_agent(&db, company_id, "paused").await;
    insert_issue(&db, company_id, "backlog").await;
    insert_issue(&db, company_id, "in_progress").await;
    insert_issue(&db, company_id, "done").await;
    let app = routes::dashboard::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/dashboard"),
    )
    .await;
    assert_eq!(status, 200, "dashboard with data: {body}");
    assert_eq!(body["agents"]["running"], 2);
    assert_eq!(body["agents"]["paused"], 1);
    assert!(
        body["tasks"]["open"].as_i64().unwrap_or(0) >= 1,
        "open tasks ≥ 1"
    );
    assert!(body["tasks"]["inProgress"].as_i64().unwrap_or(0) >= 1);
    assert!(body["tasks"]["done"].as_i64().unwrap_or(0) >= 1);
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_summary_404_for_unknown_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::dashboard::router().with_state(test_state(db));
    let (status, _) = call(&app, "GET", &format!("/api/companies/{}", Uuid::new_v4())).await;
    assert_eq!(status, 404, "unknown company should 404");
}

#[tokio::test(flavor = "current_thread")]
async fn recovery_observability_returns_stub_shape() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = Uuid::new_v4();
    let app = routes::dashboard::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/recovery-observability"),
    )
    .await;
    assert_eq!(status, 200, "recovery: {body}");
    assert_eq!(body["companyId"], company_id.to_string());
    assert!(body["weeks"].as_f64().unwrap() > 0.0);
    assert!(body["thresholdPercent"].as_f64().unwrap() > 0.0);
    assert_eq!(body["summary"]["meetsThreshold"], json!(true));
    assert!(body["series"].as_array().unwrap().is_empty());
}
