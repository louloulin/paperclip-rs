//! `/api/environments*` 路由：CRUD（environments 不属于 company，全局共享）。

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
use pc_repos::environment::EnvironmentRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/environments", get(list).post(create))
        .route(
            "/api/environments/:id",
            get(get_one).patch(update).delete(remove),
        )
}

async fn list(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let rows = EnvironmentRepo::new(&state.db).list_all().await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("environment {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    name: String,
    #[serde(default = "default_driver")]
    driver: String,
    #[serde(default)]
    config: serde_json::Value,
}
fn default_driver() -> String {
    "local".into()
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let cfg = if body.config.is_null() {
        serde_json::json!({})
    } else {
        body.config
    };
    let row = EnvironmentRepo::new(&state.db)
        .create_simple(&body.name, &body.driver, cfg)
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("environment.created", "environment", row.id));
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "name": row.name, "driver": row.driver, "status": row.status
        })),
    ))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = EnvironmentRepo::new(&state.db)
        .update(id, body.name.as_deref(), body.status.as_deref(), body.config)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("environment {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("environment.updated", "environment", row.id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = EnvironmentRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("environment {id}")))
    }
}
