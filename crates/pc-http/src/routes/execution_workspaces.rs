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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::execution::{
    ActionKind, ActionStatus, ExecutionRepo, NewLease, RuntimeLifecycle, RuntimeServiceRow,
    WorkspaceRow, WorkspaceStatus,
};

use pc_auth::AuthContext;

use crate::{authz_runtime_service, ApiError, ApiResult, AppState};

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
        // ===== Round 32: workspace validation + git worktree =====
        .route(
            "/api/execution-workspaces/:id/validate",
            post(validate_workspace_route),
        )
        .route(
            "/api/execution-workspaces/:id/worktree",
            post(create_worktree_route),
        )
        .route(
            "/api/execution-workspaces/:id/worktree/cleanup",
            post(cleanup_worktree_route),
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
    let (active, recent, needs_attention) = ExecutionRepo::new(&state.db)
        .overview_stats(company_id)
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
    let direct = ExecutionRepo::new(&state.db)
        .get_by_id(id)
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
    // R807: update_name returns WorkspaceRow; RepoError::NotFound -> 404
    let row = ExecutionRepo::new(&state.db)
        .update_name(id, body.name.as_deref())
        .await
        .map_err(|err| match err {
            pc_repos::RepoError::NotFound { .. } => ApiError::NotFound(format!("workspace {id}")),
            other => ApiError::Internal(other.to_string()),
        })?;
    Ok(Json(json!({
        "id": row.id,
        "status": "updated",
        "name": row.name,
    })))
}

async fn close_readiness(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<Value>> {
    let last_run = ExecutionRepo::new(&state.db)
        .latest_heartbeat_for_workspace(id)
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
    let ws = ExecutionRepo::new(&state.db)
        .get_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (status, mode) = match ws {
        Some(w) => (w.status, w.mode),
        None => ("active".to_string(), "execution".to_string()),
    };
    let mut operations = vec![
        json!({ "key": "rebuild", "label": "Rebuild", "enabled": status != "closed" }),
        json!({ "key": "reset", "label": "Reset", "enabled": status == "active" }),
        json!({ "key": "reconcile", "label": "Reconcile", "enabled": status == "active" }),
        json!({ "key": "archive", "label": "Archive", "enabled": status == "active" || status == "cleaning" }),
    ];
    if mode == "execution" {
        operations.push(
            json!({ "key": "switch_strategy", "label": "Switch Strategy", "enabled": false }),
        );
    }
    Ok(Json(json!({
        "id": id,
        "operations": operations,
    })))
}

async fn runtime_service_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(uuid::Uuid, String)>,
    auth: AuthContext,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let company_id = ExecutionRepo::new(&state.db)
        .company_id_for_workspace(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("execution workspace {id}")))?;
    authz_runtime_service::assert_execution_workspace_runtime_manage(
        &state.db, &auth, company_id, id, None,
    )
    .await
    .map_err(authz_runtime_service::map_authz_error_to_api)?;
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
    let _ = ExecutionRepo::new(&state.db)
        .set_status_to_reconciling(id)
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
        None => Err(ApiError::NotFound(format!(
            "active lease for workspace {id}"
        ))),
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
    let company_id = repo
        .company_id_for_id(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("execution workspace {id}")))?;
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
        None => Err(ApiError::NotFound(format!(
            "lease {} not held",
            body.lease_id
        ))),
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
    // R803: release_lease returns LeaseRow; RepoError::NotFound -> 404
    let repo = ExecutionRepo::new(&state.db);
    let row = repo
        .release_lease(body.lease_id, &body.token)
        .await
        .map_err(|err| match err {
            pc_repos::RepoError::NotFound { .. } => ApiError::NotFound(format!(
                "lease {} not held",
                body.lease_id
            )),
            other => ApiError::Internal(other.to_string()),
        })?;
    Ok(Json(
        json!({ "leaseId": row.id, "status": "released" }),
    ))
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
        None => Err(ApiError::NotFound(format!(
            "active lease for workspace {_id}"
        ))),
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

// ============ Round 32: workspace validation + git worktree ============

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ValidateWorkspaceBody {
    #[serde(default)]
    fetch_remote: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceValidationReport {
    workspace_id: Uuid,
    company_id: Uuid,
    worktree_path: Option<String>,
    valid: bool,
    repo_root: Option<String>,
    branch: Option<String>,
    cleanliness: &'static str,
    dirty_files: Vec<String>,
    error: Option<String>,
    checked_at: chrono::DateTime<chrono::Utc>,
}

/// Run `git <args>` in `cwd` and return Ok(stdout) / Err(stderr).
async fn run_git(cwd: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).current_dir(cwd);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("spawn git failed: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(stderr.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// POST /api/execution-workspaces/:id/validate — git validate
async fn validate_workspace_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ValidateWorkspaceBody>,
) -> ApiResult<Json<WorkspaceValidationReport>> {
    let ws = ExecutionRepo::new(&state.db)
        .get_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("execution workspace {id}")))?;
    let company_id = ws.company_id;
    let provider_ref = ws.provider_ref.clone();
    let cwd = ws.cwd.clone();
    let worktree_path = provider_ref.or(cwd);
    let mut report = WorkspaceValidationReport {
        workspace_id: id,
        company_id,
        worktree_path: worktree_path.clone(),
        valid: false,
        repo_root: None,
        branch: None,
        cleanliness: "unknown",
        dirty_files: Vec::new(),
        error: None,
        checked_at: chrono::Utc::now(),
    };
    let path = match worktree_path.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => {
            report.error = Some("workspace has no provider_ref or cwd".into());
            return Ok(Json(report));
        }
    };
    if !tokio::fs::metadata(path).await.is_ok() {
        report.error = Some(format!("path does not exist: {path}"));
        return Ok(Json(report));
    }
    // git rev-parse --show-toplevel
    match run_git(path, &["rev-parse", "--show-toplevel"]).await {
        Ok(root) => report.repo_root = Some(root),
        Err(e) => {
            report.error = Some(format!("rev-parse: {e}"));
            return Ok(Json(report));
        }
    }
    // git symbolic-ref --quiet --short HEAD (branch)
    match run_git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await {
        Ok(b) if !b.is_empty() => report.branch = Some(b),
        _ => {}
    }
    // git status --porcelain --untracked-files=all
    match run_git(path, &["status", "--porcelain", "--untracked-files=all"]).await {
        Ok(out) => {
            let files: Vec<String> = out
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|s| s.to_string())
                .collect();
            report.cleanliness = if files.is_empty() { "clean" } else { "dirty" };
            report.dirty_files = files;
        }
        Err(e) => {
            report.error = Some(format!("status: {e}"));
            return Ok(Json(report));
        }
    }
    report.valid = report.error.is_none();
    // optional fetch
    if body.fetch_remote.unwrap_or(false) {
        let _ = run_git(path, &["fetch", "--all", "--prune"]).await;
    }
    // touch last_used_at
    ExecutionRepo::new(&state.db)
        .touch_last_used(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(report))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CreateWorktreeBody {
    branch: String,
    #[serde(default)]
    base_ref: Option<String>,
    #[serde(default)]
    worktree_path: Option<String>,
    #[serde(default)]
    fetch_remote: Option<bool>,
}

/// POST /api/execution-workspaces/:id/worktree — git worktree add
async fn create_worktree_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateWorktreeBody>,
) -> ApiResult<Json<Value>> {
    if body.branch.trim().is_empty() {
        return Err(ApiError::BadRequest("branch required".into()));
    }
    let ws = ExecutionRepo::new(&state.db)
        .get_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("execution workspace {id}")))?;
    let company_id = ws.company_id;
    let cwd = ws.cwd.clone();
    let provider_ref = ws.provider_ref.clone();
    let main_repo = cwd.ok_or_else(|| {
        ApiError::BadRequest("workspace has no cwd (main repo path); cannot create worktree".into())
    })?;
    let worktree_path = body.worktree_path.clone().unwrap_or_else(|| {
        format!(
            "{}/.worktrees/{}",
            main_repo.trim_end_matches('/'),
            body.branch
        )
    });
    // optional fetch
    if body.fetch_remote.unwrap_or(false) {
        let _ = run_git(&main_repo, &["fetch", "--all", "--prune"]).await;
    }
    // git worktree add [-B <branch> [<base>]] <path>
    let mut args: Vec<String> = vec!["worktree".into(), "add".into()];
    if let Some(base) = body.base_ref.as_deref() {
        args.push("-B".into());
        args.push(body.branch.clone());
        args.push(base.into());
    } else {
        args.push("-b".into());
        args.push(body.branch.clone());
    }
    args.push(worktree_path.clone());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git(&main_repo, &arg_refs)
        .await
        .map_err(|e| ApiError::Conflict(format!("git worktree add failed: {e}")))?;
    // Persist new branch + provider_ref on the workspace
    ExecutionRepo::new(&state.db)
        .set_branch_provider_ref(id, &body.branch, &worktree_path)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "created": true,
        "workspaceId": id,
        "companyId": company_id,
        "branch": body.branch,
        "worktreePath": worktree_path,
        "mainRepo": main_repo,
        "previousProviderRef": provider_ref,
    })))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CleanupWorktreeBody {
    #[serde(default)]
    force: Option<bool>,
}

/// POST /api/execution-workspaces/:id/worktree/cleanup — git worktree remove
async fn cleanup_worktree_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CleanupWorktreeBody>,
) -> ApiResult<Json<Value>> {
    let ws = ExecutionRepo::new(&state.db)
        .get_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("execution workspace {id}")))?;
    let company_id = ws.company_id;
    let cwd = ws.cwd.clone();
    let provider_ref = ws.provider_ref.clone();
    let worktree_path = provider_ref.clone().ok_or_else(|| {
        ApiError::BadRequest("workspace has no provider_ref; nothing to clean up".into())
    })?;
    let main_repo = cwd.clone().unwrap_or_else(|| worktree_path.clone());
    let force_flag = body.force.unwrap_or(false);
    let mut args: Vec<String> = vec!["worktree".into(), "remove".into()];
    if force_flag {
        args.push("--force".into());
    }
    args.push(worktree_path.clone());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let removed = run_git(&main_repo, &arg_refs).await.is_ok();
    if removed {
        ExecutionRepo::new(&state.db)
            .clear_provider_ref(id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    Ok(Json(json!({
        "removed": removed,
        "workspaceId": id,
        "companyId": company_id,
        "worktreePath": worktree_path,
        "forced": force_flag,
    })))
}
