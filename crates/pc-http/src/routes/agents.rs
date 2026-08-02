//! `/api/agents*` 路由：CRUD。

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

use pc_repos::agent::AgentRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agents", get(list).post(create))
        .route("/api/agents/:id", get(get_one).patch(update).delete(remove))
}

#[derive(Debug, Deserialize)]
struct ListQuery { #[serde(default)] company_id: Option<Uuid> }

async fn list(State(state): State<AppState>, axum::extract::Query(q): axum::extract::Query<ListQuery>) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => AgentRepo::new(&state.db).list_by_company(cid).await?,
        None => sqlx::query_as::<_, pc_repos::agent::AgentRow>(
            "SELECT id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                    adapter_type, adapter_config, runtime_config, default_environment_id, \
                    budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                    error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at \
             FROM agents ORDER BY created_at DESC",
        ).fetch_all(state.db.pool()).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = AgentRepo::new(&state.db).get(id).await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    name: String,
    #[serde(default = "default_role")] role: String,
    #[serde(default)] title: Option<String>,
    #[serde(default = "default_adapter")] adapter_type: String,
    #[serde(default)] adapter_config: serde_json::Value,
}
fn default_role() -> String { "general".into() }
fn default_adapter() -> String { "process".into() }

async fn create(State(state): State<AppState>, Json(body): Json<CreateBody>) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let row = AgentRepo::new(&state.db)
        .create(body.company_id, &body.name, &body.role, body.title.as_deref(),
                &body.adapter_type, body.adapter_config)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({
        "id": row.id, "company_id": row.company_id, "name": row.name,
        "role": row.role, "status": row.status
    }))))
}

#[derive(Debug, Deserialize)]
struct UpdateBody {
    #[serde(default)] name: Option<String>,
    #[serde(default)] role: Option<String>,
    #[serde(default)] title: Option<String>,
    #[serde(default)] status: Option<String>,
}

async fn update(State(state): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<UpdateBody>) -> ApiResult<Json<Value>> {
    let row = AgentRepo::new(&state.db)
        .update(id, body.name.as_deref(), body.role.as_deref(), body.title.as_deref(), body.status.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = AgentRepo::new(&state.db).delete(id).await?;
    if ok { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::NotFound(format!("agent {id}"))) }
}