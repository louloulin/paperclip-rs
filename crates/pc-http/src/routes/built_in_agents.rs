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
use pc_repos::agent::{AgentRepo, AgentRow};

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
        "briefs",
        "Briefs Agent",
        "Prepares concise operational briefs for the board and agent company.",
    ),
    (
        "learning",
        "Learning Agent",
        "Maintains reusable company learning from completed work and recurring patterns.",
    ),
    (
        "reflection-coach",
        "Reflection Coach",
        "Runs evidence-backed reflection loops on recent agent work.",
    ),
    (
        "summarizer",
        "Summarizer",
        "Refreshes stale status summaries from grounded company work.",
    ),
];

const BUILT_IN_ALLOWED_ADAPTER_TYPES: &[&str] = &[
    "codex_local",
    "claude_local",
    "gemini_local",
    "opencode_local",
    "process",
];

fn built_in_definition_json(key: &str, display_name: &str, short_purpose: &str) -> Value {
    json!({
        "key": key,
        "displayName": display_name,
        "featureKeys": [key],
        "shortPurpose": short_purpose,
        "defaultInstructions": short_purpose,
        "defaultRole": "general",
        "allowedAdapterTypes": BUILT_IN_ALLOWED_ADAPTER_TYPES,
        "defaultAdapterType": BUILT_IN_ALLOWED_ADAPTER_TYPES[0],
        "defaultAdapterConfig": {},
        "defaultBudgetMonthlyCents": 0,
    })
}

fn has_complete_adapter_config(adapter_type: &str, config: &Value) -> bool {
    let Some(object) = config.as_object() else {
        return false;
    };
    let non_empty = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    match adapter_type {
        "process" | "command" => non_empty("command") || non_empty("script"),
        "http" => non_empty("url") || non_empty("endpoint") || non_empty("webhookUrl"),
        "openclaw_gateway" | "hermes_gateway" => non_empty("baseUrl") || non_empty("url"),
        _ => non_empty("model"),
    }
}

fn built_in_status(agent: Option<&AgentRow>) -> &'static str {
    let Some(agent) = agent else {
        return "not_provisioned";
    };
    if agent.status == "pending_approval" {
        return "pending_approval";
    }
    if agent.status == "paused" || agent.paused_at.is_some() {
        return "paused";
    }
    if has_complete_adapter_config(&agent.adapter_type, &agent.adapter_config) {
        "ready"
    } else {
        "needs_setup"
    }
}

fn redact_agent(agent: &AgentRow) -> Value {
    let mut redacted = agent.clone();
    redacted.adapter_config = json!({});
    redacted.runtime_config = json!({});
    serde_json::to_value(redacted).unwrap_or_else(|_| json!({}))
}

async fn built_in_state(
    state: &AppState,
    company_id: Uuid,
    key: &str,
) -> ApiResult<Value> {
    let (display_name, short_purpose) = BUILT_INS
        .iter()
        .find(|(candidate, _, _)| *candidate == key)
        .map(|(_, name, purpose)| (*name, *purpose))
        .ok_or_else(|| ApiError::NotFound(format!("built-in {key}")))?;
    let repo = AgentRepo::new(&state.db);
    let agent = match repo
        .find_built_in_agent_id(company_id, key)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?
    {
        Some(agent_id) => repo
            .get(agent_id)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?,
        None => None,
    };
    let status = built_in_status(agent.as_ref());
    Ok(json!({
        "definition": built_in_definition_json(key, display_name, short_purpose),
        "status": status,
        "agentId": agent.as_ref().map(|row| row.id),
        "agent": agent.as_ref().map(redact_agent),
        "pauseReason": agent.as_ref().and_then(|row| row.pause_reason.clone()),
        "resources": [],
        "approval": null,
    }))
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct EmptyBody {}

async fn list_built_in(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Vec<Value>>> {
    let mut states = Vec::with_capacity(BUILT_INS.len());
    for (key, _, _) in BUILT_INS {
        states.push(built_in_state(&state, company_id, key).await?);
    }
    Ok(Json(states))
}

async fn get_built_in_status(
    State(state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(built_in_state(&state, company_id, &key).await?))
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
