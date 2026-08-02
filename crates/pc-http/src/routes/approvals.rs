//! `/api/approvals*` 路由：CRUD + 决策。

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
use pc_repos::approval::ApprovalRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/approvals", get(list).post(create))
        .route("/api/approvals/:id", get(get_one).delete(remove))
        .route("/api/approvals/:id/decide", post(decide))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    company_id: Uuid,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = ApprovalRepo::new(&state.db)
        .list_by_company(q.company_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = ApprovalRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("approval {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    approval_type: String,
    #[serde(default)]
    payload: serde_json::Value,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.approval_type.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "approval_type must not be empty".into(),
        ));
    }
    let payload = if body.payload.is_null() {
        serde_json::json!({})
    } else {
        body.payload
    };
    let row = ApprovalRepo::new(&state.db)
        .create(body.company_id, &body.approval_type, payload)
        .await?;
    state.realtime.publish(
        LiveEvent::new("approval.created", "approval", row.id).with_company(row.company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "approval_type": row.approval_type, "status": row.status
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct DecideBody {
    status: String,
    #[serde(default)]
    note: Option<String>,
    decided_by: String,
}

async fn decide(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<DecideBody>,
) -> ApiResult<Json<Value>> {
    if !["approved", "rejected", "cancelled"].contains(&body.status.as_str()) {
        return Err(ApiError::BadRequest(
            "status must be approved|rejected|cancelled".into(),
        ));
    }
    let row = ApprovalRepo::new(&state.db)
        .decide(id, &body.status, body.note.as_deref(), &body.decided_by)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("approval {id}")))?;
    state.realtime.publish(
        LiveEvent::new(format!("approval.{}", body.status), "approval", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = ApprovalRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("approval {id}")))
    }
}
