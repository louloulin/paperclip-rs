//! `POST /api/decision-training` 路由模块（decision training）。
//!
//! 完整实现位于 Phase C；当前为 Phase A/B 占位，返回与原 server 同构的空响应。

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/decision-training", get(handler))
}

async fn handler() -> Json<Value> {
    Json(json!({
        "module": "decision_training",
        "description": "decision training",
        "status": "ok",
    }))
}
