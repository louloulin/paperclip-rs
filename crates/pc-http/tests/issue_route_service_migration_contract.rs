//! R602 v6: 路由层 IssueService 迁移 端到端 contract 测试。
//!
//! 验证迁移到 IssueService 的 3 个高频 endpoint：
//! - `GET  /api/companies/:id/issues/count` —— IssueService.count_with_status
//! - `GET  /api/companies/:id/issues/by-status` —— IssueService.count_by_status
//! - `POST /api/companies/:id/issues` —— IssueService.create（触发 IssueActivityHook）

use std::sync::Arc;

use pc_adapter_api::AdapterRegistry;
use pc_activity::{ActivityKind, ActivityLog, InMemoryActivityLog, SharedActivitySink};
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

fn test_state_with_activity(db: Db) -> (AppState, Arc<InMemoryActivityLog>) {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    let in_mem = Arc::new(InMemoryActivityLog::new());
    let activity = ActivityLog::new(SharedActivitySink::new(in_mem.clone()));
    let state = AppState::new(
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
    .with_activity(activity);
    (state, in_mem)
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R602V6-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Issue-{id}"))
    .bind(status)
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

/// 真实启动 axum router（不通过 axum::Router 的额外 stateful 路径）— 直接构造 app_state + 调用 routes
async fn call_via_axum(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let app = pc_http::routes::issues::router().with_state(state.clone());
    let mut builder = Request::builder().method(method).uri(path);
    let mut req = if let Some(b) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let _ = &mut req; // silence unused mut warning if no body

    let response = app.oneshot(req).await.expect("send");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let json: serde_json::Value = if bytes.is_empty() {
        serde_json::json!(null)
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::json!(null))
    };
    (status, json)
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v6_create_endpoint_uses_issue_service_and_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_activity(db);
    let state_arc = Arc::new(state);
    let state_for_request = state_arc.as_ref().clone();
    let company_id = insert_company(&pool).await;

    // 通过 axum router POST 调用
    let (status, body) = call_via_axum(
        &state_for_request,
        "POST",
        &format!("/api/companies/{company_id}/issues"),
        Some(serde_json::json!({
            "title": "migrate-test",
            "description": "test",
            "priority": "high",
        })),
    )
    .await;
    assert!((200..300).contains(&status), "create should be 2xx, got {status}: {body}");
    assert_eq!(body["title"], "migrate-test");

    // 验证 IssueActivityHook.on_created 触发了
    let snapshot = in_mem.snapshot();
    let issue_created = snapshot
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::IssueCreated));
    assert!(
        issue_created,
        "expected IssueCreated activity via IssueActivityHook on_created, got {snapshot:?}"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v6_count_endpoint_uses_issue_service() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let state = test_state_with_activity(db).0;
    let state_for_request = state;
    let company_id = insert_company(&pool).await;
    insert_issue(&pool, company_id, "todo").await;
    insert_issue(&pool, company_id, "todo").await;
    insert_issue(&pool, company_id, "done").await;

    let (status, body) = call_via_axum(
        &state_for_request,
        "GET",
        &format!("/api/companies/{company_id}/issues/count"),
        None,
    )
    .await;
    assert_eq!(status, 200, "count should return 200");
    assert_eq!(body["count"], 3);

    // status filter
    let (_status, body) = call_via_axum(
        &state_for_request,
        "GET",
        &format!("/api/companies/{company_id}/issues/count?status=done"),
        None,
    )
    .await;
    assert_eq!(body["count"], 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v6_by_status_endpoint_uses_issue_service() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let state = test_state_with_activity(db).0;
    let company_id = insert_company(&pool).await;
    insert_issue(&pool, company_id, "todo").await;
    insert_issue(&pool, company_id, "todo").await;
    insert_issue(&pool, company_id, "done").await;

    let (status, body) = call_via_axum(
        &state,
        "GET",
        &format!("/api/companies/{company_id}/issues/by-status"),
        None,
    )
    .await;
    assert_eq!(status, 200, "by-status should return 200");
    assert_eq!(body["total"], 3);

    let groups = body["groups"].as_array().expect("groups array");
    let todo_count = groups
        .iter()
        .find(|g| g["status"] == "todo")
        .and_then(|g| g["count"].as_i64())
        .unwrap_or(0);
    let done_count = groups
        .iter()
        .find(|g| g["status"] == "done")
        .and_then(|g| g["count"].as_i64())
        .unwrap_or(0);
    assert_eq!(todo_count, 2);
    assert_eq!(done_count, 1);

    cleanup(&pool, company_id).await;
}
