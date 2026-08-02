//! 用户资料及投入统计路由。

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::get,
    Json, Router,
};
use pc_repos::user_profile::{UserProfileRepo, UserProfileResponse};
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/users/:user_slug/profile",
        get(get_profile),
    )
}

async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((company_id, user_slug)): Path<(Uuid, String)>,
) -> ApiResult<Json<UserProfileResponse>> {
    require_user_id(&state, &headers).await?;
    let profile = UserProfileRepo::new(&state.db)
        .load(company_id, &user_slug)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".to_owned()))?;
    Ok(Json(profile))
}
