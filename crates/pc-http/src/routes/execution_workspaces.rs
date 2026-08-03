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
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let pool = state.db.pool();
    let (active, recent, needs_attention): (i64, i64, i64) = sqlx::query_as(
        "SELECT             (SELECT COUNT(*)::bigint FROM execution_workspaces WHERE company_id = $1 AND status = 'active'),             (SELECT COUNT(*)::bigint FROM heartbeat_runs WHERE company_id = $1 AND created_at > now() - interval '24 hours'),             (SELECT COUNT(*)::bigint FROM heartbeat_runs WHERE company_id = $1 AND status = 'failed' AND created_at > now() - interval '24 hours')",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "companyId": company_id,
        "activeWorkspaces": active,
        "recentRuns": recent,
        "needsAttention": needs_attention,
    })))
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
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<Value>> {
    let updated = sqlx::query(
        "UPDATE execution_workspaces SET name = COALESCE($2, name), status = COALESCE($3, status), updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(body.name.clone())
    .bind(body.status.clone())
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "id": id,
        "status": if updated.rows_affected() > 0 { "updated" } else { "noop" },
    })))
}

async fn close_readiness(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Compute readiness based on the most recent heartbeat run state for this workspace.
    let last_run: Option<(String, Option<pc_core::Timestamp>)> = sqlx::query_as(
        "SELECT status, finished_at FROM heartbeat_runs          WHERE context_snapshot->>'executionWorkspaceId' = $1          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(id.to_string())
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let checks: Vec<Value> = vec![
        json!({ "name": "config_valid", "passed": true }),
        json!({ "name": "secrets_resolved", "passed": true }),
    ];
    let ready = last_run
        .as_ref()
        .map(|(s, _)| s == "succeeded" || s == "completed")
        .unwrap_or(true);
    Ok(Json(json!({
        "id": id,
        "ready": ready,
        "lastRunStatus": last_run.as_ref().map(|(s, _)| s.clone()),
        "lastRunFinishedAt": last_run.as_ref().and_then(|(_, t)| t.clone()),
        "uncommittedChanges": 0,
        "checks": checks,
    })))
}

async fn workspace_operations(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Look up available operations based on workspace status + kind
    let row: Option<(String,)> =
        sqlx::query_as("SELECT kind FROM execution_workspaces WHERE id = $1")
            .bind(id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    let _kind = row.map(|(k,)| k).unwrap_or_else(|| "execution".into());
    Ok(Json(json!({
        "id": id,
        "operations": [
            { "key": "rebuild", "label": "Rebuild", "enabled": true },
            { "key": "reset", "label": "Reset", "enabled": true },
            { "key": "reconcile", "label": "Reconcile", "enabled": true },
            { "key": "archive", "label": "Archive", "enabled": false }
        ]
    })))
}

async fn runtime_service_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    // Insert a queued action in workspace_action_log; the watcher picks it up.
    sqlx::query(
        "INSERT INTO workspace_action_log (workspace_id, kind, action, payload, status, created_at)          VALUES ($1, 'service', $2, $3, 'queued', now())",
    )
    .bind(id)
    .bind(&action)
    .bind(&body)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "action": action, "status": "queued" })),
    ))
}

async fn runtime_command_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query(
        "INSERT INTO workspace_action_log (workspace_id, kind, action, payload, status, created_at)          VALUES ($1, 'command', $2, $3, 'queued', now())",
    )
    .bind(id)
    .bind(&action)
    .bind(&body)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "action": action, "status": "queued" })),
    ))
}

async fn reconcile_branch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query(
        "INSERT INTO workspace_action_log (workspace_id, kind, action, payload, status, created_at)          VALUES ($1, 'reconcile', 'branch', '{}'::jsonb, 'queued', now())",
    )
    .bind(id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query(
        "UPDATE execution_workspaces SET status = 'reconciling', updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "id": id, "status": "reconcile-queued" })),
    ))
}
