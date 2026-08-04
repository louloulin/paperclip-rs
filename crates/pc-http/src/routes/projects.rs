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
