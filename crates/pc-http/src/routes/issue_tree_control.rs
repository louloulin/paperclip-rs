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
use pc_repos::issue::IssueRepo;
use pc_repos::issue_tree_hold::IssueTreeHoldRepo;

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
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PreviewBody>,
) -> ApiResult<Json<Value>> {
    let mode = body.mode.unwrap_or_else(|| "merge".to_owned());
    // Fetch affected child issue IDs (one level deep).
    let children = IssueRepo::new(&state.db)
        .list_children(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let affected: Vec<Uuid> = children.into_iter().map(|c| c.id).collect();
    Ok(Json(json!({
        "issueId": id,
        "mode": mode,
        "affectedIssueIds": affected,
        "warnings": [],
        "previewAt": chrono::Utc::now()
    })))
}

async fn tree_control_state(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let hold_count = IssueTreeHoldRepo::new(&state.db)
        .count_active_by_released_at(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let last = IssueTreeHoldRepo::new(&state.db)
        .latest_change_at(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "issueId": id,
        "mode": "merge",
        "holdCount": hold_count,
        "lastChangedAt": last,
    })))
}

async fn list_tree_holds(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueTreeHoldRepo::new(&state.db)
        .list_holds_v1(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let holds: Vec<Value> = rows
        .into_iter()
        .map(|(id, scope, reason, by, created, released)| {
            json!({
                "id": id,
                "scope": scope,
                "reason": reason,
                "createdBy": by,
                "createdAt": created,
                "releasedAt": released,
            })
        })
        .collect();
    Ok(Json(json!({"issueId": id, "holds": holds})))
}

async fn get_tree_hold(
    State(state): State<AppState>,
    Path((_id, hold_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    if hold_id.is_empty() {
        return Err(ApiError::BadRequest("hold_id required".into()));
    }
    let hold_uuid =
        Uuid::parse_str(&hold_id).map_err(|_| ApiError::BadRequest("invalid hold id".into()))?;
    let row = IssueTreeHoldRepo::new(&state.db)
        .get_hold_by_id_v1(hold_uuid)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((id, root_issue_id, scope, reason, by, created, released)) = row else {
        return Err(ApiError::NotFound(format!("tree hold {hold_id}")));
    };
    let status = if released.is_some() {
        "released"
    } else {
        "active"
    };
    Ok(Json(json!({
        "id": id,
        "issueId": root_issue_id,
        "scope": scope,
        "reason": reason,
        "createdBy": by,
        "createdAt": created,
        "releasedAt": released,
        "status": status,
    })))
}

async fn create_tree_hold(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateHoldBody>,
) -> ApiResult<impl IntoResponse> {
    let scope = body.scope.clone().unwrap_or_else(|| "subtree".to_owned());
    let reason = body.reason.clone();
    let issue_company = IssueRepo::new(&state.db)
        .get(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map(|i| i.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    let row = IssueTreeHoldRepo::new(&state.db)
        .create_v1(issue_company, id, "merge", "active", reason.as_deref(), "local-board")
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.0,
            "issueId": id,
            "scope": scope,
            "reason": reason,
            "createdAt": row.1,
        })),
    ))
}

async fn release_tree_hold(
    State(state): State<AppState>,
    Path((_id, hold_id)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let hold_uuid =
        Uuid::parse_str(&hold_id).map_err(|_| ApiError::BadRequest("invalid hold id".into()))?;
    IssueTreeHoldRepo::new(&state.db)
        .release_by_id(hold_uuid)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::OK,
        Json(json!({ "id": hold_id, "status": "released" })),
    ))
}
