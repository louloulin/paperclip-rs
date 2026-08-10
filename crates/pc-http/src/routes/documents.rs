//! `/api/documents*` 路由：CRUD。
use axum::Extension as AxumExtension;
use pc_auth::AuthContext;
use pc_authz::{enforce_permission, PermissionKey};
use sqlx;

use crate::{ApiError, ApiResult, AppState};
#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use pc_realtime::LiveEvent;
use pc_repos::document::DocumentRepo;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/documents", get(list).post(create))
        .route(
            "/api/documents/:id",
            get(get_one).patch(update).delete(remove),
        )
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
        serde_json::to_value(
            DocumentRepo::new(&s.db)
                .list_by_company(q.company_id)
                .await?,
        )
        .unwrap_or_default(),
    ))
}
async fn get_one(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let r = DocumentRepo::new(&s.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {id}")))?;
    Ok(Json(serde_json::to_value(r).unwrap_or_default()))
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    #[serde(default)]
    title: Option<String>,
    body: String,
}
async fn create(
    State(s): State<AppState>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(b): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    // pc-authz：创建 document 需要 UsersInvite 权限
    if let Err(err) =
        enforce_permission(&s.db, &actor, b.company_id, PermissionKey::UsersInvite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let r = DocumentRepo::new(&s.db)
        .create(b.company_id, b.title.as_deref(), &b.body)
        .await?;
    s.realtime
        .publish(LiveEvent::new("document.created", "document", r.id).with_company(r.company_id));
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":r.id,"title":r.title,"format":r.format})),
    ))
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
}
async fn update(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    // pc-authz：查 company_id
    let preview: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM documents WHERE id = $1")
        .bind(id)
        .fetch_optional(s.db.pool())
        .await?;
    let preview_company_id = preview
        .ok_or_else(|| ApiError::NotFound(format!("document {id}")))?
        .0;
    if let Err(err) = enforce_permission(
        &s.db,
        &actor,
        preview_company_id,
        PermissionKey::UsersInvite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let r = DocumentRepo::new(&s.db)
        .update(id, b.title.as_deref(), b.body.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("document {id}")))?;
    s.realtime
        .publish(LiveEvent::new("document.updated", "document", r.id).with_company(r.company_id));
    Ok(Json(serde_json::to_value(r).unwrap_or_default()))
}
async fn remove(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if DocumentRepo::new(&s.db).delete(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("document {id}")))
    }
}
