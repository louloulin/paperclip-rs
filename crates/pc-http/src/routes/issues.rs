//! `/api/issues*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, patch, delete},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::issue::IssueRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/issues", get(list).post(create))
        .route("/api/issues/:id", get(get_one).patch(update).delete(remove))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    company_id: Uuid,
    #[serde(default)] status: Option<String>,
}

async fn list(State(state): State<AppState>, axum::extract::Query(q): axum::extract::Query<ListQuery>) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db).list_by_company(q.company_id, q.status.as_deref()).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db).get(id).await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    title: String,
    #[serde(default)] description: Option<String>,
    #[serde(default = "default_priority")] priority: String,
    #[serde(default)] assignee_agent_id: Option<Uuid>,
}
fn default_priority() -> String { "medium".into() }

async fn create(State(state): State<AppState>, Json(body): Json<CreateBody>) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = IssueRepo::new(&state.db)
        .create(body.company_id, &body.title, body.description.as_deref(),
                &body.priority, body.assignee_agent_id)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({
        "id": row.id, "company_id": row.company_id, "title": row.title,
        "status": row.status, "priority": row.priority
    }))))
}

#[derive(Debug, Deserialize)]
struct UpdateBody {
    #[serde(default)] title: Option<String>,
    #[serde(default)] description: Option<String>,
    #[serde(default)] status: Option<String>,
    #[serde(default)] priority: Option<String>,
    #[serde(default)] assignee_agent_id: Option<Uuid>,
}

async fn update(State(state): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<UpdateBody>) -> ApiResult<Json<Value>> {
    let row = IssueRepo::new(&state.db)
        .update(id, body.title.as_deref(), body.description.as_deref(),
                body.status.as_deref(), body.priority.as_deref(),
                Some(body.assignee_agent_id))
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = IssueRepo::new(&state.db).delete(id).await?;
    if ok { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::NotFound(format!("issue {id}"))) }
}