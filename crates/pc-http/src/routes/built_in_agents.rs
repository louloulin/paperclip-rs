//! 公司内置 agent（系统自带的 skill/template agent）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_repos::agent::AgentRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/built-in-agents",
            get(list_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/status",
            get(get_built_in_status),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/reconcile",
            post(reconcile_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/install",
            post(install_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/reset",
            post(reset_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/archive",
            post(archive_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/restore",
            post(restore_built_in),
        )
}

const BUILT_INS: &[(&str, &str, &str)] = &[
    (
        "code-reviewer",
        "Code Reviewer",
        "Reviews code for issues and proposes fixes.",
    ),
    (
        "doc-writer",
        "Doc Writer",
        "Writes and maintains documentation.",
    ),
    (
        "issue-triager",
        "Issue Triager",
        "Triages incoming issues and assigns priority.",
    ),
];

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct EmptyBody {}

async fn list_built_in(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Look up installed agents per built-in key
    let installed = AgentRepo::new(&state.db)
        .list_built_in_keys(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let installed_keys: Vec<&str> = installed.iter().map(|k| k.as_str()).collect();
    let items: Vec<Value> = BUILT_INS
        .iter()
        .map(|(key, name, desc)| {
            json!({
                "key": key,
                "name": name,
                "description": desc,
                "installed": installed_keys.contains(key),
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "available": items,
        "installedCount": installed_keys.len(),
    })))
}

async fn get_built_in_status(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let row = AgentRepo::new(&state.db)
        .find_built_in_agent_id(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (status, agent_id) = match row {
        Some(id) => ("installed", Some(id)),
        None => ("available", None),
    };
    Ok(Json(json!({
        "companyId": company_id,
        "key": key,
        "status": status,
        "agentId": agent_id,
    })))
}

async fn reconcile_built_in(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
    Json(_body): Json<EmptyBody>,
) -> ApiResult<impl IntoResponse> {
    AgentRepo::new(&state.db)
        .touch_built_in(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "reconciling"
        })),
    ))
}

async fn install_built_in(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let def = BUILT_INS
        .iter()
        .find(|(k, _, _)| *k == key)
        .ok_or_else(|| ApiError::NotFound(format!("built-in {key}")))?;
    let role = match key.as_str() {
        "code-reviewer" => "reviewer",
        "doc-writer" => "writer",
        "issue-triager" => "triager",
        _ => "assistant",
    };
    // Idempotent insert keyed by (company_id, metadata.builtInKey)
    let metadata = json!({ "builtInKey": key, "source": "built-in" });
    let row = AgentRepo::new(&state.db)
        .install_built_in(company_id, def.1, role, &metadata)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": if row.is_some() { "installed" } else { "already-installed" },
            "agentId": row,
        })),
    ))
}

async fn reset_built_in(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let _ = AgentRepo::new(&state.db)
        .reset_built_in(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "reset-queued"
        })),
    ))
}

async fn archive_built_in(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    AgentRepo::new(&state.db)
        .archive_built_in(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "archive-queued"
        })),
    ))
}

async fn restore_built_in(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    AgentRepo::new(&state.db)
        .restore_built_in(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "restore-queued"
        })),
    ))
}
