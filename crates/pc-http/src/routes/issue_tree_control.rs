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
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PreviewBody>,
) -> ApiResult<Json<Value>> {
    let mode = body.mode.unwrap_or_else(|| "merge".to_owned());
    // Fetch affected child issue IDs (one level deep).
    let affected: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM issues WHERE parent_id = $1 AND hidden_at IS NULL")
            .bind(id)
            .fetch_all(state.db.pool())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let pool = state.db.pool();
    let hold_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM issue_tree_holds WHERE issue_id = $1 AND released_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let last: Option<pc_core::Timestamp> =
        sqlx::query_scalar("SELECT MAX(created_at) FROM issue_tree_holds WHERE issue_id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .flatten();
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
    let rows: Vec<(Uuid, String, Option<String>, Option<String>, pc_core::Timestamp, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT id, scope, reason, created_by_user_id, created_at, released_at          FROM issue_tree_holds WHERE issue_id = $1 ORDER BY created_at DESC",
    )
    .bind(id)
    .fetch_all(state.db.pool())
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
    let row: Option<(Uuid, Uuid, String, Option<String>, Option<String>, pc_core::Timestamp, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT id, issue_id, scope, reason, created_by_user_id, created_at, released_at          FROM issue_tree_holds WHERE id = $1",
    )
    .bind(hold_uuid)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((id, issue_id, scope, reason, by, created, released)) = row else {
        return Err(ApiError::NotFound(format!("tree hold {hold_id}")));
    };
    let status = if released.is_some() {
        "released"
    } else {
        "active"
    };
    Ok(Json(json!({
        "id": id,
        "issueId": issue_id,
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
    let row: (Uuid, pc_core::Timestamp) = sqlx::query_as(
        "INSERT INTO issue_tree_holds (issue_id, scope, reason, created_by_user_id)          VALUES ($1, $2, $3, 'local-board') RETURNING id, created_at",
    )
    .bind(id)
    .bind(&scope)
    .bind(&reason)
    .fetch_one(state.db.pool())
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
    sqlx::query(
        "UPDATE issue_tree_holds SET released_at = now() WHERE id = $1 AND released_at IS NULL",
    )
    .bind(hold_uuid)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::OK,
        Json(json!({ "id": hold_id, "status": "released" })),
    ))
}
