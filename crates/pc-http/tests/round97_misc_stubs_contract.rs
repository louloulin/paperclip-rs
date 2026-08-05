//! Integration tests for Round 97:
//! 修复 tool_gateway.rs / adapters.rs / workspace_runtime_service_authz.rs 中
//! 引用不存在表的内联 SQL。
//!
//! 涉及表：
//! - tool_mcp_gateway_tools（tool_gateway.rs × 2）
//! - tool_gateway_runtime_slots（tool_gateway.rs × 3）
//! - adapter_plugins（adapters.rs × 5）
//! - workspace_runtime_service_overrides（workspace_runtime_service_authz.rs × 1）

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
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
        pc_http::state::RuntimeHandles {
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

// =====================================================================
// tool_gateway.rs: tool_mcp_gateway_tools + tool_gateway_runtime_slots
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_list_gateway_tools_returns_deprecated_empty() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "GET",
        "/api/tool-gateway/tools",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn http_gateway_mcp_get_returns_deprecated_empty() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let gateway_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/tool-mcp-gateways/{gateway_id}/tools"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn http_list_runtime_slots_returns_deprecated_empty() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "GET",
        "/api/tool-gateway/runtime-slots",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn http_restart_stop_runtime_slot_return_deprecated_stubs() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let slot_id = Uuid::new_v4();
    let (s1, b1) = call(
        &app,
        "POST",
        &format!("/api/tool-gateway/runtime-slots/{slot_id}/restart"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s1, 200);
    assert_eq!(b1["status"], "restarting");
    assert_eq!(b1["deprecated"], true);

    let (s2, b2) = call(
        &app,
        "POST",
        &format!("/api/tool-gateway/runtime-slots/{slot_id}/stop"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s2, 200);
    assert_eq!(b2["status"], "stopped");
    assert_eq!(b2["deprecated"], true);
}

// =====================================================================
// adapters.rs: adapter_plugins
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_install_adapter_returns_queued_without_db_write() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "POST",
        "/api/adapters/install",
        serde_json::json!({"packageName": "test-pkg", "version": "1.0.0"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "queued");
    // 验证没有写入 adapter_plugins（因为表不存在）
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM information_schema.tables WHERE table_name = 'adapter_plugins'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 0, "table must not exist; stub must not create it");
}

#[tokio::test(flavor = "current_thread")]
async fn http_reinstall_adapter_returns_queued() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "POST",
        "/api/adapters/test-type/reinstall",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["type"], "test-type");
    assert_eq!(body["status"], "queued");
}

#[tokio::test(flavor = "current_thread")]
async fn http_patch_adapter_returns_disabled_flag() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "PATCH",
        "/api/adapters/test-type",
        serde_json::json!({"disabled": true}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["disabled"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn http_remove_adapter_returns_removed_false() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "DELETE",
        "/api/adapters/test-type",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["removed"], false, "stub never removed anything");
}

#[tokio::test(flavor = "current_thread")]
async fn http_override_adapter_returns_paused_flag() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let (status, body) = call(
        &app,
        "POST",
        "/api/adapters/test-type/override",
        serde_json::json!({"paused": true}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["paused"], true);
}

// =====================================================================
// workspace_runtime_service_authz.rs: workspace_runtime_service_overrides
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_workspace_runtime_service_authz_returns_empty_overrides() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let workspace_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/workspaces/{workspace_id}/runtime-service-authz"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["services"].as_array().unwrap().len(), 0);
    assert_eq!(body["workspaceId"], serde_json::json!(workspace_id));
}
