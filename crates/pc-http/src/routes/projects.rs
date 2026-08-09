//! `/api/projects*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::project::ProjectRepo;

use axum::Extension as AxumExtension;
use pc_auth::AuthContext;
use pc_authz::{enforce_permission, PermissionKey};
use sqlx;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects", get(list).post(create))
        .route(
            "/api/projects/:id",
            get(get_one).patch(update).delete(remove),
        )
        .route(
            "/api/companies/:company_id/projects",
            get(list_company_projects).post(create_company_project),
        )
        .route("/api/projects/:id/workspaces", get(list_project_workspaces))
        .route("/api/projects/:id/goals", get(list_project_goals))
        .route(
            "/api/projects/:id/external-object-summary",
            get(project_external_object_summary),
        )
        // ── Round 22: project workspace CRUD + runtime commands ──
        .route(
            "/api/projects/:id/workspaces",
            post(create_project_workspace),
        )
        .route(
            "/api/projects/:id/workspaces/:workspace_id",
            patch(patch_project_workspace).delete(delete_project_workspace),
        )
        .route(
            "/api/projects/:id/workspaces/:workspace_id/runtime-services/:action",
            post(workspace_runtime_action),
        )
        .route(
            "/api/projects/:id/workspaces/:workspace_id/runtime-commands/:action",
            post(workspace_runtime_action),
        )
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => {
            ProjectRepo::new(&state.db)
                .list_by_company_no_filter(cid)
                .await?
        }
        None => ProjectRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = ProjectRepo::new(&state.db)
        .get_id_only(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("project {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let row = ProjectRepo::new(&state.db)
        .create_simple(body.company_id, &body.name, body.description.as_deref())
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("project.created", "project", row.id).with_company(row.company_id));
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "name": row.name, "status": row.status
        })),
    ))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = ProjectRepo::new(&state.db)
        .update(
            id,
            body.name.as_deref(),
            body.description.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("project {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("project.updated", "project", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = ProjectRepo::new(&state.db).delete_one(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("project {id}")))
    }
}

// ============== Sub-resource handlers ==============

async fn list_company_projects(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ProjectRepo::new(&state.db)
        .list_by_company(company_id, false)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn create_company_project(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::PipelinesWrite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let row = ProjectRepo::new(&state.db)
        .create_simple(company_id, &body.name, body.description.as_deref())
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("project.created", "project", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn list_project_workspaces(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ProjectRepo::new(&state.db).list_workspaces(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn list_project_goals(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ProjectRepo::new(&state.db).goals_for_project(id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn project_external_object_summary(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let project = ProjectRepo::new(&state.db)
        .get_id_only(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("project {id}")))?;
    let workspaces = ProjectRepo::new(&state.db)
        .list_workspaces(id)
        .await
        .unwrap_or_default();
    let goals = ProjectRepo::new(&state.db)
        .goals_for_project(id)
        .await
        .unwrap_or_default();
    let summary = json!({
        "projectId": project.id,
        "companyId": project.company_id,
        "workspaceCount": workspaces.len(),
        "workspaceSources": workspaces.iter().map(|w| w.source_type.as_str()).collect::<Vec<_>>(),
        "goalCount": goals.len(),
        "links": Vec::<Value>::new(),
        "files": Vec::<Value>::new(),
    });
    Ok(Json(summary))
}

// ============== Round 22: project workspace CRUD + runtime commands ==============

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectWorkspaceBody {
    name: String,
    cwd: String,
    repo_url: Option<String>,
    repo_ref: Option<String>,
    metadata: Option<Value>,
    is_primary: Option<bool>,
}

async fn create_project_workspace(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<CreateProjectWorkspaceBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    if body.cwd.trim().is_empty() {
        return Err(ApiError::BadRequest("cwd is required".into()));
    }
    let company_id = ProjectRepo::new(&state.db)
        .company_id_for_project(project_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("project {project_id}")))?;
    // If is_primary, unset other primary
    if body.is_primary.unwrap_or(false) {
        ProjectRepo::new(&state.db)
            .unset_all_primary_workspaces(project_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    let id = ProjectRepo::new(&state.db)
        .insert_workspace_simple(
            company_id,
            project_id,
            &body.name,
            &body.cwd,
            body.repo_url.as_deref(),
            body.repo_ref.as_deref(),
            body.metadata.clone().or(Some(json!({}))),
            body.is_primary,
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new("project_workspace.created", "project_workspace", id)
            .with_company(company_id)
            .with_data(json!({"projectId": project_id})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "companyId": company_id,
            "projectId": project_id,
            "name": body.name,
            "cwd": body.cwd,
            "repoUrl": body.repo_url,
            "repoRef": body.repo_ref,
            "metadata": body.metadata.unwrap_or_else(|| json!({})),
            "isPrimary": body.is_primary.unwrap_or(false),
        })),
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchProjectWorkspaceBody {
    name: Option<String>,
    cwd: Option<String>,
    repo_url: Option<String>,
    repo_ref: Option<String>,
    metadata: Option<Value>,
    is_primary: Option<bool>,
}

async fn patch_project_workspace(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchProjectWorkspaceBody>,
) -> ApiResult<Json<Value>> {
    // If is_primary=true, unset others first
    if body.is_primary.unwrap_or(false) {
        ProjectRepo::new(&state.db)
            .unset_other_primary_workspaces(project_id, workspace_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    let affected = ProjectRepo::new(&state.db)
        .patch_workspace_partial(
            workspace_id,
            project_id,
            body.name.as_deref(),
            body.cwd.as_deref(),
            body.repo_url.as_deref(),
            body.repo_ref.as_deref(),
            body.metadata.clone(),
            body.is_primary,
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if affected == 0 {
        return Err(ApiError::NotFound(format!(
            "project workspace {workspace_id}"
        )));
    }
    let cid = ProjectRepo::new(&state.db)
        .company_id_for_workspace_any(workspace_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if let Some(cid) = cid {
        state.realtime.publish(
            LiveEvent::new(
                "project_workspace.updated",
                "project_workspace",
                workspace_id,
            )
            .with_company(cid),
        );
    }
    Ok(Json(json!({
        "id": workspace_id,
        "projectId": project_id,
        "updated": true,
    })))
}

async fn delete_project_workspace(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let company_id = ProjectRepo::new(&state.db)
        .company_id_for_workspace(workspace_id, project_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("project workspace {workspace_id}")))?;
    let affected = ProjectRepo::new(&state.db)
        .delete_workspace_in_project(workspace_id, project_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if affected == 0 {
        return Err(ApiError::NotFound(format!(
            "project workspace {workspace_id}"
        )));
    }
    state.realtime.publish(
        LiveEvent::new(
            "project_workspace.deleted",
            "project_workspace",
            workspace_id,
        )
        .with_company(company_id),
    );
    Ok(Json(json!({
        "id": workspace_id,
        "projectId": project_id,
        "deleted": true,
    })))
}

async fn workspace_runtime_action(
    State(state): State<AppState>,
    Path((project_id, workspace_id, action)): Path<(Uuid, Uuid, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let allowed = ["start", "stop", "restart", "pause", "resume", "status"];
    if !allowed.contains(&action.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "invalid action '{action}', must be one of {allowed:?}"
        )));
    }
    let company_id = ProjectRepo::new(&state.db)
        .company_id_for_workspace(workspace_id, project_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("project workspace {workspace_id}")))?;
    // Append a runtime action to the workspace's metadata for audit
    let _ = ProjectRepo::new(&state.db)
        .append_runtime_action(workspace_id, &action)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()));
    state.realtime.publish(
        LiveEvent::new(
            format!("project_workspace.runtime_{action}"),
            "project_workspace",
            workspace_id,
        )
        .with_company(company_id)
        .with_data(json!({"projectId": project_id, "action": action, "body": body})),
    );
    Ok(Json(json!({
        "projectId": project_id,
        "workspaceId": workspace_id,
        "action": action,
        "accepted": true,
        "at": chrono::Utc::now(),
    })))
}
