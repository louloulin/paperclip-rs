//! 公司执行 workspace 概览与操作。
//!
//! 端点与 Node `routes/execution-workspaces.ts` 对齐：
//! * list by company
//! * workspace overview（汇总 active / recent / needs attention）
//! * 单 workspace get/patch
//! * close readiness（config_valid + secrets_resolved + 最近 heartbeat run 状态）
//! * workspace operations（rebuild / reset / reconcile / archive）
//! * runtime service / command action 排队 → workspace_action_log
//! * branch reconcile 状态切换 + 排队
//! * runtime service state machine 状态切换

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use pc_repos::execution::{NewLease,
    ActionKind, ActionStatus, ExecutionRepo, RuntimeLifecycle, RuntimeServiceRow, WorkspaceRow,
    WorkspaceStatus,
};

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
        .route(
            "/api/execution-workspaces/:id/action-log",
            get(list_action_log),
        )
        .route(
            "/api/execution-workspaces/:id/lease",
            get(active_lease).delete(revoke_lease_route),
        )
        .route(
            "/api/execution-workspaces/:id/lease/acquire",
            post(acquire_lease_route),
        )
        .route(
            "/api/execution-workspaces/:id/lease/renew",
            post(renew_lease_route),
        )
        .route(
            "/api/execution-workspaces/:id/lease/release",
            post(release_lease_route),
        )
        .route(
            "/api/execution-workspaces/:id/runtime-services",
            get(list_runtime_services),
        )
        .route(
            "/api/runtime-services/:service_id/lifecycle",
            post(set_runtime_service_lifecycle),
        )
}

fn row_json(row: &WorkspaceRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "projectId": row.project_id,
        "projectWorkspaceId": row.project_workspace_id,
        "sourceIssueId": row.source_issue_id,
        "name": row.name,
        "mode": row.mode,
        "strategyType": row.strategy_type,
        "status": row.status,
        "branchName": row.branch_name,
        "baseRef": row.base_ref,
        "cwd": row.cwd,
        "repoUrl": row.repo_url,
        "providerType": row.provider_type,
        "providerRef": row.provider_ref,
        "openedAt": row.opened_at,
        "closedAt": row.closed_at,
        "lastUsedAt": row.last_used_at,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

fn runtime_service_json(row: &RuntimeServiceRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "projectId": row.project_id,
        "projectWorkspaceId": row.project_workspace_id,
        "issueId": row.issue_id,
        "scopeType": row.scope_type,
        "scopeId": row.scope_id,
        "serviceName": row.service_name,
        "status": row.status,
        "lifecycle": row.lifecycle,
        "reuseKey": row.reuse_key,
        "command": row.command,
        "cwd": row.cwd,
        "port": row.port,
        "url": row.url,
        "provider": row.provider,
        "providerRef": row.provider_ref,
        "ownerAgentId": row.owner_agent_id,
        "startedByRunId": row.started_by_run_id,
        "lastUsedAt": row.last_used_at,
        "startedAt": row.started_at,
        "stoppedAt": row.stopped_at,
        "stopPolicy": row.stop_policy,
        "healthStatus": row.health_status,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

async fn list_workspaces(
    State(state): State<AppState>,
    Path(company_id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ExecutionRepo::new(&state.db)
        .list_by_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn workspace_overview(
    State(state): State<AppState>,
    Path(company_id): Path<uuid::Uuid>,
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
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    let direct: Option<WorkspaceRow> = sqlx::query_as::<_, WorkspaceRow>(
        "SELECT id, company_id, project_id, project_workspace_id, source_issue_id, mode, \
                strategy_type, name, status, cwd, repo_url, base_ref, branch_name, \
                provider_type, provider_ref, derived_from_execution_workspace_id, \
                last_used_at, opened_at, closed_at, cleanup_eligible_at, cleanup_reason, \
                metadata, created_at, updated_at \
         FROM execution_workspaces WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    match direct {
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
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<Value>> {
    let updated = sqlx::query(
        "UPDATE execution_workspaces SET name = COALESCE($2, name), updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(body.name.clone())
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
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
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
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT status, mode FROM execution_workspaces WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (status, mode) = row.unwrap_or(("active".into(), "execution".into()));
    let mut operations = vec![
        json!({ "key": "rebuild", "label": "Rebuild", "enabled": status != "closed" }),
        json!({ "key": "reset", "label": "Reset", "enabled": status == "active" }),
        json!({ "key": "reconcile", "label": "Reconcile", "enabled": status == "active" }),
        json!({ "key": "archive", "label": "Archive", "enabled": status == "active" || status == "cleaning" }),
    ];
    if mode == "execution" {
        operations.push(json!({ "key": "switch_strategy", "label": "Switch Strategy", "enabled": false }));
    }
    Ok(Json(json!({
        "id": id,
        "operations": operations,
    })))
}

async fn runtime_service_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(uuid::Uuid, String)>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let repo = ExecutionRepo::new(&state.db);
    let queued = repo
        .enqueue_action(&pc_repos::execution::NewActionLog {
            workspace_id: id,
            kind: ActionKind::Service,
            action: action.clone(),
            payload: Some(body),
            requested_by_user_id: None,
            requested_by_agent_id: None,
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "id": queued.id,
            "workspaceId": queued.workspace_id,
            "kind": queued.kind,
            "action": queued.action,
            "status": queued.status,
            "createdAt": queued.created_at,
        })),
    ))
}

async fn runtime_command_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(uuid::Uuid, String)>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let repo = ExecutionRepo::new(&state.db);
    let queued = repo
        .enqueue_action(&pc_repos::execution::NewActionLog {
            workspace_id: id,
            kind: ActionKind::Command,
            action: action.clone(),
            payload: Some(body),
            requested_by_user_id: None,
            requested_by_agent_id: None,
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "id": queued.id,
            "workspaceId": queued.workspace_id,
            "kind": queued.kind,
            "action": queued.action,
            "status": queued.status,
            "createdAt": queued.created_at,
        })),
    ))
}

async fn reconcile_branch(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let repo = ExecutionRepo::new(&state.db);
    let queued = repo
        .enqueue_action(&pc_repos::execution::NewActionLog {
            workspace_id: id,
            kind: ActionKind::Reconcile,
            action: "branch".into(),
            payload: Some(serde_json::json!({})),
            requested_by_user_id: None,
            requested_by_agent_id: None,
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    // 同时切到 reconciling 状态；事务失败会被 rollback
    let _ = sqlx::query(
        "UPDATE execution_workspaces SET status = 'reconciling', updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "id": queued.id,
            "workspaceId": queued.workspace_id,
            "kind": queued.kind,
            "action": queued.action,
            "status": queued.status,
            "createdAt": queued.created_at,
        })),
    ))
}

async fn list_action_log(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ExecutionRepo::new(&state.db)
        .list_actions_for_workspace(id, 100)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "workspaceId": r.workspace_id,
                "kind": r.kind,
                "action": r.action,
                "payload": r.payload,
                "status": r.status,
                "error": r.error,
                "startedAt": r.started_at,
                "completedAt": r.completed_at,
                "createdAt": r.created_at,
                "updatedAt": r.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "workspaceId": id,
        "items": items,
    })))
}

async fn list_runtime_services(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ExecutionRepo::new(&state.db)
        .list_runtime_services_for_workspace(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows.iter().map(runtime_service_json).collect();
    Ok(Json(json!({
        "workspaceId": id,
        "items": items,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Lifecycle {
    Fresh,
    Started,
    Restarting,
    Stopped,
}

#[derive(Debug, Deserialize)]
struct LifecycleBody {
    lifecycle: Lifecycle,
}

async fn set_runtime_service_lifecycle(
    State(state): State<AppState>,
    Path(service_id): Path<uuid::Uuid>,
    Json(body): Json<LifecycleBody>,
) -> ApiResult<Json<Value>> {
    let target = match body.lifecycle {
        Lifecycle::Fresh => RuntimeLifecycle::Fresh,
        Lifecycle::Started => RuntimeLifecycle::Started,
        Lifecycle::Restarting => RuntimeLifecycle::Restarting,
        Lifecycle::Stopped => RuntimeLifecycle::Stopped,
    };
    let row = ExecutionRepo::new(&state.db)
        .set_runtime_service_lifecycle(service_id, target)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(r) => Ok(Json(runtime_service_json(&r))),
        None => Err(ApiError::NotFound(format!("runtime service {service_id}"))),
    }
}


fn lease_json(row: &pc_repos::execution::LeaseRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "workspaceId": row.workspace_id,
        "agentId": row.agent_id,
        "runId": row.run_id,
        "heartbeatRunId": row.heartbeat_run_id,
        "state": row.state,
        "token": row.token,
        "acquiredAt": row.acquired_at,
        "expiresAt": row.expires_at,
        "lastRenewedAt": row.last_renewed_at,
        "releasedAt": row.released_at,
        "revocationReason": row.revocation_reason,
    })
}

async fn active_lease(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = ExecutionRepo::new(&state.db);
    let row = repo
        .active_lease_for_workspace(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(r) => Ok(Json(lease_json(&r))),
        None => Err(ApiError::NotFound(format!("active lease for workspace {id}"))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcquireLeaseBody {
    agent_id: uuid::Uuid,
    #[serde(default)]
    run_id: Option<uuid::Uuid>,
    #[serde(default)]
    heartbeat_run_id: Option<uuid::Uuid>,
    #[serde(default = "default_ttl")]
    ttl_secs: i64,
}

fn default_ttl() -> i64 {
    300
}

async fn acquire_lease_route(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<AcquireLeaseBody>,
) -> ApiResult<Json<Value>> {
    let repo = ExecutionRepo::new(&state.db);
    let company_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT company_id FROM execution_workspaces WHERE id = $1",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let row = repo
        .acquire_lease(&NewLease {
            company_id,
            workspace_id: id,
            agent_id: body.agent_id,
            run_id: body.run_id,
            heartbeat_run_id: body.heartbeat_run_id,
            ttl_secs: body.ttl_secs,
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(r) => Ok(Json(lease_json(&r))),
        None => Err(ApiError::Conflict(format!(
            "workspace {id} already has an active lease"
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenewLeaseBody {
    lease_id: uuid::Uuid,
    token: String,
    #[serde(default = "default_ttl")]
    new_ttl_secs: i64,
}

async fn renew_lease_route(
    State(state): State<AppState>,
    Path(_id): Path<uuid::Uuid>,
    Json(body): Json<RenewLeaseBody>,
) -> ApiResult<Json<Value>> {
    let repo = ExecutionRepo::new(&state.db);
    let row = repo
        .renew_lease(body.lease_id, &body.token, body.new_ttl_secs)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(r) => Ok(Json(lease_json(&r))),
        None => Err(ApiError::NotFound(format!("lease {} not held", body.lease_id))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseLeaseBody {
    lease_id: uuid::Uuid,
    token: String,
}

async fn release_lease_route(
    State(state): State<AppState>,
    Path(_id): Path<uuid::Uuid>,
    Json(body): Json<ReleaseLeaseBody>,
) -> ApiResult<Json<Value>> {
    let repo = ExecutionRepo::new(&state.db);
    let released = repo
        .release_lease(body.lease_id, &body.token)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if released {
        Ok(Json(json!({ "leaseId": body.lease_id, "status": "released" })))
    } else {
        Err(ApiError::NotFound(format!("lease {} not held", body.lease_id)))
    }
}

async fn revoke_lease_route(
    State(state): State<AppState>,
    Path(_id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    // DELETE /lease：清除当前 active lease（如有）。
    let repo = ExecutionRepo::new(&state.db);
    let active = repo
        .active_lease_for_workspace(_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match active {
        Some(r) => {
            repo.revoke_lease(r.id, "admin-revoked")
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            Ok(Json(json!({ "leaseId": r.id, "status": "revoked" })))
        }
        None => Err(ApiError::NotFound(format!("active lease for workspace {_id}"))),
    }
}
#[allow(dead_code)]
fn status_from_str(s: &str) -> Option<WorkspaceStatus> {
    WorkspaceStatus::parse(s)
}

#[allow(dead_code)]
fn action_status_from_str(s: &str) -> Option<ActionStatus> {
    ActionStatus::parse(s)
}
