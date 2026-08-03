//! `/api/cases` `/api/projects` `/api/goals` `/api/environments` `/api/folders` CRUD 路由契约测试。

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
    .bind(format!("crud-{id}"))
    .bind(format!("CR{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
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
async fn cases_create_get_patch_delete_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::cases::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/cases",
        Some(json!({
            "company_id": company_id,
            "case_type": "pipeline",
            "title": "Test case",
            "summary": "Initial summary"
        })),
    )
    .await;
    assert_eq!(status, 201, "case create: {body}");
    let case_id = body["id"].as_str().expect("id");
    assert_eq!(body["title"], "Test case");
    assert_eq!(body["status"], "draft");
    assert!(body["identifier"].as_str().unwrap().starts_with("CASE-"));

    let (status, body) = call(&app, "GET", &format!("/api/cases/{case_id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], case_id);

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/cases/{case_id}"),
        Some(json!({
            "title": "Updated title",
            "status": "in_progress"
        })),
    )
    .await;
    assert_eq!(status, 200, "case patch: {body}");
    assert_eq!(body["title"], "Updated title");
    assert_eq!(body["status"], "in_progress");

    let (status, _) = call(&app, "DELETE", &format!("/api/cases/{case_id}"), None).await;
    assert_eq!(status, 204);
}

#[tokio::test(flavor = "current_thread")]
async fn cases_list_filters_by_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::cases::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/cases",
        Some(json!({
            "company_id": company_id,
            "case_type": "support",
            "title": "Listed case"
        })),
    )
    .await;
    assert_eq!(status, 201);
    let case_id = body["id"].as_str().unwrap().to_string();

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/cases?company_id={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|c| c["id"] == case_id));
}

#[tokio::test(flavor = "current_thread")]
async fn projects_create_list_get_update_archive() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::projects::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/projects",
        Some(json!({
            "company_id": company_id,
            "name": "Demo project",
            "description": "Test description"
        })),
    )
    .await;
    assert_eq!(status, 201, "project create: {body}");
    let project_id = body["id"].as_str().expect("id");
    assert_eq!(body["name"], "Demo project");
    assert_eq!(body["status"], "backlog");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/projects?company_id={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|p| p["id"] == project_id));

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/projects/{project_id}"),
        Some(json!({
            "name": "Renamed project",
            "status": "active"
        })),
    )
    .await;
    assert_eq!(status, 200, "project patch: {body}");
    assert_eq!(body["name"], "Renamed project");
    assert_eq!(body["status"], "active");

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/projects/{project_id}"),
        None,
    )
    .await;
    assert_eq!(status, 204, "project delete: {status}");
}

#[tokio::test(flavor = "current_thread")]
async fn goals_create_get_update_delete() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::goals::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/goals",
        Some(json!({
            "company_id": company_id,
            "title": "Q4 objective",
            "description": "Top-level goal"
        })),
    )
    .await;
    assert_eq!(status, 201, "goal create: {body}");
    let goal_id = body["id"].as_str().expect("id");
    assert_eq!(body["title"], "Q4 objective");

    let (status, body) = call(&app, "GET", &format!("/api/goals/{goal_id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], goal_id);

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/goals/{goal_id}"),
        Some(json!({ "status": "in_progress" })),
    )
    .await;
    assert_eq!(status, 200, "goal patch: {body}");
    assert_eq!(body["status"], "in_progress");

    let (status, _) = call(&app, "DELETE", &format!("/api/goals/{goal_id}"), None).await;
    assert_eq!(status, 204);
}

#[tokio::test(flavor = "current_thread")]
async fn environments_create_and_list() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let app = routes::environments::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/environments",
        Some(json!({
            "name": format!("dev-env-{}", Uuid::new_v4().simple()),
            "driver": format!("driver-{}", Uuid::new_v4().simple()),
            "config": { "shell": "zsh" }
        })),
    )
    .await;
    assert_eq!(status, 201, "env create: {body}");
    let env_id = body["id"].as_str().expect("id");
    assert!(body["name"].as_str().unwrap().starts_with("dev-env-"));
    assert!(body["driver"].as_str().unwrap().starts_with("driver-"));

    let (status, body) = call(&app, "GET", "/api/environments", None).await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|e| e["id"] == env_id));
}

#[tokio::test(flavor = "current_thread")]
async fn environments_create_rejects_empty_name() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let app = routes::environments::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/environments",
        Some(json!({ "name": "", "driver": "local" })),
    )
    .await;
    assert_eq!(status, 400, "empty name: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn folders_create_list_delete_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::folders::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/folders",
        Some(json!({
            "company_id": company_id,
            "kind": "issue",
            "name": "Backlog",
            "slug": "backlog",
            "color": "#ff0000"
        })),
    )
    .await;
    assert_eq!(status, 201, "folder create: {body}");
    let folder_id = body["id"].as_str().expect("id");
    assert_eq!(body["name"], "Backlog");
    assert_eq!(body["kind"], "issue");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/folders?company_id={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|f| f["id"] == folder_id));

    let (status, _) = call(&app, "DELETE", &format!("/api/folders/{folder_id}"), None).await;
    assert_eq!(status, 204);
}
