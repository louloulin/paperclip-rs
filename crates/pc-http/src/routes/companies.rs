//! `/api/companies*` 路由：CRUD + 归档。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::company::{CompanyListRow, CompanyRepo};

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/companies", get(list).post(create))
        .route(
            "/api/companies/:id",
            get(get_one).patch(update).delete(remove),
        )
        .route("/api/companies/:id/archive", post(archive))
}

async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<CompanyListRow>>> {
    let rows = CompanyRepo::new(&state.db).list().await?;
    Ok(Json(rows))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = CompanyRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let row = CompanyRepo::new(&state.db)
        .create(&body.name, body.description.as_deref())
        .await?;
    let owner_id = match require_user_id(&state, &headers).await {
        Ok(user_id) => user_id,
        Err(ApiError::Unauthorized(_)) => "local-board".to_owned(),
        Err(error) => return Err(error),
    };
    sqlx::query(
        "INSERT INTO company_memberships \
            (company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, 'user', $2, 'active', 'owner') \
         ON CONFLICT (company_id, principal_type, principal_id) DO UPDATE SET \
            status = 'active', membership_role = COALESCE(company_memberships.membership_role, 'owner'), \
            updated_at = now()",
    )
    .bind(row.id)
    .bind(&owner_id)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("company.created", "company", row.id)
            .with_company(row.id)
            .with_actor("system"),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": row.id, "name": row.name, "status": row.status })),
    ))
}

#[derive(Debug, Deserialize)]
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
    let row = CompanyRepo::new(&state.db)
        .update(
            id,
            body.name.as_deref(),
            body.description.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("company.updated", "company", row.id).with_company(row.id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn archive(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = CompanyRepo::new(&state.db)
        .archive(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {id}")))?;
    Ok(Json(
        json!({ "id": row.id, "status": row.status, "archived_at": row.updated_at }),
    ))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = CompanyRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("company {id}")))
    }
}
