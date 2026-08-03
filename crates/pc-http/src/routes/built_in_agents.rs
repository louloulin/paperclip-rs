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
    let pool = state.db.pool();
    let installed: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT metadata->>'builtInKey' FROM agents \
         WHERE company_id = $1 AND metadata->>'builtInKey' IS NOT NULL",
    )
    .bind(company_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let installed_keys: Vec<&str> = installed.iter().map(|(k,)| k.as_str()).collect();
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
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM agents WHERE company_id = $1 \
         AND metadata->>'builtInKey' = $2 LIMIT 1",
    )
    .bind(company_id)
    .bind(&key)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (status, agent_id) = match row {
        Some((id,)) => ("installed", Some(id)),
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
    sqlx::query(
        "UPDATE agents SET updated_at = now()          WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
    )
    .bind(company_id)
    .bind(&key)
    .execute(state.db.pool())
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
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO agents (company_id, name, role, status, adapter_type, metadata) \
         VALUES ($1, $2, $3, 'idle', 'codex_local', $4) \
         ON CONFLICT DO NOTHING \
         RETURNING id",
    )
    .bind(company_id)
    .bind(def.1)
    .bind(role)
    .bind(&metadata)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": if row.is_some() { "installed" } else { "already-installed" },
            "agentId": row.map(|(id,)| id),
        })),
    ))
}

async fn reset_built_in(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let _ = sqlx::query(
        "UPDATE agents SET status = 'idle', pause_reason = NULL, paused_at = NULL, updated_at = now() \
         WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
    )
    .bind(company_id)
    .bind(&key)
    .execute(state.db.pool())
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
    sqlx::query(
        "UPDATE agents SET status = 'archived', archived_at = now(), updated_at = now() \
         WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
    )
    .bind(company_id)
    .bind(&key)
    .execute(state.db.pool())
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
    sqlx::query(
        "UPDATE agents SET status = 'idle', archived_at = NULL, updated_at = now() \
         WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
    )
    .bind(company_id)
    .bind(&key)
    .execute(state.db.pool())
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
