//! `/api/cases*` 路由：CRUD。

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
use pc_repos::case::CaseRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/cases", get(list).post(create))
        .route("/api/cases/:case_id", get(get_one).patch(update).delete(remove))
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
        Some(cid) => CaseRepo::new(&state.db).list_by_company(cid).await?,
        None => CaseRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(case_id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    case_type: String,
    title: String,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    summary: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = CaseRepo::new(&state.db)
        .create(
            body.company_id,
            &body.case_type,
            &body.title,
            body.project_id,
            body.summary.as_deref(),
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("case.created", "case", row.id).with_company(row.company_id));
    let response = serde_json::json!({
            "id": row.id, "company_id": row.company_id, "title": row.title,
            "case_type": row.case_type, "status": row.status, "identifier": row.identifier
        });
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = CaseRepo::new(&state.db)
        .update(
            case_id,
            body.title.as_deref(),
            body.summary.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("case.updated", "case", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(case_id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = CaseRepo::new(&state.db).delete(case_id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("case {case_id}")))
    }
}
