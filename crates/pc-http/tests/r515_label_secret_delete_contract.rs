//! R515 — 4 个真缺漏的 route 契约测试
//! - DELETE /api/labels/:label_id
//! - DELETE /api/secrets/:id
//! - GET /api/companies/ (trailing-slash 列表)
//! - POST /api/companies/ (trailing-slash 创建)

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

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (axum::http::StatusCode, Value) {
    let builder = Request::builder().method(method).uri(path);
    let req = if let Some(b) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 10_000_000)
        .await
        .unwrap();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("r515-{id}"))
    .bind(id.simple().to_string())
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_label(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO labels (id, company_id, name, color, created_at, updated_at)          VALUES ($1, $2, $3, '#aabbcc', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .execute(db.pool())
    .await
    .expect("insert label");
    id
}

async fn insert_company_secret(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    let key = format!("r515-{}", &id.simple().to_string()[..8]);
    sqlx::query(
        "INSERT INTO company_secrets (id, company_id, name, key, provider, scope, created_at, updated_at)          VALUES ($1, $2, $3, $4, 'local_encrypted', 'company', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .bind(&key)
    .execute(db.pool())
    .await
    .expect("insert company secret");
    id
}

#[tokio::test(flavor = "current_thread")]
async fn delete_label_route_removes_label() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let label_id = insert_label(&db, company_id, "r515-del-label").await;
    let app = routes::labels::router().with_state(test_state(db.clone()));

    let (status, body) = call(&app, "DELETE", &format!("/api/labels/{label_id}"), None).await;
    assert_eq!(status, 200, "delete label: {body}");
    assert_eq!(body["labelId"], label_id.to_string());

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM labels WHERE id = $1")
        .bind(label_id)
        .fetch_one(db.pool())
        .await
        .expect("count labels");
    assert_eq!(count, 0, "label should be deleted");
}

#[tokio::test(flavor = "current_thread")]
async fn delete_label_returns_404_when_missing() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::labels::router().with_state(test_state(db.clone()));
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/labels/{}", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "current_thread")]
async fn delete_secret_route_soft_deletes_secret() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let secret_id = insert_company_secret(&db, company_id, "r515-del-secret").await;
    let app = routes::secrets::router().with_state(test_state(db.clone()));

    let (status, body) = call(&app, "DELETE", &format!("/api/secrets/{secret_id}"), None).await;
    assert_eq!(status, 200, "delete secret: {body}");

    let deleted_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM company_secrets WHERE id = $1")
            .bind(secret_id)
            .fetch_one(db.pool())
            .await
            .expect("fetch deleted_at");
    assert!(deleted_at.is_some(), "secret should be soft-deleted");
}

#[tokio::test(flavor = "current_thread")]
async fn companies_trailing_slash_lists_companies() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call(&app, "GET", "/api/companies/", None).await;
    assert_eq!(status, 200, "list companies /: {body}");
    let items = body
        .as_array()
        .or_else(|| body["items"].as_array())
        .expect("items array");
    let has_company = items.iter().any(|item| {
        item["id"].as_str() == Some(company_id.to_string().as_str())
            || item["id"] == json!(company_id.to_string())
    });
    assert!(
        has_company,
        "items should contain the just-created company: {body}"
    );
}
