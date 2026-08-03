//! Plugin UI static asset delivery 路由契约测试。

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
use serde_json::Value;
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

async fn call(app: &axum::Router, method: &str, path: &str) -> (u16, String) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
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
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, text)
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_ui_404_for_unknown_plugin() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    let unknown = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "GET",
        &format!("/_plugins/{unknown}/ui/index.html"),
    )
    .await;
    assert_eq!(status, 404, "should 404 for unknown plugin: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_ui_404_for_malformed_plugin_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    let (status, _) = call(&app, "GET", "/_plugins/not-a-uuid/ui/index.html").await;
    assert_eq!(status, 404, "should 404 for malformed id");
}
