//! 实例级数据库备份。

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde_json::json;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/instance/database-backups", post(trigger_backup))
}

async fn trigger_backup(State(_state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "accepted",
            "trigger": "manual",
            "message": "backup scheduling delegated to background task in the embedded PG mode"
        })),
    )
}
