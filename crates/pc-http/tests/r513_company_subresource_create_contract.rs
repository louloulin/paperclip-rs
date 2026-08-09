//! R513 — 4 个真缺漏的公司级 POST 路由 contract：
//! - `POST /api/companies/:company_id/approvals`
//! - `POST /api/companies/:company_id/decisions`
//! - `POST /api/companies/:company_id/pipelines`
//!
//! 这些 endpoint 在 Node `approvals.ts:124` / `decisions.ts:42` /
//! `pipelines.ts:891` 存在，Rust 端只有 GET。补齐后路由覆盖率
//! 期望从 97.76% 上升。

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
    .bind(format!("r513-{id}"))
    .bind(format!("R5{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, \
         adapter_config, runtime_config, permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, \
         '{}'::jsonb, '{}'::jsonb, now(), now())",
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
         VALUES ($1, $2, 'Decision test', 'todo', 'medium', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_heartbeat_run(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source) \
         VALUES ($1, $2, $3, 'queued', 'manual_test') ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert run");
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
async fn company_approvals_post_creates_pending_approval() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/approvals"),
        Some(json!({
            "approval_type": "hire_agent",
            "payload": { "name": "Hire Bot", "role": "general" },
            "requested_by_user_id": "board-test"
        })),
    )
    .await;
    assert_eq!(status, 201, "approval create under company: {body}");
    assert_eq!(body["companyId"], company_id.to_string());
    assert_eq!(body["status"], "pending");
    assert_eq!(body["approvalType"], "custom");

    let (status, list) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/approvals"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let items = list["items"].as_array().expect("items array");
    assert!(
        items
            .iter()
            .any(|item| item["id"] == body["id"] && item["companyId"] == company_id.to_string()),
        "items should contain the just-created approval; items={list}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn company_approvals_post_rejects_empty_approval_type() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/approvals"),
        Some(json!({
            "approval_type": "",
            "payload": {}
        })),
    )
    .await;
    assert_eq!(status, 400);
}

#[tokio::test(flavor = "current_thread")]
async fn company_decisions_post_creates_pending_decision() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let _issue_id = insert_issue(&db, company_id).await;
    let _run_id = insert_heartbeat_run(&db, company_id, agent_id).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/decisions"),
        Some(json!({
            "title": "Migrate auth to OIDC",
            "body": "Replace password auth with corporate OIDC."
        })),
    )
    .await;
    assert_eq!(status, 201, "decision create under company: {body}");
    assert_eq!(body["title"], "Migrate auth to OIDC");
    assert_eq!(body["company_id"], company_id.to_string());
    let decision_id = body["id"].as_str().expect("id");

    let (status, list) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/decisions"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let items = list["items"].as_array().expect("items array");
    assert!(
        items.iter().any(|item| item["id"] == decision_id
            && (item["company_id"].as_str() == Some(company_id.to_string().as_str())
                || item["companyId"].as_str() == Some(company_id.to_string().as_str()))),
        "items should contain the just-created decision; items={list}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn company_pipelines_post_creates_pipeline() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/pipelines"),
        Some(json!({
            "key": "r513-pipeline",
            "name": "R513 Pipeline",
            "description": "created via company-scoped POST"
        })),
    )
    .await;
    assert_eq!(status, 201, "pipeline create under company: {body}");
    assert_eq!(body["companyId"], company_id.to_string());
    assert_eq!(body["key"], "r513-pipeline");
    let pipeline_id = body["id"].as_str().expect("id");

    let (status, list) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/pipelines"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let items = list["items"].as_array().expect("items array");
    assert!(
        items
            .iter()
            .any(|item| item["id"] == pipeline_id && item["companyId"] == company_id.to_string()),
        "items should contain the just-created pipeline; items={list}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn company_pipelines_post_rejects_empty_key() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/pipelines"),
        Some(json!({
            "key": "",
            "name": "Bad Pipeline",
            "description": null
        })),
    )
    .await;
    assert_eq!(status, 400);
}

// =============================================================================
// R514 — `PUT /api/cases/:case_id/documents/:key` contract
// =============================================================================

async fn insert_case(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, identifier, case_number, case_type, title, status, fields, created_at, updated_at) \
         VALUES ($1, $2, $5, $3, 'task', $4, 'draft', '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(1_i32)
    .bind("R514 Case")
    .bind(format!("r514-{}", &id.simple().to_string()[..6]))
    .execute(db.pool())
    .await
    .expect("insert case");
    id
}

#[tokio::test(flavor = "current_thread")]
async fn case_document_put_creates_new_document() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let case_id = insert_case(&db, company_id).await;
    let app = routes::cases::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec",
            "format": "markdown",
            "body": "# R514 spec\n\nInitial content.",
            "changeSummary": "Initial draft"
        })),
    )
    .await;
    assert_eq!(status, 200, "case document upsert: {body}");
    let document_id = body["id"].as_str().expect("document id");
    assert_eq!(body["latestRevisionNumber"].as_u64(), Some(1));

    let (status, fetched) = call(
        &app,
        "GET",
        &format!("/api/cases/{case_id}/documents/spec"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(fetched["documentId"], document_id);
}

#[tokio::test(flavor = "current_thread")]
async fn case_document_put_update_increases_revision_number() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let case_id = insert_case(&db, company_id).await;
    let app = routes::cases::router().with_state(test_state(db.clone()));

    let (_, body) = call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec",
            "format": "markdown",
            "body": "first version"
        })),
    )
    .await;
    let base_revision_id = body["latestRevisionId"].as_str().expect("latestRevisionId");
    let first_revision_number = body["latestRevisionNumber"].as_u64().expect("latestRevisionNumber");

    let (status, updated) = call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec v2",
            "format": "markdown",
            "body": "second version",
            "baseRevisionId": base_revision_id
        })),
    )
    .await;
    assert_eq!(status, 200, "second upsert: {updated}");
    let second_revision_number = updated["latestRevisionNumber"]
        .as_u64()
        .expect("latestRevisionNumber");
    assert!(
        second_revision_number > first_revision_number,
        "revision number should advance (first={first_revision_number}, second={second_revision_number})"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn case_document_put_rejects_stale_base_revision() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let case_id = insert_case(&db, company_id).await;
    let app = routes::cases::router().with_state(test_state(db.clone()));

    call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec",
            "format": "markdown",
            "body": "v1"
        })),
    )
    .await;

    call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec v2",
            "format": "markdown",
            "body": "v2"
        })),
    )
    .await;

    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec v3",
            "format": "markdown",
            "body": "v3",
            "baseRevisionId": Uuid::new_v4().to_string()
        })),
    )
    .await;
    assert_eq!(status, 409, "stale baseRevisionId should be rejected");
}

#[tokio::test(flavor = "current_thread")]
async fn case_document_put_rejects_locked_document() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let case_id = insert_case(&db, company_id).await;
    let app = routes::cases::router().with_state(test_state(db.clone()));

    call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec",
            "format": "markdown",
            "body": "v1"
        })),
    )
    .await;

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/cases/{case_id}/documents/spec/lock"),
        None,
    )
    .await;
    assert_eq!(status, 200);

    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/cases/{case_id}/documents/spec"),
        Some(json!({
            "title": "Design Spec v2",
            "format": "markdown",
            "body": "v2"
        })),
    )
    .await;
    assert_eq!(status, 409, "locked document should be rejected");
}
