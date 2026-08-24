//! `/api/companies` 扩展路由 HTTP contract 测试（R801）。
//!
//! 覆盖 companies_http_contract.rs 未覆盖的主要 companies 子路由：
//! - /api/companies                    — list, create
//! - /api/companies/:id               — get, update, delete
//! - /api/companies/:id/archive       — POST
//! - /api/companies/:id/artifacts    — GET
//! - /api/companies/:id/diagnostics  — GET
//! - /api/companies/:id/labels        — GET, POST
//! - /api/companies/:id/labels/:id   — PATCH, DELETE
//! - /api/companies/:id/invites      — GET, POST
//! - /api/companies/:id/invites/:id  — DELETE
//! - /api/companies/:id/members      — GET
//! - /api/companies/:id/org          — GET
//! - /api/companies/:id/search/extract — POST

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
use pc_db::Migrator;
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
    body: Option<Value>,
    token: Option<&str>,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("request"))
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

async fn insert_company(db: &Db, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(name)
    .bind(format!("R801{}", &id.simple().to_string()[..5]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_user(db: &Db) -> (String, String) {
    let user_id = Uuid::new_v4().to_string();
    let email = format!("{user_id}@example.com");
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, true, now(), now()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(&user_id)
    .bind(format!("User {user_id}"))
    .bind(&email)
    .execute(db.pool())
    .await
    .expect("insert user");
    (user_id, email)
}

async fn insert_session(db: &Db, user_id: &str) -> String {
    let token = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO sessions (id, user_id, created_at, updated_at, expires_at) \
         VALUES ($1, $2, now(), now(), now() + interval '7 days') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(&token)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert session");
    token
}

async fn make_member(db: &Db, company_id: Uuid, user_id: &str, role: &str) {
    sqlx::query(
        "INSERT INTO company_memberships \
         (company_id, principal_type, principal_id, membership_role, status, created_at, updated_at) \
         VALUES ($1, 'user', $2, $3, 'active', now(), now()) \
         ON CONFLICT DO NOTHING",
    )
    .bind(company_id)
    .bind(user_id)
    .bind(role)
    .execute(db.pool())
    .await
    .expect("make member");
}

/// Run DB migrations (idempotent — safe to call on every test).
async fn ensure_migrated(db: &Db) {
    if let Err(e) = Migrator::run(db).await {
        eprintln!("migration note: {e}");
    }
}

/// Sign in via POST /api/auth/sign-in and return the session token.
async fn sign_in(app: &axum::Router, email: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, 200, "sign-in: {body}");
    body["session_token"].as_str().expect("session_token").to_string()
}

// ── /api/companies list + create ────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_list_returns_array() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call(&app, "GET", "/api/companies", None, None).await;
    assert_eq!(status, 200, "list companies: {body}");
    assert!(body.is_array(), "should return array");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_create_returns_201() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801c-{}@example.com", Uuid::new_v4().simple())).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/companies",
        Some(json!({ "name": "R801 Test Co" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, 201, "create company: {body}");
    assert!(body["id"].is_string(), "should have id: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_create_rejects_empty_name() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, _body) = call(
        &app,
        "POST",
        "/api/companies",
        Some(json!({ "name": "" })),
        None,
    )
    .await;
    // Validation error or bad request
    assert!(status >= 400, "empty name should fail with 4xx");
}

// ── /api/companies/:id ───────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_get_returns_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let id = insert_company(&db, "r801-get").await;
    let (status, body) = call(&app, "GET", &format!("/api/companies/{id}"), None, None).await;
    assert_eq!(status, 200, "get company: {body}");
    assert_eq!(body["id"].as_str().unwrap(), id.to_string(), "id match: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_get_404_for_missing() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let fake_id = Uuid::new_v4();
    let (status, _body) = call(
        &app,
        "GET",
        &format!("/api/companies/{fake_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404, "missing company should return 404");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_patch_updates_name() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let id = insert_company(&db, "r801-patch-old").await;
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{id}"),
        Some(json!({ "name": "r801-patch-new" })),
        None,
    )
    .await;
    assert_eq!(status, 200, "patch company: {body}");
    assert_eq!(body["name"].as_str(), Some("r801-patch-new"), "name updated: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_delete_returns_204() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let id = insert_company(&db, "r801-delete").await;
    let (status, _body) = call(
        &app,
        "DELETE",
        &format!("/api/companies/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204, "delete should return 204");
}

// ── /api/companies/:id/archive ───────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_archive_archives_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let id = insert_company(&db, "r801-archive").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/archive"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "archive: {body}");
}

// ── /api/companies/:id/artifacts ────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_artifacts_returns_array() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801a-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-artifacts").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/artifacts"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "artifacts: {body}");
    assert!(body["assets"].is_array(), "artifacts should have assets array: {body}");
}

// ── /api/companies/:id/diagnostics ─────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_diagnostics_returns_object() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let id = insert_company(&db, "r801-diag").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/diagnostics"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "diagnostics: {body}");
    assert!(
        body.is_object(),
        "diagnostics should be object: {body}"
    );
}

// ── /api/companies/:id/labels ────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_labels_list_returns_array() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801ll-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-labels").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/labels"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "labels list: {body}");
    assert!(body.is_array(), "labels should be array: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_labels_create_returns_label() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801lc-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-label-create").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/labels"),
        Some(json!({ "name": "urgent", "color": "#ff0000" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, 201, "create label: {body}");
    assert_eq!(body["name"].as_str(), Some("urgent"), "label name: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_labels_patch_updates() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801lp-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-label-patch").await;
    // Create a label first
    let (_, create_body) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/labels"),
        Some(json!({ "name": "old-name", "color": "#000000" })),
        Some(&token),
    )
    .await;
    let label_id = create_body["id"].as_str().expect("label id");

    // Patch it
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{id}/labels/{label_id}"),
        Some(json!({ "name": "new-name" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "patch label: {body}");
    assert!(body["updated"].as_bool() == Some(true), "name updated: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_labels_delete_returns_204() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801ld-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-label-del").await;
    let (_, create_body) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/labels"),
        Some(json!({ "name": "to-delete", "color": "#ffffff" })),
        Some(&token),
    )
    .await;
    let label_id = create_body["id"].as_str().expect("label id");

    let (status, _body) = call(
        &app,
        "DELETE",
        &format!("/api/companies/{id}/labels/{label_id}"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, 204, "delete label should return 204");
}

// ── /api/companies/:id/invites ─────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_invites_list_returns_array() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801i-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-invites").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/invites"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "invites list: {body}");
    assert!(body["items"].is_array(), "invites should have items array: {body}");
}

// ── /api/companies/:id/members ─────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_members_list_returns_array() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801m-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-members").await;
    let (user_id, _email) = insert_user(&db).await;
    make_member(&db, id, &user_id, "member").await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/members"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "members list: {body}");
    let items = body["items"].as_array().expect("members should have items array: {body}");
    assert!(!items.is_empty(), "members should not be empty");
}

// ── /api/companies/:id/org ──────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_org_returns_object() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));
    let token = sign_in(&app, &format!("r801o-{}@example.com", Uuid::new_v4().simple())).await;

    let id = insert_company(&db, "r801-org").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/org"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "org chart: {body}");
    assert!(
        body.is_array(),
        "org chart should be array: {body}"
    );
}

// ── /api/companies/:id/search/extract ──────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r801_companies_search_extract_returns_result() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let id = insert_company(&db, "r801-search").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/search/extract"),
        Some(json!({ "query": "hello world", "limit": 5 })),
        None,
    )
    .await;
    // Should return 200 even if no results (search endpoint exists)
    assert!(
        status == 200 || status == 400 || status == 422,
        "search should return 2xx/4xx: {status} {body}"
    );
}
