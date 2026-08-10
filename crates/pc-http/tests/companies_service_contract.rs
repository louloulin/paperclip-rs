//! R590: CompanyService 端到端 HTTP contract 测试。
//!
//! 验证 list / get / create / update / archive / remove 端点通过 service 层运行。

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

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn insert_company(db: &Db, suffix: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R590-{suffix}-{id}"))
    .bind(format!("R5{}", &id.simple().to_string()[..5]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn cleanup(db: &Db, id: Uuid) {
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_list_via_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = insert_company(&db, "list").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(&app, "GET", "/api/companies", json!({})).await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert!(
        arr.iter().any(|c| c["id"].as_str() == Some(id.to_string().as_str())),
        "created company should appear in list"
    );

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_get_one_via_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = insert_company(&db, "get").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["id"].as_str(), Some(id.to_string().as_str()));

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_get_returns_404_for_missing() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::companies::router().with_state(test_state(db));

    let (status, _body) = call(
        &app,
        "GET",
        &format!("/api/companies/{}", Uuid::new_v4()),
        json!({}),
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_create_via_service_returns_201() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let name = format!("R590-Create-{}", Uuid::new_v4());
    let (status, body) = call(
        &app,
        "POST",
        "/api/companies",
        json!({
            "name": name,
            "description": "test create via service",
        }),
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let id_str = body["id"].as_str().expect("id");
    let id = Uuid::parse_str(id_str).expect("uuid");

    // owner membership should exist
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM company_memberships WHERE company_id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert!(count.0 >= 1, "owner membership should exist");

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_create_rejects_empty_name() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::companies::router().with_state(test_state(db));

    let (status, body) = call(
        &app,
        "POST",
        "/api/companies",
        json!({ "name": "   ", "description": null }),
    )
    .await;
    assert_eq!(status, 400, "empty name should reject: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_update_via_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = insert_company(&db, "update").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{id}"),
        json!({ "description": "via-service" }),
    )
    .await;
    assert_eq!(status, 200, "update: {body}");
    assert_eq!(body["description"].as_str(), Some("via-service"));

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_update_rejects_invalid_status() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = insert_company(&db, "status").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{id}"),
        json!({ "status": "bogus" }),
    )
    .await;
    assert_eq!(status, 400, "invalid status should reject: {body}");

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_update_returns_404_for_missing() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::companies::router().with_state(test_state(db));

    let (status, _body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{}", Uuid::new_v4()),
        json!({ "description": "x" }),
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_archive_via_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = insert_company(&db, "archive").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/archive"),
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "archive: {body}");
    assert_eq!(body["status"].as_str(), Some("archived"));

    // Verify in DB
    let row: (String,) =
        sqlx::query_as("SELECT status FROM companies WHERE id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("fetch");
    assert_eq!(row.0, "archived");

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_remove_via_service_returns_204() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = insert_company(&db, "remove").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, _body) = call(
        &app,
        "DELETE",
        &format!("/api/companies/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, 204);

    // Verify gone
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM companies WHERE id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(count.0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn r590_companies_remove_missing_returns_404() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::companies::router().with_state(test_state(db));

    let (status, _body) = call(
        &app,
        "DELETE",
        &format!("/api/companies/{}", Uuid::new_v4()),
        json!({}),
    )
    .await;
    assert_eq!(status, 404);
}
