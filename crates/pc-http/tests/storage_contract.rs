//! Object storage 路由契约测试。
//! 在测试中注册一个临时 LocalDiskStorage 以覆盖 put / get / list。

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

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state_with_local_storage(db: Db, storage_root: std::path::PathBuf) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    let storage = StorageRegistry::new();
    storage
        .register(Arc::new(LocalDiskStorage::new(storage_root)))
        .expect("register local_disk storage");
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
        Arc::new(WsState {
            realtime: realtime.clone(),
            server_name: "test".into(),
        }),
        realtime,
    );
    state.storage = Arc::new(storage);
    state
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

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

#[tokio::test(flavor = "current_thread")]
async fn put_get_list_round_trip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let tmp = TempDir::new().expect("tempdir");
    let state = test_state_with_local_storage(db, tmp.path().to_path_buf());
    let app = routes::storage::router().with_state(state);

    // PUT 1: foo.txt
    let (status, body) = call(
        &app,
        "POST",
        "/api/storage/test-bucket/objects/foo.txt",
        Some(json!({
            "content_base64": b64("hello world"),
            "content_type": "text/plain"
        })),
    )
    .await;
    assert_eq!(status, 200, "put: {body}");
    assert_eq!(body["key"], "foo.txt");
    assert_eq!(body["size"], json!(11));
    assert!(body["sha256"].is_string());
    assert_eq!(body["contentType"], "text/plain");

    // PUT 2: nested/bar.txt
    let (status, body) = call(
        &app,
        "POST",
        "/api/storage/test-bucket/objects/nested/bar.txt",
        Some(json!({
            "content_base64": b64("nested content")
        })),
    )
    .await;
    assert_eq!(status, 200, "put nested: {body}");

    // GET 1
    let (status, body) = call(
        &app,
        "GET",
        "/api/storage/test-bucket/objects/foo.txt",
        None,
    )
    .await;
    assert_eq!(status, 200, "get: {body}");

    // GET missing → 404
    let (status, body) = call(
        &app,
        "GET",
        "/api/storage/test-bucket/objects/missing.txt",
        None,
    )
    .await;
    assert_eq!(status, 404, "get missing: {body}");

    // LIST under nested/
    let (status, body) = call(
        &app,
        "POST",
        "/api/storage/test-bucket/list",
        Some(json!({ "prefix": "nested/" })),
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    let items = body["items"].as_array().expect("items array");
    assert!(
        items.iter().any(|it| it["key"] == "nested/bar.txt"),
        "list should contain nested/bar.txt: {body}"
    );

    // DELETE
    let (status, _) = call(
        &app,
        "DELETE",
        "/api/storage/test-bucket/objects/foo.txt",
        None,
    )
    .await;
    assert_eq!(status, 200, "delete");

    let (status, _) = call(
        &app,
        "GET",
        "/api/storage/test-bucket/objects/foo.txt",
        None,
    )
    .await;
    assert_eq!(status, 404, "after delete");
}

#[tokio::test(flavor = "current_thread")]
async fn put_rejects_path_traversal_in_key() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let tmp = TempDir::new().expect("tempdir");
    let state = test_state_with_local_storage(db, tmp.path().to_path_buf());
    let app = routes::storage::router().with_state(state);

    // Normal key writes
    let (status, body) = call(
        &app,
        "POST",
        "/api/storage/test-bucket/objects/normal.txt",
        Some(json!({ "content_base64": b64("hi") })),
    )
    .await;
    assert_eq!(status, 200, "normal key upload: {body}");

    // Bucket name with ".." must be rejected as invalid bucket
    let (status, _) = call(
        &app,
        "POST",
        "/api/storage/bad..bucket/objects/x.txt",
        Some(json!({ "content_base64": b64("x") })),
    )
    .await;
    assert!(
        status >= 400,
        "expected rejection for bucket with traversal chars: status={status}"
    );
}
