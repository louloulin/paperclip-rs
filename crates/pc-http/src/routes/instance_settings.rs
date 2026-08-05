//! Instance-wide settings singleton.

use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use pc_repos::agent::AgentRepo;
use pc_repos::case::CaseRepo;
use pc_repos::company::CompanyRepo;
use pc_repos::company_member::CompanyMemberRepo;
use pc_repos::issue::IssueRepo;
use pc_repos::settings::{InstanceSetting, SettingsRepo};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use pc_realtime::LiveEvent;

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
        // ---- Round 41: instance-level admin endpoints ----
        .route("/api/stats", get(get_instance_stats))
        .route("/api/dev-server/restart", post(restart_dev_server))
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
    Ok(Json(SettingsRepo::new(&state.db).get().await?))
}
async fn patch_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<InstanceSetting>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db)
            .patch_simple(body.default_environment_id, body.general, body.experimental)
            .await?,
    ))
}
async fn get_general(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_user_id(&state, &headers).await?;
    Ok(Json(
        SettingsRepo::new(&state.db).get().await?.general,
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
            .patch_simple(None, Some(value), None)
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
            .get()
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
            .patch_simple(None, None, Some(value))
            .await?
            .experimental,
    ))
}


// ============================================================================
// Round 41: instance-level stats + dev-server restart sentinel.
// ============================================================================

/// `GET /api/stats` — aggregate per-company counts (agents/issues/cases/users).
/// Mirrors Node `/stats`.  Synthesized via per-company SQL aggregations.
async fn get_instance_stats(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    let company_ids = CompanyRepo::new(&state.db)
        .list_ids()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let agents_repo = AgentRepo::new(&state.db);
    let issues_repo = IssueRepo::new(&state.db);
    let cases_repo = CaseRepo::new(&state.db);
    let members_repo = CompanyMemberRepo::new(&state.db);
    let mut out = serde_json::Map::new();
    for company_id in company_ids {
        let agents = agents_repo
            .count_for_company(company_id)
            .await
            .unwrap_or(0);
        let issues = issues_repo
            .count_visible_for_company(company_id)
            .await
            .unwrap_or(0);
        let cases = cases_repo
            .count_for_company(company_id)
            .await
            .unwrap_or(0);
        let users = members_repo
            .count_for_company(company_id)
            .await
            .unwrap_or(0);
        out.insert(company_id.to_string(), json!({
            "companyId": company_id,
            "agentCount": agents,
            "issueCount": issues,
            "caseCount": cases,
            "userCount": users,
        }));
    }
    Ok(Json(json!({
        "perCompany": out,
        "instance": {
            "totalCompanies": out.len(),
            "generatedAt": chrono::Utc::now(),
        }
    })))
}

/// `POST /api/dev-server/restart` — request dev-server supervisor to restart.
/// Mirrors Node `/dev-server/restart`.  Always returns 202 in our build; the
/// actual supervisor is a separate process that polls for the sentinel file.
async fn restart_dev_server(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    state.realtime.publish(
        LiveEvent::new("dev_server.restart_requested", "instance", Uuid::nil())
            .with_data(json!({
                "requestedAt": chrono::Utc::now(),
                "reason": "manual_restart_now",
            })),
    );
    Ok(Json(json!({
        "status": "restart_requested",
        "requestedAt": chrono::Utc::now(),
    })))
}
