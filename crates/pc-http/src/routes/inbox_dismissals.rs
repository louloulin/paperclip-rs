//! User-scoped inbox dismissal and snooze state.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
    Json, Router,
};
use pc_core::Timestamp;
use pc_repos::inbox::{InboxDismissalRow, InboxRepo};
use serde::Deserialize;
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/inbox-dismissals",
            get(list).post(upsert),
        )
        .route(
            "/api/companies/:company_id/inbox-dismissals/:item_key",
            delete(restore),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissalBody {
    item_key: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    snoozed_until: Option<chrono::DateTime<chrono::Utc>>,
}
fn default_kind() -> String {
    "dismiss".into()
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Vec<InboxDismissalRow>>> {
    let user_id = require_user_id(&state, &headers).await?;
    Ok(Json(
        InboxRepo::new(&state.db)
            .list_for_user(company_id, &user_id)
            .await?,
    ))
}

async fn upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(body): Json<DismissalBody>,
) -> ApiResult<(StatusCode, Json<InboxDismissalRow>)> {
    let user_id = require_user_id(&state, &headers).await?;
    if !matches!(body.kind.as_str(), "dismiss" | "snooze") || body.item_key.trim().is_empty() {
        return Err(ApiError::BadRequest("invalid inbox dismissal".into()));
    }
    if body.kind == "snooze" && body.snoozed_until.is_none() {
        return Err(ApiError::BadRequest(
            "snoozedUntil is required for snooze".into(),
        ));
    }
    let row = InboxRepo::new(&state.db)
        .upsert_simple(
            company_id,
            &user_id,
            body.item_key.trim(),
            &body.kind,
            body.snoozed_until.map(Timestamp::from_dt),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((company_id, item_key)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let user_id = require_user_id(&state, &headers).await?;
    InboxRepo::new(&state.db)
        .restore(company_id, &user_id, &item_key)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
