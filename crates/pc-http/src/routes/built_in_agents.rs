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
use pc_realtime::LiveEvent;
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
        // ── Round 200: provision（POST install-or-approve） ──
        .route(
            "/api/companies/:company_id/built-in-agents/:key/provision",
            post(provision_built_in),
        )
        // ── Round 200: routines enable/disable/run ──
        .route(
            "/api/companies/:company_id/built-in-agents/:key/routines/:routine_key/enable",
            post(enable_routine_schedule),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/routines/:routine_key/disable",
            post(disable_routine_schedule),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/routines/:routine_key/run",
            post(run_routine_now),
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

// ============================================================================
// Round 200: provision + routine schedule control
// ============================================================================

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct ProvisionBody {
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    initial_trigger: Option<Value>,
}

/// Provision a built-in agent (simplified — directly installs).
async fn provision_built_in(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
    Json(_body): Json<ProvisionBody>,
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
    let metadata = json!({
        "builtInKey": key,
        "source": "built-in",
        "provisionedAt": chrono::Utc::now().to_rfc3339(),
    });
    let agent_id = AgentRepo::new(&state.db)
        .install_built_in(company_id, def.1, role, &metadata)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::Conflict("built-in already provisioned".into()))?;
    state.realtime.publish(
        LiveEvent::new("built_in_agent.provisioned", "agent", agent_id).with_company(company_id),
    );
    Ok((
        StatusCode::OK,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "provisioned",
            "agentId": agent_id,
        })),
    ))
}

/// Toggle routine trigger schedule enabled flag.
async fn toggle_routine_trigger(
    state: &AppState,
    company_id: Uuid,
    agent_id: Uuid,
    routine_key: &str,
    enabled: bool,
) -> ApiResult<Value> {
    // Look up routine by title match
    let lookup: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM routines          WHERE company_id = $1 AND assignee_agent_id = $2 AND title = $3 LIMIT 1",
    )
    .bind(company_id)
    .bind(agent_id)
    .bind(routine_key)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (routine_id, routine_status) = lookup
        .ok_or_else(|| ApiError::NotFound(format!("routine {routine_key} for agent {agent_id}")))?;

    let updated = sqlx::query(
        "UPDATE routine_triggers SET enabled = $1, updated_at = now() WHERE routine_id = $2",
    )
    .bind(enabled)
    .bind(routine_id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .rows_affected();
    Ok(json!({
        "companyId": company_id,
        "agentId": agent_id,
        "routineKey": routine_key,
        "routineId": routine_id,
        "routineStatus": routine_status,
        "enabled": enabled,
        "triggersUpdated": updated,
    }))
}

async fn enable_routine_schedule(
    State(state): State<AppState>,
    Path((company_id, key, routine_key)): Path<(Uuid, String, String)>,
) -> ApiResult<Json<Value>> {
    let agent_id = AgentRepo::new(&state.db)
        .find_built_in_agent_id(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("built-in {key} not installed")))?;
    let result = toggle_routine_trigger(&state, company_id, agent_id, &routine_key, true).await?;
    state.realtime.publish(
        LiveEvent::new(
            "built_in_agent.routine_schedule_enabled",
            "routine",
            Uuid::nil(),
        )
        .with_company(company_id),
    );
    Ok(Json(result))
}

async fn disable_routine_schedule(
    State(state): State<AppState>,
    Path((company_id, key, routine_key)): Path<(Uuid, String, String)>,
) -> ApiResult<Json<Value>> {
    let agent_id = AgentRepo::new(&state.db)
        .find_built_in_agent_id(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("built-in {key} not installed")))?;
    let result = toggle_routine_trigger(&state, company_id, agent_id, &routine_key, false).await?;
    state.realtime.publish(
        LiveEvent::new(
            "built_in_agent.routine_schedule_disabled",
            "routine",
            Uuid::nil(),
        )
        .with_company(company_id),
    );
    Ok(Json(result))
}

/// Trigger a manual routine run (creates a routine_run row with source='manual').
async fn run_routine_now(
    State(state): State<AppState>,
    Path((company_id, key, routine_key)): Path<(Uuid, String, String)>,
) -> ApiResult<impl IntoResponse> {
    let agent_id = AgentRepo::new(&state.db)
        .find_built_in_agent_id(company_id, &key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("built-in {key} not installed")))?;

    let lookup: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM routines WHERE company_id = $1 AND assignee_agent_id = $2 AND title = $3 LIMIT 1",
    )
    .bind(company_id)
    .bind(agent_id)
    .bind(&routine_key)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (routine_id,) =
        lookup.ok_or_else(|| ApiError::NotFound(format!("routine {routine_key}")))?;

    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routine_runs (id, company_id, routine_id, source, status, triggered_at)          VALUES ($1, $2, $3, 'manual', 'received', now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(routine_id)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    state.realtime.publish(
        LiveEvent::new(
            "built_in_agent.routine_run_triggered",
            "routine_run",
            run_id,
        )
        .with_company(company_id),
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "agentId": agent_id,
            "routineKey": routine_key,
            "routineId": routine_id,
            "runId": run_id,
            "status": "received",
        })),
    ))
}
