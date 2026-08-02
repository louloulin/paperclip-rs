//! `/api/routines*` 路由：CRUD + trigger。

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
use pc_repos::routine::RoutineRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/routines", get(list).post(create))
        .route(
            "/api/routines/:id",
            get(get_one).patch(update).delete(remove),
        )
        .route("/api/routines/:id/trigger", post(trigger))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    company_id: Uuid,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = RoutineRepo::new(&state.db)
        .list_by_company(q.company_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = RoutineRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    assignee_agent_id: Option<Uuid>,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = RoutineRepo::new(&state.db)
        .create(
            body.company_id,
            &body.title,
            body.description.as_deref(),
            body.assignee_agent_id,
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("routine.created", "routine", row.id).with_company(row.company_id));
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "title": row.title,
            "status": row.status, "priority": row.priority
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
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
    let row = RoutineRepo::new(&state.db)
        .update(
            id,
            body.title.as_deref(),
            body.description.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("routine.updated", "routine", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn trigger(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = RoutineRepo::new(&state.db)
        .trigger(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("routine {id}")))?;
    state.realtime.publish(
        LiveEvent::new("routine.triggered", "routine", row.id).with_company(row.company_id),
    );
    Ok(Json(json!({
        "id": row.id, "last_triggered_at": row.last_triggered_at, "last_enqueued_at": row.last_enqueued_at
    })))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = RoutineRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("routine {id}")))
    }
}
