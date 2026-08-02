//! Issue checkout + wakeup 路径。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/issues/:issue_id/checkout", post(checkout))
        .route("/api/issues/:issue_id/wakeup", post(wakeup))
}

async fn checkout(State(_state): State<AppState>, Path(issue_id): Path<Uuid>) -> impl IntoResponse {
    let _ = issue_id;
    (
        StatusCode::OK,
        Json(json!({
            "issueId": issue_id,
            "status": "checked-out",
            "actorId": null
        })),
    )
}

async fn wakeup(State(_state): State<AppState>, Path(issue_id): Path<Uuid>) -> impl IntoResponse {
    let _ = issue_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "issueId": issue_id,
            "status": "wakeup-queued"
        })),
    )
}
