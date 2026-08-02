//! Sidebar company/project ordering preferences.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::get,
    Json, Router,
};
use pc_repos::sidebar::{normalize_ordered_ids, SidebarRepo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{state::require_user_id, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/sidebar-preferences/me",
            get(get_companies).put(put_companies),
        )
        .route(
            "/api/companies/:company_id/sidebar-preferences/me",
            get(get_projects).put(put_projects),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertBody {
    ordered_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreferenceResponse {
    ordered_ids: Vec<String>,
    updated_at: Option<pc_core::Timestamp>,
}

fn ordered_ids(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

async fn get_companies(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<PreferenceResponse>> {
    let user_id = require_user_id(&state, &headers).await?;
    let row = SidebarRepo::new(&state.db)
        .get_company_order(&user_id)
        .await?;
    Ok(Json(PreferenceResponse {
        ordered_ids: row
            .as_ref()
            .map_or_else(Vec::new, |row| ordered_ids(&row.company_order)),
        updated_at: row.map(|row| row.updated_at),
    }))
}

async fn put_companies(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpsertBody>,
) -> ApiResult<Json<PreferenceResponse>> {
    let user_id = require_user_id(&state, &headers).await?;
    let ids = normalize_ordered_ids(body.ordered_ids);
    let row = SidebarRepo::new(&state.db)
        .upsert_company_order(&user_id, &ids)
        .await?;
    Ok(Json(PreferenceResponse {
        ordered_ids: ordered_ids(&row.company_order),
        updated_at: Some(row.updated_at),
    }))
}

async fn get_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<PreferenceResponse>> {
    let user_id = require_user_id(&state, &headers).await?;
    let row = SidebarRepo::new(&state.db)
        .get_project_order(company_id, &user_id)
        .await?;
    Ok(Json(PreferenceResponse {
        ordered_ids: row
            .as_ref()
            .map_or_else(Vec::new, |row| ordered_ids(&row.project_order)),
        updated_at: row.map(|row| row.updated_at),
    }))
}

async fn put_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(body): Json<UpsertBody>,
) -> ApiResult<Json<PreferenceResponse>> {
    let user_id = require_user_id(&state, &headers).await?;
    let ids = normalize_ordered_ids(body.ordered_ids);
    let row = SidebarRepo::new(&state.db)
        .upsert_project_order(company_id, &user_id, &ids)
        .await?;
    Ok(Json(PreferenceResponse {
        ordered_ids: ordered_ids(&row.project_order),
        updated_at: Some(row.updated_at),
    }))
}
