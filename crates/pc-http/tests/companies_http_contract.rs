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
        Arc::new(WsState::new(
            realtime.clone(),
            "test".to_string(),
        )),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
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

#[tokio::test(flavor = "current_thread")]
async fn company_stats_returns_aggregates() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("stats-{id}"))
        .bind(format!("ST{}", &id.simple().to_string()[..6]))
        .execute(db.pool())
        .await
        .expect("insert company");

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (s, b) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/stats"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 200, "stats: {b}");
    assert_eq!(b["company_id"], id.to_string());
    assert!(b["issue_count"].is_number());
    assert!(b["agent_count"].is_number());
}

#[tokio::test(flavor = "current_thread")]
async fn company_timeline_returns_events() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("tl-{id}"))
        .bind(format!("TL{}", &id.simple().to_string()[..6]))
        .execute(db.pool())
        .await
        .expect("insert company");

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (s, b) = call(
        &app,
        "GET",
        &format!("/api/companies/{id}/timeline"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 200, "timeline: {b}");
    assert!(b["events"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn company_branding_patch_updates_name_and_logo() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("br-{id}"))
        .bind(format!("BR{}", &id.simple().to_string()[..6]))
        .execute(db.pool())
        .await
        .expect("insert company");

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (s, b) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{id}/branding"),
        serde_json::json!({ "name": "New Name", "logo_url": "https://example.com/logo.png" }),
    )
    .await;
    assert_eq!(s, 200, "branding: {b}");
    assert_eq!(b["name"], "New Name");
    // description should contain logo marker
    let desc = b["description"].as_str().unwrap_or("");
    assert!(
        desc.contains("logo:https://example.com/logo.png"),
        "expected logo in desc, got: {desc}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn company_export_and_import_preview() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("ex-{id}"))
        .bind(format!("EX{}", &id.simple().to_string()[..6]))
        .execute(db.pool())
        .await
        .expect("insert company");

    let app = routes::companies::router().with_state(test_state(db.clone()));

    // export preview
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/exports/preview"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 200, "export: {b}");
    assert_eq!(b["version"], "1.0");
    assert!(b["counts"].is_object());

    // import preview - valid
    let (s, b) = call(&app, "POST", &format!("/api/companies/{id}/imports/preview"),
        serde_json::json!({ "payload": { "version": "1.0", "company": { "name": "X" }, "issues": [{}, {}] } })).await;
    assert_eq!(s, 200, "import valid: {b}");
    assert_eq!(b["valid"], true);
    assert_eq!(b["would_import"]["issues"], 2);

    // import preview - invalid (missing version)
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/companies/{id}/imports/preview"),
        serde_json::json!({ "payload": { "company": { "name": "X" } } }),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(b["valid"], false);
}
