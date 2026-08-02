//! 通用 authz 决策端点。

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/authz", get(authz))
}

async fn authz(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({
        "module": "authz",
        "status": "ok"
    }))
}
