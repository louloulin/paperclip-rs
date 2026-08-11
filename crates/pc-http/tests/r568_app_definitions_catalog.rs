//! R568 — R-INTEGRATION-8: pc-app-definitions → pc-http `/tools/catalog` route.
//!
//! Verifies that GET `/api/companies/:company_id/tools/catalog` returns the
//! static connectable-app catalog (powered by `pc-app-definitions`).

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
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("r568-catalog-{company_id}"))
    .bind(format!("R5{}", &company_id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    company_id
}

async fn insert_user(db: &Db, user_id: &str) {
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, true, now(), now()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind(format!("User {user_id}"))
    .bind(format!("{user_id}@example.com"))
    .execute(db.pool())
    .await
    .expect("insert user");
}

async fn insert_session(db: &Db, user_id: &str) -> String {
    let token = format!("sess-r568-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO session (id, expires_at, token, created_at, updated_at, user_id) \
         VALUES ($1, now() + interval '1 hour', $2, now(), now(), $3) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("sess-r568-{user_id}-{}", Uuid::new_v4().simple()))
    .bind(&token)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert session");
    token
}

async fn setup_session(db: &Db, user_id: &str) -> String {
    insert_user(db, user_id).await;
    insert_session(db, user_id).await
}

async fn call_get(app: &axum::Router, path: &str, session_token: &str) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .header("authorization", format!("Bearer {session_token}"))
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

async fn build_app() -> (axum::Router, Db) {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let router = routes::tool_access::router().with_state(state);
    (router, db)
}

#[tokio::test(flavor = "current_thread")]
async fn r568_catalog_returns_seven_connectable_apps() {
    let (app, db) = build_app().await;
    let company_id = insert_company(&db).await;
    let session = setup_session(&db, "r568-catalog").await;
    let (status, body) = call_get(
        &app,
        &format!("/api/companies/{company_id}/tools/catalog"),
        &session,
    )
    .await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    let apps = body
        .get("apps")
        .and_then(|v| v.as_array())
        .expect("apps array");
    assert_eq!(apps.len(), 7, "expected 7 connectable apps: {apps:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn r568_catalog_includes_required_slugs() {
    let (app, db) = build_app().await;
    let company_id = insert_company(&db).await;
    let session = setup_session(&db, "r568-catalog-slugs").await;
    let (status, body) = call_get(
        &app,
        &format!("/api/companies/{company_id}/tools/catalog"),
        &session,
    )
    .await;
    assert_eq!(status, 200, "got {status}: {body}");
    let apps = body.get("apps").and_then(|v| v.as_array()).expect("apps");
    let slugs: Vec<&str> = apps
        .iter()
        .filter_map(|a| a.get("slug").and_then(|s| s.as_str()))
        .collect();
    for required in [
        "zapier",
        "github",
        "slack",
        "notion",
        "linear",
        "google-sheets",
        "context7",
    ] {
        assert!(
            slugs.contains(&required),
            "missing required slug `{required}` in {slugs:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r568_catalog_entries_have_ownership_availability() {
    let (app, db) = build_app().await;
    let company_id = insert_company(&db).await;
    let session = setup_session(&db, "r568-catalog-ownership").await;
    let (status, body) = call_get(
        &app,
        &format!("/api/companies/{company_id}/tools/catalog"),
        &session,
    )
    .await;
    assert_eq!(status, 200);
    let apps = body.get("apps").and_then(|v| v.as_array()).expect("apps");
    for app in apps {
        let slug = app.get("slug").and_then(|v| v.as_str()).unwrap_or("?");
        let ownership = app
            .get("ownershipAvailability")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| panic!("ownershipAvailability missing for `{slug}`"));
        // customer + dcr should be true (matches pc-app-definitions
        // default_ownership_availability); platform_* should be false.
        assert_eq!(
            ownership.get("customer").and_then(|v| v.as_bool()),
            Some(true),
            "{slug} customer"
        );
        assert_eq!(
            ownership.get("dcr").and_then(|v| v.as_bool()),
            Some(true),
            "{slug} dcr"
        );
        assert_eq!(
            ownership.get("platform_shared").and_then(|v| v.as_bool()),
            Some(false),
            "{slug} platform_shared"
        );
        assert_eq!(
            ownership
                .get("platform_provisioned")
                .and_then(|v| v.as_bool()),
            Some(false),
            "{slug} platform_provisioned"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r568_catalog_entries_have_label_and_category() {
    let (app, db) = build_app().await;
    let company_id = insert_company(&db).await;
    let session = setup_session(&db, "r568-catalog-labels").await;
    let (status, body) = call_get(
        &app,
        &format!("/api/companies/{company_id}/tools/catalog"),
        &session,
    )
    .await;
    assert_eq!(status, 200);
    let apps = body.get("apps").and_then(|v| v.as_array()).expect("apps");
    for app in apps {
        let slug = app.get("slug").and_then(|v| v.as_str()).unwrap_or("?");
        let label = app
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("label missing for `{slug}`"));
        let category = app
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("category missing for `{slug}`"));
        assert!(!label.is_empty(), "{slug} empty label");
        assert!(!category.is_empty(), "{slug} empty category");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r568_catalog_company_id_echoed() {
    let (app, db) = build_app().await;
    let company_id = insert_company(&db).await;
    let session = setup_session(&db, "r568-catalog-echo").await;
    let (status, body) = call_get(
        &app,
        &format!("/api/companies/{company_id}/tools/catalog"),
        &session,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body.get("companyId").and_then(|v| v.as_str()),
        Some(company_id.to_string().as_str())
    );
}
