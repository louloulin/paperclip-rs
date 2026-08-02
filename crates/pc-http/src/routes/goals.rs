//! `/api/goals*` 路由。

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
use pc_repos::goal::GoalRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/goals", get(list).post(create))
        .route("/api/goals/:id", get(get_one).patch(update).delete(remove))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListQuery {
    company_id: Uuid,
}
async fn list(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        serde_json::to_value(GoalRepo::new(&s.db).list_by_company(q.company_id).await?)
            .unwrap_or_default(),
    ))
}
async fn get_one(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let r = GoalRepo::new(&s.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("goal {id}")))?;
    Ok(Json(serde_json::to_value(r).unwrap_or_default()))
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    owner_agent_id: Option<Uuid>,
}
async fn create(
    State(s): State<AppState>,
    Json(b): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if b.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title required".into()));
    }
    let r = GoalRepo::new(&s.db)
        .create(
            b.company_id,
            &b.title,
            b.description.as_deref(),
            b.owner_agent_id,
        )
        .await?;
    s.realtime
        .publish(LiveEvent::new("goal.created", "goal", r.id).with_company(r.company_id));
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":r.id,"title":r.title,"status":r.status})),
    ))
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
}
async fn update(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let r = GoalRepo::new(&s.db)
        .update(id, b.title.as_deref(), b.status.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("goal {id}")))?;
    s.realtime
        .publish(LiveEvent::new("goal.updated", "goal", r.id).with_company(r.company_id));
    Ok(Json(serde_json::to_value(r).unwrap_or_default()))
}
async fn remove(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if GoalRepo::new(&s.db).delete(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("goal {id}")))
    }
}
