//! Round 196: 全局邀请端点（无 company scope）。
//!
//! Node 等价：POST /invites/:inviteId/revoke
//!
//! 与 `/api/companies/:id/invites/:invite_id`（DELETE）的区别：
//! - DELETE scoped: 必须属于指定 company，否则 404
//! - POST global revoke: 不限 company，主要用于 bootstrap_ceo 等邀请

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_repos::invite::InviteRepo;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/invites/:invite_id/revoke",
        post(revoke_invite_by_id),
    )
}

async fn revoke_invite_by_id(
    State(state): State<AppState>,
    Path(invite_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let n = InviteRepo::new(&state.db)
        .revoke_by_id(invite_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if n == 0 {
        return Err(ApiError::NotFound(format!("invite {invite_id}")));
    }
    Ok((
        StatusCode::OK,
        Json(json!({
            "id": invite_id,
            "revoked": true,
        })),
    ))
}
