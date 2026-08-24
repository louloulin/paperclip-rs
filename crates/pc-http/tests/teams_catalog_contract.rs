//! Teams catalog 路由契约测试。

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

async fn ensure_team_installs_schema(db: &Db) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS team_installs (
            id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
            company_id uuid NOT NULL,
            catalog_id text NOT NULL,
            status text DEFAULT 'queued' NOT NULL,
            snapshot jsonb NOT NULL DEFAULT '{}'::jsonb,
            installed_at timestamptz DEFAULT now() NOT NULL,
            created_at timestamptz DEFAULT now() NOT NULL,
            updated_at timestamptz DEFAULT now() NOT NULL
        )",
    )
    .execute(db.pool())
    .await
    .expect("create team_installs");
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS team_installs_company_catalog_uq          ON team_installs USING btree (company_id, catalog_id)",
    )
    .execute(db.pool())
    .await
    .expect("create uq idx");
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
async fn teams_catalog_list_returns_items_from_embedded_catalog() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::teams_catalog::router().with_state(test_state(db));
    let (status, body) = call(&app, "GET", "/api/teams/catalog", None).await;
    assert_eq!(status, 200, "list: {body}");
    let items = body["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "should have at least one bundled team");
    // Each item has key/name/etc
    let first = &items[0];
    assert!(
        first["key"].is_string() || first["id"].is_string(),
        "team must have key/id: {first}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn teams_catalog_detail_404_for_missing_team() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::teams_catalog::router().with_state(test_state(db));
    let (status, _) = call(&app, "GET", "/api/teams/catalog/does-not-exist-xyz", None).await;
    assert_eq!(status, 404, "unknown catalog should 404");
}

#[tokio::test(flavor = "current_thread")]
async fn teams_install_then_list_installed_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    ensure_team_installs_schema(&db).await;
    let app = routes::teams_catalog::router().with_state(test_state(db.clone()));

    // Pick the first catalog key to install
    let (_, catalog_body) = call(&app, "GET", "/api/teams/catalog", None).await;
    let raw_key = catalog_body["items"][0]["key"]
        .as_str()
        .or_else(|| catalog_body["items"][0]["id"].as_str())
        .map(str::to_string)
        .expect("catalog key");
    let key = raw_key.clone();
    let key_enc: String = raw_key
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                (b as char).to_string()
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect();

    // Install
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/teams/catalog/{key_enc}/install"),
        None,
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "install status={status}: {body}"
    );
    assert!(
        body["status"].as_str().unwrap().contains("queued"),
        "status={}; body={}",
        body["status"],
        body
    );
    assert_eq!(body["catalogId"], key);

    // List installed
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/teams/catalog/installed"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let items = body["items"].as_array().expect("installed items");
    assert!(
        items.iter().any(|it| it["catalogId"] == key),
        "installed list should include the freshly installed team: {body}"
    );

    // Uninstall
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/teams/catalog/{key_enc}/uninstall"),
        None,
    )
    .await;
    assert!((200..300).contains(&status), "uninstall status={status}");

    // Verify removed / status updated
    let (_, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/teams/catalog/installed"),
        None,
    )
    .await;
    let items = body["items"].as_array().expect("installed items");
    // After uninstall the row might still exist but with different status OR be removed
    assert!(
        items.iter().all(|it| it["catalogId"] != key),
        "the team should no longer be installed: {body}"
    );
}
