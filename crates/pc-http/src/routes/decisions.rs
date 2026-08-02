//! `/api/decisions*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::decision::DecisionRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/decisions", get(list).post(create))
        .route("/api/decisions/:id", get(get_one).delete(remove))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    company_id: Uuid,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = DecisionRepo::new(&state.db)
        .list_by_company(q.company_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = DecisionRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("decision {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    title: String,
    body: String,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() || body.body.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "title and body must not be empty".into(),
        ));
    }
    let row = DecisionRepo::new(&state.db)
        .create(body.company_id, &body.title, &body.body)
        .await?;
    state.realtime.publish(
        LiveEvent::new("decision.created", "decision", row.id).with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "title": row.title, "body": row.body, "status": row.status, "expires_at": row.expires_at
        })),
    ))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = DecisionRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("decision {id}")))
    }
}
