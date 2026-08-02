//! 公司执行 workspace 概览与操作。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/execution-workspaces",
            get(list_workspaces),
        )
        .route(
            "/api/companies/:company_id/workspace-overview",
            get(workspace_overview),
        )
        .route(
            "/api/execution-workspaces/:id",
            get(get_workspace).patch(patch_workspace),
        )
        .route(
            "/api/execution-workspaces/:id/close-readiness",
            get(close_readiness),
        )
        .route(
            "/api/execution-workspaces/:id/workspace-operations",
            get(workspace_operations),
        )
        .route(
            "/api/execution-workspaces/:id/runtime-services/:action",
            post(runtime_service_action),
        )
        .route(
            "/api/execution-workspaces/:id/runtime-commands/:action",
            post(runtime_command_action),
        )
        .route(
            "/api/execution-workspaces/:id/reconcile-branch",
            post(reconcile_branch),
        )
}

#[derive(Debug, FromRow)]
struct WorkspaceRow {
    id: Uuid,
    company_id: Uuid,
    project_id: Uuid,
    name: String,
    mode: String,
    strategy_type: String,
    status: String,
    branch_name: Option<String>,
    base_ref: Option<String>,
    cwd: Option<String>,
    repo_url: Option<String>,
    opened_at: pc_core::Timestamp,
    closed_at: Option<pc_core::Timestamp>,
    last_used_at: pc_core::Timestamp,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn row_json(row: &WorkspaceRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "projectId": row.project_id,
        "name": row.name,
        "mode": row.mode,
        "strategyType": row.strategy_type,
        "status": row.status,
        "branchName": row.branch_name,
        "baseRef": row.base_ref,
        "cwd": row.cwd,
        "repoUrl": row.repo_url,
        "openedAt": row.opened_at,
        "closedAt": row.closed_at,
        "lastUsedAt": row.last_used_at,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

async fn list_workspaces(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<WorkspaceRow> = sqlx::query_as(
        "SELECT id, company_id, project_id, name, mode, strategy_type, status, branch_name, base_ref, \
                cwd, repo_url, opened_at, closed_at, last_used_at, created_at, updated_at \
         FROM execution_workspaces WHERE company_id = $1 ORDER BY last_used_at DESC LIMIT 100",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn workspace_overview(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "activeWorkspaces": 0,
        "recentRuns": 0,
        "needsAttention": 0
    }))
}

async fn get_workspace(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<WorkspaceRow> = sqlx::query_as(
        "SELECT id, company_id, project_id, name, mode, strategy_type, status, branch_name, base_ref, \
                cwd, repo_url, opened_at, closed_at, last_used_at, created_at, updated_at \
         FROM execution_workspaces WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(row_json(&row))),
        None => Err(ApiError::NotFound(format!("workspace {id}"))),
    }
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct PatchBody {
    name: Option<String>,
    status: Option<String>,
}

async fn patch_workspace(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(_body): Json<PatchBody>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "id": id, "status": "updated" })))
}

async fn close_readiness(State(_state): State<AppState>, Path(id): Path<Uuid>) -> Json<Value> {
    Json(json!({
        "id": id,
        "ready": true,
        "uncommittedChanges": 0,
        "checks": []
    }))
}

async fn workspace_operations(State(_state): State<AppState>, Path(id): Path<Uuid>) -> Json<Value> {
    Json(json!({
        "id": id,
        "operations": [
            { "key": "rebuild", "label": "Rebuild", "enabled": true },
            { "key": "reset", "label": "Reset", "enabled": true }
        ]
    }))
}

async fn runtime_service_action(
    State(_state): State<AppState>,
    Path((id, action)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "action": action, "status": "queued" })),
    )
}

async fn runtime_command_action(
    State(_state): State<AppState>,
    Path((id, action)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "action": action, "status": "queued" })),
    )
}

async fn reconcile_branch(
    State(_state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "status": "reconcile-queued" })),
    )
}
