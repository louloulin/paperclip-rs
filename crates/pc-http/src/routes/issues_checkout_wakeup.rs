//! `POST /api/issues/checkout-wakeup` 路由模块（issues checkout wakeup）。
//!
//! 完整实现位于 Phase C；当前为 Phase A/B 占位，返回与原 server 同构的空响应。

use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/issues/checkout-wakeup", post(handler))
        .route("/api/issues/checkout-wakeup", get(meta))
}

async fn meta() -> Json<Value> {
    Json(json!({"method": "POST", "description": "should-wake-assignee"}))
}

async fn handler() -> Json<Value> {
    Json(json!({
        "module": "issues_checkout_wakeup",
        "description": "issues checkout wakeup",
        "status": "ok",
    }))
}
