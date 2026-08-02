//! `POST /api/plugins` 路由模块（plugins）。
//!
//! 完整实现位于 Phase C；当前为 Phase A/B 占位，返回与原 server 同构的空响应。

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/plugins", get(handler))
}

async fn handler() -> Json<Value> {
    Json(json!({
        "module": "plugins",
        "description": "plugins",
        "status": "ok",
    }))
}
