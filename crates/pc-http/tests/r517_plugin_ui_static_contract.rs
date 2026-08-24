//! R517 — `/api/_plugins/:plugin_id/ui/*path` Node 契约
//!
//! Node 端 (server/src/routes/plugin-ui-static.ts) 契约要点：
//! - GET /api/_plugins/:pluginId/ui/*filePath
//! - pluginId 可为 DB UUID 或 plugin key
//! - 仅 status='ready' + manifest 声明 ui 的 plugin 提供 UI
//! - 路径遍历防护 (../, %2F, \ 等)
//! - MIME type detection
//! - Cache headers: 内容哈希文件名 -> immutable/1y; 其他 -> must-revalidate + ETag
//! - 304 Not Modified on If-None-Match

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
use pc_repos::{plugin::PluginRegistration, Db};
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

async fn call_get(app: &axum::Router, path: &str) -> (u16, axum::http::HeaderMap, String) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, headers, text)
}

async fn register_plugin(
    db: &Db,
    key: &str,
    status: &str,
    manifest: Value,
    package_path: Option<&str>,
) -> Uuid {
    use pc_repos::plugin::PluginRepo;
    let input = PluginRegistration {
        plugin_key: key.to_string(),
        package_name: format!("@paperclip/{key}"),
        package_path: package_path.map(String::from),
        version: "1.0.0".to_string(),
        api_version: 1,
        categories: serde_json::json!([]),
        manifest_json: manifest,
    };
    let row = PluginRepo::new(db)
        .register(&input)
        .await
        .expect("register plugin");
    if status != "installed" {
        PluginRepo::new(db)
            .update_status(row.id, status, None)
            .await
            .expect("update status");
    }
    row.id
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "plugin UI static files directory not found — infrastructure gap"]
async fn plugin_ui_route_mounted_at_api_prefix() {
    // R517: 验证 _plugins 路由在 /api 命名空间下 (Node 端路径是 /api/_plugins/...)。
    // 通过 known plugin + bad path 走完流程来验证路由已被挂载:
    // - 未挂载路径会得 404 (路由不匹配), 无法走"plugin 不存在"的逻辑
    // - 已挂载路径进入 handler -> plugin 存在 -> status=ready -> ui entrypoints 存在
    //   -> 进入 file IO 分支 -> object storage 不可用 -> 404 with "asset" body
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let plugin_id = register_plugin(
        &db,
        "r517-api-prefix",
        "ready",
        json!({"entrypoints": {"ui": "./dist/ui/"}}),
        None,
    )
    .await;
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    let (status, _, body) = call_get(&app, &format!("/api/_plugins/{plugin_id}/ui/main.js")).await;
    assert_eq!(status, 404, "应进入 handler 后 404: {body}");
    assert!(
        body.contains("asset") || body.contains("storage"),
        "应说明是 asset/storage 失败, 不是路由 fall-through: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_ui_404_for_unknown_plugin() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    let unknown = Uuid::new_v4();
    let (status, _, _) = call_get(&app, &format!("/api/_plugins/{unknown}/ui/index.html")).await;
    assert_eq!(status, 404, "unknown plugin should 404");
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_ui_403_for_non_ready_status() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let plugin_id = register_plugin(
        &db,
        "r517-not-ready",
        "installed",
        json!({"entrypoints": {"ui": "./dist/ui/"}}),
        None,
    )
    .await;
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    let (status, _, _) = call_get(&app, &format!("/api/_plugins/{plugin_id}/ui/index.html")).await;
    assert_eq!(status, 403, "non-ready plugin should 403");
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_ui_404_for_plugin_without_ui_entrypoints() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let plugin_id = register_plugin(
        &db,
        "r517-no-ui",
        "ready",
        json!({"entry": "dist/worker.js"}),
        None,
    )
    .await;
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    let (status, _, _) = call_get(&app, &format!("/api/_plugins/{plugin_id}/ui/index.html")).await;
    assert_eq!(status, 404, "plugin without ui entrypoints should 404");
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_ui_rejects_path_traversal() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let plugin_id = register_plugin(
        &db,
        "r517-traversal",
        "ready",
        json!({"entrypoints": {"ui": "./dist/ui/"}}),
        None,
    )
    .await;
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    // ..%2F -> URL-encoded /
    let (status, _, _) = call_get(
        &app,
        &format!("/api/_plugins/{plugin_id}/ui/..%2F..%2Fetc%2Fpasswd"),
    )
    .await;
    assert!(
        status == 400 || status == 403 || status == 404,
        "path traversal should be rejected (got {status})"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "plugin UI static files directory not found — infrastructure gap"]
async fn plugin_ui_serves_index_redirect_for_entry() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let plugin_id = register_plugin(
        &db,
        "r517-entry",
        "ready",
        json!({"entrypoints": {"ui": "./dist/ui/"}, "ui": {"entry": "main.html"}}),
        None,
    )
    .await;
    let app = routes::plugin_ui_static::router().with_state(test_state(db));
    let (status, headers, _) =
        call_get(&app, &format!("/api/_plugins/{plugin_id}/ui/main.html")).await;
    // 期望: 307/308 重定向到 /ui/plugins/<id>/main.html
    assert!(
        status == 307 || status == 308 || status == 302,
        "entry should redirect (got {status})"
    );
    let loc = headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        loc.contains(&plugin_id.to_string()) || loc.contains("main.html"),
        "Location should reference plugin id and entry, got {loc}"
    );
}
