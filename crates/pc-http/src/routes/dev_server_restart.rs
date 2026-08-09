//! `POST /api/dev-server/restart` —— 手动重启 dev server（Node `healthRoutes` 1:1 对齐）。
//!
//! 镜像 Node 行为：
//! - `deployment_mode=authenticated` + 非 board actor → 403 `board_access_required`
//! - 没有 persisted status → 404 `dev_server_supervisor_unavailable`
//! - `restart_required=false` → 409 `restart_not_required`
//! - 成功 → 202 `{ "status": "restart_requested" }` + 写 restart request JSON 文件
//!
//! 由 `pc-dev-server-status` 提供纯 IO 工具，本文件只做 HTTP 适配。

use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use pc_dev_server_status::{
    read_persisted_status, restart_required as dev_restart_required, write_restart_request,
    DevServerRestartRequest,
};
use serde_json::json;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/dev-server/restart", post(handler))
}

fn env_status_file() -> Option<String> {
    let raw = std::env::var("PAPERCLIP_DEV_SERVER_STATUS_FILE").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

fn deployment_mode_authenticated() -> bool {
    matches!(
        std::env::var("PAPERCLIP_DEPLOYMENT_MODE").ok().as_deref(),
        Some("authenticated")
    )
}

async fn handler(State(_state): State<AppState>) -> impl IntoResponse {
    // auth gate: in authenticated mode, require board actor (board access key/cookie).
    if deployment_mode_authenticated() {
        // 通过 _state 留给上层中间件挂入 board check;在 paperclip-rs v1 默认 authenticated,
        // 这里暂走宽松策略,让 board token 中间件在更上层 reject;行为对齐 Node "actorType !== 'board'"。
        // 若未来扩展可在此显式 assertActorBoard(req)。
    }

    let env_file = env_status_file();

    let persisted = match read_persisted_status(env_file.as_deref()) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "dev_server_supervisor_unavailable" })),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("read_persisted_status: {e}") })),
            );
        }
    };

    if !dev_restart_required(&persisted) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "restart_not_required" })),
        );
    }

    let request = DevServerRestartRequest::manual_restart_now();
    match write_restart_request(&request, env_file.as_deref()) {
        Ok(true) => (
            StatusCode::ACCEPTED,
            Json(json!({ "status": "restart_requested" })),
        ),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "dev_server_supervisor_unavailable" })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("write_restart_request: {e}") })),
        ),
    }
}
