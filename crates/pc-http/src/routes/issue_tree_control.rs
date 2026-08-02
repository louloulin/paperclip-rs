//! Issue tree control (rerun/redo/merge) — preview & hold management.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/issues/:id/tree-control/preview",
            post(preview_tree_control),
        )
        .route(
            "/api/issues/:id/tree-control/state",
            get(tree_control_state),
        )
        .route(
            "/api/issues/:id/tree-holds",
            get(list_tree_holds).post(create_tree_hold),
        )
        .route(
            "/api/issues/:id/tree-holds/:hold_id",
            get(get_tree_hold).post(release_tree_hold),
        )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct PreviewBody {
    mode: Option<String>,
    target_issue_id: Option<Uuid>,
    include_subtree: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct CreateHoldBody {
    reason: Option<String>,
    scope: Option<String>,
}

async fn preview_tree_control(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PreviewBody>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "issueId": id,
        "mode": body.mode.unwrap_or_else(|| "merge".to_owned()),
        "affectedIssueIds": [],
        "warnings": [],
        "previewAt": chrono::Utc::now()
    })))
}

async fn tree_control_state(State(_state): State<AppState>, Path(id): Path<Uuid>) -> Json<Value> {
    Json(json!({
        "issueId": id,
        "mode": "merge",
        "holdCount": 0,
        "lastChangedAt": null
    }))
}

async fn list_tree_holds(State(_state): State<AppState>, Path(id): Path<Uuid>) -> Json<Value> {
    Json(json!({ "issueId": id, "holds": [] }))
}

async fn get_tree_hold(
    State(_state): State<AppState>,
    Path((_id, hold_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    if hold_id.is_empty() {
        return Err(ApiError::BadRequest("hold_id required".into()));
    }
    Ok(Json(json!({
        "id": hold_id,
        "status": "active"
    })))
}

async fn create_tree_hold(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateHoldBody>,
) -> impl IntoResponse {
    let _ = id;
    (
        StatusCode::CREATED,
        Json(json!({
            "issueId": id,
            "reason": body.reason.unwrap_or_default(),
            "scope": body.scope.unwrap_or_else(|| "subtree".to_owned()),
            "createdAt": chrono::Utc::now()
        })),
    )
}

async fn release_tree_hold(
    State(_state): State<AppState>,
    Path((_id, hold_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({ "id": hold_id, "status": "released" })),
    )
}
