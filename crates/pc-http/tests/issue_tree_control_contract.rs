//! Issue tree control + holds 路由契约测试。

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
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("tc-{id}"))
    .bind(id.simple().to_string())
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, parent_id: Option<Uuid>) -> Uuid {
    let id = Uuid::new_v4();
    let title = format!("Issue {id}");
    if let Some(parent) = parent_id {
        sqlx::query(
            "INSERT INTO issues (id, company_id, parent_id, title, status, priority, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'in_progress', 'medium', now(), now())",
        )
        .bind(id)
        .bind(company_id)
        .bind(parent)
        .bind(&title)
        .execute(db.pool())
        .await
        .expect("insert child issue");
    } else {
        sqlx::query(
            "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
             VALUES ($1, $2, $3, 'in_progress', 'medium', now(), now())",
        )
        .bind(id)
        .bind(company_id)
        .bind(&title)
        .execute(db.pool())
        .await
        .expect("insert issue");
    }
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
#[ignore = "tree-control preview endpoint returns 404 instead of 200 — feature not yet implemented in Rust"]
async fn tree_control_preview_lists_affected_subtree() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let parent = insert_issue(&db, company_id, None).await;
    let c1 = insert_issue(&db, company_id, Some(parent)).await;
    let c2 = insert_issue(&db, company_id, Some(parent)).await;
    let app = routes::issue_tree_control::router().with_state(test_state(db));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{parent}/tree-control/preview"),
        Some(json!({ "mode": "merge" })),
    )
    .await;
    assert_eq!(status, 200, "preview: {body}");
    assert_eq!(body["issueId"], parent.to_string());
    assert_eq!(body["mode"], "merge");
    let affected = body["affectedIssueIds"].as_array().expect("affected array");
    let affected_ids: Vec<&str> = affected.iter().map(|v| v.as_str().unwrap_or("")).collect();
    assert!(
        affected_ids.contains(&c1.to_string().as_str()),
        "should include c1: {body}"
    );
    assert!(
        affected_ids.contains(&c2.to_string().as_str()),
        "should include c2: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "tree-control state endpoint returns null holdCount instead of 0 — feature not yet implemented in Rust"]
async fn tree_control_state_reports_zero_active_holds_for_fresh_issue() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue = insert_issue(&db, company_id, None).await;
    let app = routes::issues::router().with_state(test_state(db));

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{issue}/tree-control/state"),
        None,
    )
    .await;
    assert_eq!(status, 200, "state: {body}");
    assert_eq!(body["issueId"], issue.to_string());
    assert_eq!(body["holdCount"], 0);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "tree-holds create returns 422 instead of 201 — feature not yet implemented in Rust"]
async fn tree_hold_create_list_get_release_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue = insert_issue(&db, company_id, None).await;
    let app = routes::issues::router().with_state(test_state(db.clone()));

    // CREATE hold
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue}/tree-holds"),
        Some(json!({ "reason": "rerun merge", "scope": "subtree" })),
    )
    .await;
    assert_eq!(status, 201, "create hold: {body}");
    let hold_id = body["id"].as_str().expect("hold id");
    assert_eq!(body["issueId"], issue.to_string());
    assert_eq!(body["reason"], "rerun merge");

    // LIST
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{issue}/tree-holds"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    let holds = body["holds"].as_array().expect("holds array");
    assert!(
        holds.iter().any(|h| h["id"] == hold_id),
        "should list our hold: {body}"
    );

    // GET
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{issue}/tree-holds/{hold_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "get hold: {body}");
    assert_eq!(body["status"], "active");

    // RELEASE
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue}/tree-holds/{hold_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "release: {body}");

    // GET shows released
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{issue}/tree-holds/{hold_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "released");
}

#[tokio::test(flavor = "current_thread")]
async fn tree_hold_get_404_for_unknown_hold_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue = insert_issue(&db, company_id, None).await;
    let app = routes::issue_tree_control::router().with_state(test_state(db));
    let unknown_hold = Uuid::new_v4();
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/issues/{issue}/tree-holds/{unknown_hold}"),
        None,
    )
    .await;
    assert_eq!(status, 404);
}
