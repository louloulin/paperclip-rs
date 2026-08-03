//! `/api/approvals*` 与 `/api/decisions*` 路由契约测试。

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
        Arc::new(WsState {
            realtime: realtime.clone(),
            server_name: "test".into(),
        }),
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
    .bind(format!("ap-dec-{id}"))
    .bind(format!("AP{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let q = "INSERT INTO agents (id, company_id, name, role, adapter_type, status, \
             adapter_config, runtime_config, permissions, created_at, updated_at) \
             VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, \
             '{}'::jsonb, '{}'::jsonb, now(), now())";
    sqlx::query(q)
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

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
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
async fn approval_create_get_list_decide_delete_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/approvals",
        Some(json!({
            "company_id": company_id,
            "approval_type": "hire_agent",
            "payload": { "name": "Pending Bot", "role": "general" }
        })),
    )
    .await;
    assert_eq!(status, 201, "approval create: {body}");
    let approval_id = body["id"].as_str().expect("id");
    assert_eq!(body["status"], "pending");
    assert_eq!(body["approval_type"], "hire_agent");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/approvals/{approval_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], approval_id);

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/approvals?companyId={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|a| a["id"] == approval_id));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/decide"),
        Some(json!({
            "status": "approved",
            "decided_by": "board-user",
            "note": "Looks good"
        })),
    )
    .await;
    assert_eq!(status, 200, "decide: {body}");
    assert_eq!(body["status"], "approved");
    assert_eq!(body["decidedByUserId"], "board-user");

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/approvals/{approval_id}"),
        None,
    )
    .await;
    assert_eq!(status, 204);
}

#[tokio::test(flavor = "current_thread")]
async fn approval_create_rejects_empty_approval_type() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/approvals",
        Some(json!({
            "company_id": company_id,
            "approval_type": "",
            "payload": {}
        })),
    )
    .await;
    assert_eq!(status, 400, "empty type: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn decision_create_and_list_filter_by_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let _issue_id = insert_issue(&db, company_id).await;
    let _run_id = insert_heartbeat_run(&db, company_id, agent_id).await;
    let app = routes::decisions::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/decisions",
        Some(json!({
            "company_id": company_id,
            "title": "Use Rust for backend",
            "body": "Replace Node with Rust for performance gains"
        })),
    )
    .await;
    assert_eq!(status, 201, "decision create: {body}");
    let decision_id = body["id"].as_str().expect("id");
    assert_eq!(body["title"], "Use Rust for backend");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/decisions?companyId={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|d| d["id"] == decision_id));
}
