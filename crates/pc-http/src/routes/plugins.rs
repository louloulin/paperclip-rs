//! 插件管理：list、install、enable/disable、bridge、tools。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/examples", get(plugin_examples))
        .route("/api/plugins/ui-contributions", get(ui_contributions))
        .route("/api/plugins/tools", get(list_plugin_tools))
        .route("/api/plugins/tools/execute", post(execute_plugin_tool))
        .route("/api/plugins/install", post(install_plugin))
        .route("/api/plugins/:plugin_id/bridge/data", post(bridge_data))
        .route("/api/plugins/:plugin_id/bridge/action", post(bridge_action))
        .route("/api/plugins/:plugin_id/data/:key", post(plugin_data))
        .route("/api/plugins/:plugin_id/actions/:key", post(plugin_action))
        .route(
            "/api/plugins/:plugin_id/bridge/stream/:channel",
            get(bridge_stream),
        )
        .route(
            "/api/plugins/:plugin_id",
            get(get_plugin).delete(delete_plugin),
        )
        .route("/api/plugins/:plugin_id/enable", post(enable_plugin))
        .route("/api/plugins/:plugin_id/disable", post(disable_plugin))
        .route("/api/plugins/:plugin_id/health", get(plugin_health))
        .route("/api/plugins/:plugin_id/logs", get(plugin_logs))
        .route("/api/plugins/:plugin_id/upgrade", post(upgrade_plugin))
        .route("/api/plugins/:plugin_id/config", get(plugin_config))
}

async fn list_plugins(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn plugin_examples(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn ui_contributions(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn list_plugin_tools(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct ToolExecBody {
    name: Option<String>,
    arguments: Option<Value>,
}

async fn execute_plugin_tool(
    State(_s): State<AppState>,
    Json(body): Json<ToolExecBody>,
) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({ "result": null, "error": "plugin tools not implemented in Rust build yet" })),
    )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct InstallBody {
    name: Option<String>,
    version: Option<String>,
}

async fn install_plugin(
    State(_s): State<AppState>,
    Json(_body): Json<InstallBody>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "install-queued" })),
    )
}

async fn bridge_data(
    State(_s): State<AppState>,
    Path((plugin_id,)): Path<(String,)>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let _ = plugin_id;
    Json(json!({ "data": null }))
}

async fn bridge_action(
    State(_s): State<AppState>,
    Path((plugin_id,)): Path<(String,)>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let _ = plugin_id;
    Json(json!({ "ok": false, "message": "plugin bridge not implemented" }))
}

async fn plugin_data(
    State(_s): State<AppState>,
    Path((plugin_id, key)): Path<(String, String)>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let _ = plugin_id;
    let _ = key;
    Json(json!({ "ok": false }))
}

async fn plugin_action(
    State(_s): State<AppState>,
    Path((plugin_id, key)): Path<(String, String)>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let _ = plugin_id;
    let _ = key;
    Json(json!({ "ok": false }))
}

async fn bridge_stream(
    State(_s): State<AppState>,
    Path((plugin_id, channel)): Path<(String, String)>,
) -> impl IntoResponse {
    let _ = plugin_id;
    let _ = channel;
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        String::new(),
    )
}

async fn get_plugin(State(_s): State<AppState>, Path(plugin_id): Path<String>) -> Json<Value> {
    Json(json!({ "id": plugin_id }))
}

async fn delete_plugin(
    State(_s): State<AppState>,
    Path(plugin_id): Path<String>,
) -> impl IntoResponse {
    let _ = plugin_id;
    (StatusCode::NO_CONTENT, Json(json!({ "deleted": true })))
}

async fn enable_plugin(
    State(_s): State<AppState>,
    Path(plugin_id): Path<String>,
) -> impl IntoResponse {
    let _ = plugin_id;
    (
        StatusCode::OK,
        Json(json!({ "id": plugin_id, "enabled": true })),
    )
}

async fn disable_plugin(
    State(_s): State<AppState>,
    Path(plugin_id): Path<String>,
) -> impl IntoResponse {
    let _ = plugin_id;
    (
        StatusCode::OK,
        Json(json!({ "id": plugin_id, "enabled": false })),
    )
}

async fn plugin_health(State(_s): State<AppState>, Path(plugin_id): Path<String>) -> Json<Value> {
    let _ = plugin_id;
    Json(json!({ "status": "unknown" }))
}

async fn plugin_logs(
    State(_s): State<AppState>,
    Path(plugin_id): Path<String>,
) -> impl IntoResponse {
    let _ = plugin_id;
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/plain")],
        String::new(),
    )
}

async fn upgrade_plugin(
    State(_s): State<AppState>,
    Path(plugin_id): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = plugin_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "upgrade-queued" })),
    )
}

async fn plugin_config(State(_s): State<AppState>, Path(plugin_id): Path<String>) -> Json<Value> {
    let _ = plugin_id;
    Json(json!({ "config": {} }))
}
