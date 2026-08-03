//! `/api/folders*` 路由：list + create + delete。
use crate::{ApiError, ApiResult, AppState};
#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use pc_repos::folder::FolderRepo;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/folders", get(list).post(create))
        .route("/api/folders/:id", delete(remove))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    company_id: Uuid,
}

async fn list(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        serde_json::to_value(FolderRepo::new(&s.db).list_by_company(q.company_id).await?)
            .unwrap_or_default(),
    ))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    company_id: Uuid,
    kind: String,
    name: String,
    slug: String,
}

async fn create(
    State(s): State<AppState>,
    Json(b): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let r = FolderRepo::new(&s.db)
        .create_legacy(b.company_id, &b.kind, &b.name, &b.slug)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(r).unwrap_or_default()),
    ))
}

async fn remove(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if FolderRepo::new(&s.db).delete_legacy(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("folder {id}")))
    }
}
