//! Company assets (image/logo) 路由契约测试。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use base64::Engine;
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
use pc_storage::{LocalDiskStorage, StorageRegistry};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state_with_storage(db: Db, storage_root: std::path::PathBuf) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    let storage = StorageRegistry::new();
    storage
        .register(Arc::new(LocalDiskStorage::new(storage_root)))
        .expect("register local_disk");
    let mut state = AppState::new(
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
    );
    state.storage = Arc::new(storage);
    state
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("asset-{id}"))
    .bind(format!("AT{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
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
async fn upload_image_to_real_company_succeeds() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let tmp = TempDir::new().expect("tempdir");
    let state = test_state_with_storage(db, tmp.path().to_path_buf());
    let app = routes::assets::router().with_state(state);
    let payload = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl21bKAAAAA1BMVEX/AAAZ4gk3AAAAAXRSTlPM0jRW/QAAAApJREFUCNdjYAAAAAIAAeIhvDMAAAAASUVORK5CYII=";
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/assets/images"),
        Some(json!({
            "content_base64": payload,
            "content_type": "image/png",
            "filename": "test.png"
        })),
    )
    .await;
    assert_eq!(status, 201, "upload image: {body}");
    let asset_id = body["id"].as_str().expect("asset id");
    assert_eq!(body["companyId"], company_id.to_string());
    assert_eq!(body["kind"], "image");
    assert!(body["key"].as_str().unwrap().contains(&company_id.to_string()));
    assert!(body["url"].as_str().unwrap().contains(asset_id));

    // Asset content retrieval
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/assets/{asset_id}/content"),
        None,
    )
    .await;
    assert_eq!(status, 200, "asset content: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn asset_content_404_for_unknown_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let tmp = TempDir::new().expect("tempdir");
    let state = test_state_with_storage(db, tmp.path().to_path_buf());
    let app = routes::assets::router().with_state(state);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/assets/{}/content", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, 404, "should 404 for unknown asset");
}
