//! Instance-wide settings singleton.

use axum::{extract::State, http::HeaderMap, routing::get, Json, Router};
use pc_repos::settings::{InstanceSetting, SettingsRepo};
use serde::Deserialize;
use uuid::Uuid;

use crate::{state::require_user_id, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/instance/settings", get(get_all).patch(patch_all))
        .route(
            "/api/instance/settings/general",
            get(get_general).patch(patch_general),
        )
        .route(
            "/api/instance/settings/experimental",
            get(get_experimental).patch(patch_experimental),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchBody {
    #[serde(default)]
    default_environment_id: Option<Uuid>,
    #[serde(default)]
    general: Option<serde_json::Value>,
    #[serde(default)]
    experimental: Option<serde_json::Value>,
}

async fn get_all(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<InstanceSetting>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(SettingsRepo::new(&state.db).get("default").await?))
}
async fn patch_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<InstanceSetting>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .patch(body.default_environment_id, body.general, body.experimental)
            .await?,
    ))
}
async fn get_general(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db).get("default").await?.general,
    ))
}
async fn patch_general(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .patch(None, Some(value), None)
            .await?
            .general,
    ))
}
async fn get_experimental(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .get("default")
            .await?
            .experimental,
    ))
}
async fn patch_experimental(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .patch(None, None, Some(value))
            .await?
            .experimental,
    ))
}
