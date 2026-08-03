//! Documents CRUD 路由契约测试。

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
    .bind(format!("doc-{id}"))
    .bind(format!("DC{}", &id.simple().to_string()[..4]))
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
async fn document_crud_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::documents::router().with_state(test_state(db.clone()));

    // CREATE
    let (status, body) = call(
        &app,
        "POST",
        "/api/documents",
        Some(json!({
            "company_id": company_id,
            "title": "My Plan",
            "body": "# Goal\n\nPlan text."
        })),
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let doc_id = body["id"].as_str().expect("doc id");
    assert_eq!(body["title"].as_str().unwrap_or(""), "My Plan");

    // LIST
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/documents?company_id={company_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    let items = body.as_array().expect("array");
    assert!(items.iter().any(|d| d["id"] == doc_id), "doc should appear in list: {body}");

    // GET
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/documents/{doc_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "get: {body}");
    assert_eq!(body["latest_body"], "# Goal\n\nPlan text.");

    // UPDATE
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/documents/{doc_id}"),
        Some(json!({
            "title": "My Plan v2",
            "body": "# Goal v2\n\nUpdated."
        })),
    )
    .await;
    assert_eq!(status, 200, "update: {body}");

    // GET after update
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/documents/{doc_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["title"].as_str().unwrap_or(""), "My Plan v2");

    // DELETE
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/documents/{doc_id}"),
        None,
    )
    .await;
    assert!(status == 200 || status == 204, "delete: status={status}");

    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/documents/{doc_id}"),
        None,
    )
    .await;
    assert_eq!(status, 404, "after delete should 404");
}

#[tokio::test(flavor = "current_thread")]
async fn document_get_404_for_missing_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::documents::router().with_state(test_state(db));
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/documents/{}", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, 404);
}
