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
        Some(cid) => ProjectRepo::new(&state.db).list_by_company_no_filter(cid).await?,
        None => ProjectRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = ProjectRepo::new(&state.db).get_id_only(id)
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
    let row = ProjectRepo::new(&state.db).create_simple(body.company_id, &body.name, body.description.as_deref())
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
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let row = ProjectRepo::new(&state.db)
        .create_simple(company_id, &body.name, body.description.as_deref())
        .await?;
    state.realtime.publish(
        LiveEvent::new("project.created", "project", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn list_project_workspaces(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ProjectRepo::new(&state.db)
        .list_workspaces(id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn list_project_goals(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ProjectRepo::new(&state.db)
        .goals_for_project(id)
        .await?;
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
    let company_id: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
    let (company_id,) = company_id.ok_or_else(|| ApiError::NotFound(format!("project {project_id}")))?;
    // If is_primary, unset other primary
    if body.is_primary.unwrap_or(false) {
        sqlx::query(
            "UPDATE project_workspaces SET is_primary = false WHERE project_id = $1",
        )
        .bind(project_id)
        .execute(state.db.pool())
        .await?;
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO project_workspaces (company_id, project_id, name, cwd, repo_url, repo_ref, metadata, is_primary) \
         VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, '{}'::jsonb), COALESCE($8, false)) RETURNING id",
    )
    .bind(company_id)
    .bind(project_id)
    .bind(&body.name)
    .bind(&body.cwd)
    .bind(body.repo_url.as_deref())
    .bind(body.repo_ref.as_deref())
    .bind(body.metadata.clone().unwrap_or_else(|| json!({})))
    .bind(body.is_primary.unwrap_or(false))
    .fetch_one(state.db.pool())
    .await?;
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
        sqlx::query("UPDATE project_workspaces SET is_primary = false WHERE project_id = $1 AND id <> $2")
            .bind(project_id)
            .bind(workspace_id)
            .execute(state.db.pool())
            .await?;
    }
    let affected = sqlx::query(
        "UPDATE project_workspaces SET \
            name = COALESCE($1, name), \
            cwd = COALESCE($2, cwd), \
            repo_url = COALESCE($3, repo_url), \
            repo_ref = COALESCE($4, repo_ref), \
            metadata = COALESCE($5, metadata), \
            is_primary = COALESCE($6, is_primary), \
            updated_at = now() \
         WHERE id = $7 AND project_id = $8",
    )
    .bind(body.name.as_deref())
    .bind(body.cwd.as_deref())
    .bind(body.repo_url.as_deref())
    .bind(body.repo_ref.as_deref())
    .bind(body.metadata.clone())
    .bind(body.is_primary)
    .bind(workspace_id)
    .bind(project_id)
    .execute(state.db.pool())
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound(format!("project workspace {workspace_id}")));
    }
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM project_workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
    if let Some((cid,)) = row {
        state.realtime.publish(
            LiveEvent::new("project_workspace.updated", "project_workspace", workspace_id)
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
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT company_id FROM project_workspaces WHERE id = $1 AND project_id = $2",
    )
    .bind(workspace_id)
    .bind(project_id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (company_id,) = row.ok_or_else(|| ApiError::NotFound(format!("project workspace {workspace_id}")))?;
    let affected = sqlx::query("DELETE FROM project_workspaces WHERE id = $1 AND project_id = $2")
        .bind(workspace_id)
        .bind(project_id)
        .execute(state.db.pool())
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound(format!("project workspace {workspace_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("project_workspace.deleted", "project_workspace", workspace_id)
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
        return Err(ApiError::BadRequest(format!("invalid action '{action}', must be one of {allowed:?}")));
    }
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT company_id FROM project_workspaces WHERE id = $1 AND project_id = $2",
    )
    .bind(workspace_id)
    .bind(project_id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (company_id,) = row.ok_or_else(|| ApiError::NotFound(format!("project workspace {workspace_id}")))?;
    // Append a runtime action to the workspace's metadata for audit
    let _ = sqlx::query(
        "UPDATE project_workspaces SET metadata = COALESCE(metadata, '{}'::jsonb) || jsonb_build_object('lastRuntimeAction', to_jsonb($1::text), 'lastRuntimeActionAt', to_jsonb(now())), updated_at = now() WHERE id = $2",
    )
    .bind(&action)
    .bind(workspace_id)
    .execute(state.db.pool())
    .await;
    state.realtime.publish(
        LiveEvent::new(format!("project_workspace.runtime_{action}"), "project_workspace", workspace_id)
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
