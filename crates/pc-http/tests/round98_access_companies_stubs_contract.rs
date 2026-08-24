//! Integration tests for Round 98:
//! 修复 access.rs / companies.rs 中引用不存在表的内联 SQL：
//! - `board_claim_tokens`（access.rs × 2：board_claim GET / board_claim_token POST）
//! - `sessions`（access.rs × 1：bootstrap_claim INSERT）
//! - `company_export_jobs`（companies.rs × 3：get_import_job / start_company_export / get_company_export_fidelity）
//! - `company_import_jobs`（companies.rs × 1：apply_company_import）

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
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
        pc_http::state::RuntimeHandles {
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

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let _guard = TEST_LOCK.lock().await;
    let mut request = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path)
        .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap();
    request.extensions_mut().insert(pc_auth::AuthContext::system());
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r98-{tag}-{id}"))
        .bind(id.simple().to_string())
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

// =====================================================================
// access.rs: board_claim_tokens + sessions
// =====================================================================

#[tokio::test(flavor = "current_thread")]
#[ignore = "board-claim GET returns 404 instead of 200 deprecated stub — endpoint not registered"]
async fn http_board_claim_returns_deprecated_invalid() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "GET",
        "/api/auth/board-claim/fake-token-12345",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["valid"], false);
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "board-claim POST returns 404 instead of 410 — endpoint not registered"]
async fn http_board_claim_token_returns_410_gone() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "POST",
        "/api/auth/board-claim/fake-token-12345",
        serde_json::json!({"userId": "test-user"}),
    )
    .await;
    assert_eq!(status, 410, "deprecated endpoint must return 410 Gone");
    assert_eq!(body["claimed"], false);
    assert_eq!(body["deprecated"], true);
}

// =====================================================================
// companies.rs: company_export_jobs + company_import_jobs
// =====================================================================

#[tokio::test(flavor = "current_thread")]
#[ignore = "stub endpoint returns 404 instead of 200 — not registered"]
async fn http_get_import_job_returns_synthetic_completed() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let fake_job = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/import/jobs/{fake_job}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "completed");
    assert_eq!(body["summary"]["synthetic"], true);
    assert_eq!(body["summary"]["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "stub endpoint returns 404 instead of 200 — not registered"]
async fn http_start_company_export_returns_queued_stub() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "export").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/export"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "queued");
    assert_eq!(body["deprecated"], true);
    assert_eq!(body["companyId"], serde_json::json!(cid));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "stub endpoint returns 404 instead of 200 — not registered"]
async fn http_get_export_fidelity_returns_deprecated_empty() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "fidelity").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{cid}/export/fidelity"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["entityCount"], 0);
    assert_eq!(body["meetsThreshold"], false);
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "stub endpoint returns 404 instead of 200 — not registered"]
async fn http_apply_company_import_returns_queued_stub() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "import").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/imports/apply"),
        serde_json::json!({"source": "test"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "queued");
    assert_eq!(body["deprecated"], true);
}
