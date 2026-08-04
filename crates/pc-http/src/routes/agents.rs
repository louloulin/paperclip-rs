//! `/api/agents*` 路由：CRUD。

use std::sync::Arc;

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_adapter_api::{AdapterEvent, AdapterExecutionContext, OutputStream};
use pc_agent::{
    AgentConfigSnapshot, AgentPatch, AgentPermissionUpdate, AgentService, ApproveAgentCommand,
    ClearAgentErrorCommand, CreateAgent, CreateAgentCommand, CreateAgentKey, CreateAgentKeyCommand,
    HireAgentCommand, InstructionAgent, InstructionsBundleUpdate, PauseAgentCommand, PauseReason,
    ResetRuntimeSession, ResetRuntimeSessionCommand, ResumeAgentCommand, RevisionContext,
    RevokeAgentKeyCommand, RollbackConfigRevisionCommand, TerminateAgentCommand,
    UpdateAgentCommand, UpdateAgentPermissionsCommand,
};
use pc_core::actor_runtime::kameo_api::SendError;
use pc_core::Timestamp;
use pc_heartbeat::{
    evaluate_daily_cap, utc_day_window, FinishHeartbeat, HeartbeatExecutionOutcome,
    HeartbeatExecutionSink, HeartbeatOutcome, HeartbeatPolicy, LaunchHeartbeatExecution,
    StartHeartbeat, StartHeartbeatResult,
};
use pc_realtime::LiveEvent;
use pc_repos::agent::{AgentRepo, AgentRow};
use pc_repos::cost::CostRepo;
use pc_repos::execution::ExecutionRepo;
use pc_repos::skill::SkillRepo;
use pc_repos::heartbeat::{CreateHeartbeat, HeartbeatRepo, HeartbeatRow};
use pc_repos::issue::IssueRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agents", get(list).post(create))
        .route(
            "/api/companies/:company_id/agents",
            get(list_company_agents).post(create_company_agent),
        )
        .route("/api/companies/:company_id/agent-hires", post(hire_agent))
        .route("/api/agents/:id", get(get_one).patch(update).delete(remove))
        .route("/api/agents/:id/configuration", get(get_configuration))
        .route(
            "/api/agents/:id/config-revisions",
            get(list_config_revisions),
        )
        .route(
            "/api/agents/:id/config-revisions/:revision_id",
            get(get_config_revision),
        )
        .route(
            "/api/agents/:id/config-revisions/:revision_id/rollback",
            post(rollback_config_revision),
        )
        .route("/api/agents/:id/wakeup", post(wakeup))
        .route("/api/agents/:id/pause", post(pause_agent))
        .route("/api/agents/:id/resume", post(resume_agent))
        .route("/api/agents/:id/clear-error", post(clear_agent_error))
        .route("/api/agents/:id/terminate", post(terminate_agent))
        .route("/api/agents/:id/approve", post(approve_agent))
        .route(
            "/api/agents/:id/permissions",
            patch(update_agent_permissions),
        )
        .route(
            "/api/agents/:id/instructions-path",
            patch(update_instructions_path),
        )
        .route(
            "/api/agents/:id/instructions-bundle",
            get(get_instructions_bundle).patch(update_instructions_bundle),
        )
        .route(
            "/api/agents/:id/instructions-bundle/file",
            get(get_instructions_file)
                .put(put_instructions_file)
                .delete(delete_instructions_file),
        )
        .route("/api/agents/:id/runtime-state", get(get_runtime_state))
        .route("/api/agents/:id/task-sessions", get(list_task_sessions))
        .route(
            "/api/agents/:id/runtime-state/reset-session",
            post(reset_runtime_session),
        )
        .route(
            "/api/agents/:id/keys",
            get(list_agent_keys).post(create_agent_key),
        )
        .route("/api/agents/:id/keys/:key_id", delete(revoke_agent_key))
        .route("/api/agents/:id/heartbeat/invoke", post(legacy_invoke))
        .route(
            "/api/companies/:company_id/heartbeat-runs",
            get(list_heartbeat_runs),
        )
        .route("/api/heartbeat-runs/:run_id", get(get_heartbeat_run))
        .route(
            "/api/heartbeat-runs/:run_id/cancel",
            post(cancel_heartbeat_run),
        )
        .route(
            "/api/heartbeat-runs/:run_id/events",
            get(list_heartbeat_events),
        )
        .route(
            "/api/heartbeat-runs/:run_id/log",
            get(read_heartbeat_log),
        )
        .route(
            "/api/heartbeat-runs/:run_id/watchdog-decisions",
            get(list_watchdog_decisions),
        )
        .route(
            "/api/heartbeat-runs/:run_id/workspace-operations",
            get(list_heartbeat_workspace_operations),
        )
        .route(
            "/api/agents/:id/skills",
            get(list_agent_skills),
        )
        .route(
            "/api/agents/:id/skills/sync",
            post(sync_agent_skills),
        )
        .route(
            "/api/agents/:id/budgets",
            get(get_agent_budgets).patch(update_agent_budgets),
        )
        .route(
            "/api/agents/:id/claude-login",
            post(claude_login),
        )
        .route(
            "/api/companies/:company_id/agent-configurations",
            get(list_agent_configurations),
        )
        .route(
            "/api/companies/:company_id/live-runs",
            get(list_company_live_runs),
        )
        .route(
            "/api/issues/:issue_id/active-run",
            get(get_issue_active_run),
        )
        .route(
            "/api/issues/:issue_id/live-runs",
            get(list_issue_live_runs),
        )
        .route(
            "/api/instance/scheduler-heartbeats",
            get(list_instance_scheduler_heartbeats),
        )
}

async fn list_company_agents(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Vec<AgentRow>>> {
    let rows = AgentRepo::new(&state.db)
        .list_by_company(company_id)
        .await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => AgentRepo::new(&state.db).list_by_company(cid).await?,
        None => sqlx::query_as::<_, pc_repos::agent::AgentRow>(
            "SELECT id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                    adapter_type, adapter_config, runtime_config, default_environment_id, \
                    budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                    error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at \
             FROM agents ORDER BY created_at DESC",
        )
        .fetch_all(state.db.pool())
        .await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = AgentRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    name: String,
    #[serde(default = "default_role")]
    role: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    reports_to: Option<Uuid>,
    #[serde(default)]
    capabilities: Option<String>,
    #[serde(default = "default_adapter")]
    adapter_type: String,
    #[serde(default)]
    adapter_config: serde_json::Value,
    #[serde(default)]
    runtime_config: serde_json::Value,
    #[serde(default)]
    default_environment_id: Option<Uuid>,
    #[serde(default)]
    budget_monthly_cents: i32,
    #[serde(default)]
    permissions: serde_json::Value,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}
fn default_role() -> String {
    "general".into()
}
fn default_adapter() -> String {
    "process".into()
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<LegacyCreateBody>,
) -> ApiResult<impl IntoResponse> {
    create_agent(&state, body.company_id, body.agent).await
}

#[derive(Debug, Deserialize)]
struct LegacyCreateBody {
    #[serde(alias = "companyId")]
    company_id: Uuid,
    #[serde(flatten)]
    agent: CreateBody,
}

async fn create_company_agent(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    create_agent(&state, company_id, body).await
}

async fn create_agent(
    state: &AppState,
    company_id: Uuid,
    body: CreateBody,
) -> ApiResult<impl IntoResponse> {
    let input = create_agent_input(company_id, body)?;
    let row = state
        .agents
        .ask(CreateAgentCommand(input))
        .await
        .map_err(map_agent_actor_error)?;
    state.realtime.publish(
        LiveEvent::new("agent.created", "agent", row.id)
            .with_company(row.company_id)
            .with_actor("system"),
    );
    Ok((StatusCode::CREATED, Json(row)))
}

fn create_agent_input(company_id: Uuid, body: CreateBody) -> ApiResult<CreateAgent> {
    if body.budget_monthly_cents < 0 {
        return Err(ApiError::BadRequest(
            "budgetMonthlyCents must be nonnegative".into(),
        ));
    }
    Ok(CreateAgent {
        company_id,
        name: body.name,
        role: body.role,
        title: body.title,
        icon: body.icon,
        reports_to: body.reports_to,
        capabilities: body.capabilities,
        adapter_type: body.adapter_type,
        adapter_config: body.adapter_config,
        runtime_config: body.runtime_config,
        default_environment_id: body.default_environment_id,
        budget_monthly_cents: body.budget_monthly_cents,
        permissions: body.permissions,
        metadata: body.metadata,
        ..CreateAgent::default()
    })
}

async fn hire_agent(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let result = state
        .agents
        .ask(HireAgentCommand {
            input: create_agent_input(company_id, body)?,
            actor: RevisionContext::user("local-board", "hire"),
        })
        .await
        .map_err(map_agent_actor_error)?;
    state.realtime.publish(
        LiveEvent::new("agent.hired", "agent", result.agent.id)
            .with_company(result.agent.company_id)
            .with_data(json!({
                "approvalId": result.approval.as_ref().map(|approval| approval.id),
                "status": result.agent.status,
            })),
    );
    Ok((StatusCode::CREATED, Json(serde_json::to_value(result)?)))
}

fn map_agent_actor_error<M>(error: SendError<M, pc_errors::Error>) -> ApiError {
    match error {
        SendError::HandlerError(error) => error.into(),
        _ => ApiError::Internal("agent supervisor unavailable".into()),
    }
}

#[derive(Debug, Default)]
enum PatchField<T> {
    #[default]
    Missing,
    Value(Option<T>),
}

impl<'de, T> Deserialize<'de> for PatchField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Self::Value)
    }
}

impl<T> PatchField<T> {
    fn into_patch(self) -> Option<Option<T>> {
        match self {
            Self::Missing => None,
            Self::Value(value) => Some(value),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    title: PatchField<String>,
    #[serde(default)]
    icon: PatchField<String>,
    #[serde(default)]
    reports_to: PatchField<Uuid>,
    #[serde(default)]
    capabilities: PatchField<String>,
    #[serde(default)]
    adapter_type: Option<String>,
    #[serde(default)]
    adapter_config: Option<Value>,
    #[serde(default)]
    runtime_config: Option<Value>,
    #[serde(default)]
    default_environment_id: PatchField<Uuid>,
    #[serde(default)]
    budget_monthly_cents: Option<i32>,
    #[serde(default)]
    metadata: PatchField<Value>,
    #[serde(default)]
    permissions: Option<Value>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    if body.permissions.is_some() {
        return Err(ApiError::BadRequest(
            "permissions must be updated through /permissions".into(),
        ));
    }
    if body.budget_monthly_cents.is_some_and(|value| value < 0) {
        return Err(ApiError::BadRequest(
            "budgetMonthlyCents must be nonnegative".into(),
        ));
    }
    let row = state
        .agents
        .ask(UpdateAgentCommand {
            id,
            patch: AgentPatch {
                name: body.name,
                role: body.role,
                title: body.title.into_patch(),
                icon: body.icon.into_patch(),
                reports_to: body.reports_to.into_patch(),
                capabilities: body.capabilities.into_patch(),
                adapter_type: body.adapter_type,
                adapter_config: body.adapter_config,
                runtime_config: body.runtime_config,
                default_environment_id: body.default_environment_id.into_patch(),
                budget_monthly_cents: body.budget_monthly_cents,
                metadata: body.metadata.into_patch(),
            },
            revision: RevisionContext::user("local-board", "patch"),
        })
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("agent.updated", "agent", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn get_configuration(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = AgentService::new(state.db.clone())
        .get(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    Ok(Json(serde_json::to_value(
        AgentConfigSnapshot::from(&row).sanitized(),
    )?))
}

async fn list_config_revisions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    if AgentRepo::new(&state.db).get(id).await?.is_none() {
        return Err(ApiError::NotFound(format!("agent {id}")));
    }
    let rows = AgentService::new(state.db.clone())
        .list_config_revisions(id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::to_value(rows)?))
}

async fn get_config_revision(
    State(state): State<AppState>,
    Path((id, revision_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = AgentService::new(state.db.clone())
        .get_config_revision(id, revision_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::NotFound(format!("revision {revision_id}")))?;
    Ok(Json(serde_json::to_value(row)?))
}

async fn rollback_config_revision(
    State(state): State<AppState>,
    Path((id, revision_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(RollbackConfigRevisionCommand {
            id,
            revision_id,
            actor: RevisionContext::user("local-board", "rollback"),
        })
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("revision {revision_id}")))?;
    state.realtime.publish(
        LiveEvent::new("agent.config_rolled_back", "agent", row.id)
            .with_company(row.company_id)
            .with_data(json!({"revisionId": revision_id})),
    );
    Ok(Json(serde_json::to_value(row)?))
}

async fn pause_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(PauseAgentCommand {
            id,
            reason: PauseReason::Manual,
        })
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("agent.paused", "agent", id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row)?))
}

async fn resume_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(ResumeAgentCommand(id))
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("agent.resumed", "agent", id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row)?))
}

async fn clear_agent_error(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(ClearAgentErrorCommand(id))
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("agent.error_cleared", "agent", id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row)?))
}

async fn terminate_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(TerminateAgentCommand(id))
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("agent.terminated", "agent", id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row)?))
}

async fn approve_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(ApproveAgentCommand(id))
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("agent.approved", "agent", id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAgentPermissionsBody {
    can_create_agents: bool,
    #[serde(default)]
    can_create_skills: Option<bool>,
    can_assign_tasks: bool,
    #[serde(default)]
    trust_preset: Option<Value>,
    #[serde(default)]
    authorization_policy: Option<Value>,
}

async fn update_agent_permissions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateAgentPermissionsBody>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(UpdateAgentPermissionsCommand {
            id,
            input: AgentPermissionUpdate {
                can_create_agents: body.can_create_agents,
                can_create_skills: body.can_create_skills,
                can_assign_tasks: body.can_assign_tasks,
                trust_preset: body.trust_preset,
                authorization_policy: body.authorization_policy,
                granted_by_user_id: Some("local-board".into()),
            },
        })
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state.realtime.publish(
        LiveEvent::new("agent.permissions_updated", "agent", id).with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row)?))
}

async fn get_instructions_bundle(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let bundle = state
        .agent_instructions
        .get_bundle(&InstructionAgent::from(&agent))
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::to_value(bundle)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInstructionsBundleBody {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    root_path: PatchField<String>,
    #[serde(default)]
    entry_file: Option<String>,
    #[serde(default)]
    clear_legacy_prompt_template: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInstructionsPathBody {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    adapter_config_key: Option<String>,
}

async fn update_instructions_path(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateInstructionsPathBody>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let adapter_config_key = body
        .adapter_config_key
        .clone()
        .unwrap_or_else(|| "instructionsFilePath".to_owned());
    if adapter_config_key != "instructionsFilePath" {
        return Err(ApiError::Unprocessable(format!(
            "No default instructions path key '{adapter_config_key}' is supported; use instructionsFilePath"
        )));
    }
    let path = body
        .path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(relative) = path {
        if std::path::Path::new(relative).is_relative() {
            return Err(ApiError::Unprocessable(
                "Relative instructions path requires adapterConfig.cwd to be set to an absolute path"
                    .into(),
            ));
        }
    }
    let next_config = state
        .agent_instructions
        .sync_bundle_config_from_path(&InstructionAgent::from(&agent), path)
        .map_err(ApiError::from)?;
    let value = Value::Object(next_config.clone());
    persist_instructions_config(&state, id, value, "instructions_path_patch").await?;
    let stored_path = next_config
        .get("instructionsFilePath")
        .and_then(Value::as_str)
        .map(str::to_owned);
    state.realtime.publish(
        LiveEvent::new("agent.instructions_path_updated", "agent", id)
            .with_company(agent.company_id)
            .with_data(json!({
                "adapterConfigKey": adapter_config_key,
                "path": stored_path,
                "cleared": path.is_none(),
            })),
    );
    Ok(Json(json!({
        "agentId": id,
        "adapterType": agent.adapter_type,
        "adapterConfigKey": adapter_config_key,
        "path": stored_path,
    })))
}

async fn update_instructions_bundle(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateInstructionsBundleBody>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let result = state
        .agent_instructions
        .update_bundle(
            &InstructionAgent::from(&agent),
            InstructionsBundleUpdate {
                mode: body.mode,
                root_path: body.root_path.into_patch(),
                entry_file: body.entry_file,
                clear_legacy_prompt_template: body.clear_legacy_prompt_template,
            },
        )
        .await
        .map_err(ApiError::from)?;
    persist_instructions_config(
        &state,
        id,
        result.adapter_config,
        "instructions_bundle_patch",
    )
    .await?;
    state.realtime.publish(
        LiveEvent::new("agent.instructions_bundle_updated", "agent", id)
            .with_company(agent.company_id),
    );
    Ok(Json(serde_json::to_value(result.bundle)?))
}

#[derive(Debug, Deserialize)]
struct InstructionsFileQuery {
    #[serde(default)]
    path: Option<String>,
}

fn required_instruction_path(path: Option<String>) -> ApiResult<String> {
    path.map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::Unprocessable("Query parameter 'path' is required".into()))
}

async fn get_instructions_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<InstructionsFileQuery>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let file = state
        .agent_instructions
        .read_file(
            &InstructionAgent::from(&agent),
            &required_instruction_path(query.path)?,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::to_value(file)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutInstructionsFileBody {
    path: String,
    content: String,
    #[serde(default)]
    clear_legacy_prompt_template: bool,
}

async fn put_instructions_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PutInstructionsFileBody>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let result = state
        .agent_instructions
        .write_file(
            &InstructionAgent::from(&agent),
            &body.path,
            &body.content,
            body.clear_legacy_prompt_template,
        )
        .await
        .map_err(ApiError::from)?;
    persist_instructions_config(
        &state,
        id,
        result.adapter_config,
        "instructions_bundle_file_put",
    )
    .await?;
    state.realtime.publish(
        LiveEvent::new("agent.instructions_file_updated", "agent", id)
            .with_company(agent.company_id)
            .with_data(json!({"path": result.file.path, "size": result.file.size})),
    );
    Ok(Json(serde_json::to_value(result.file)?))
}

async fn delete_instructions_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<InstructionsFileQuery>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let path = required_instruction_path(query.path)?;
    let result = state
        .agent_instructions
        .delete_file(&InstructionAgent::from(&agent), &path)
        .await
        .map_err(ApiError::from)?;
    persist_instructions_config(
        &state,
        id,
        result.adapter_config,
        "instructions_bundle_file_delete",
    )
    .await?;
    state.realtime.publish(
        LiveEvent::new("agent.instructions_file_deleted", "agent", id)
            .with_company(agent.company_id)
            .with_data(json!({"path": path})),
    );
    Ok(Json(serde_json::to_value(result.bundle)?))
}

async fn load_agent(state: &AppState, id: Uuid) -> ApiResult<AgentRow> {
    AgentRepo::new(&state.db)
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))
}

async fn persist_instructions_config(
    state: &AppState,
    id: Uuid,
    adapter_config: Value,
    source: &str,
) -> ApiResult<()> {
    state
        .agents
        .ask(UpdateAgentCommand {
            id,
            patch: AgentPatch {
                adapter_config: Some(adapter_config),
                ..AgentPatch::default()
            },
            revision: RevisionContext::user("local-board", source),
        })
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    Ok(())
}

async fn get_runtime_state(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = AgentService::new(state.db.clone())
        .runtime_state(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    Ok(Json(serde_json::to_value(row)?))
}

async fn list_task_sessions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = AgentService::new(state.db.clone())
        .list_task_sessions(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    Ok(Json(serde_json::to_value(rows)?))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetRuntimeSessionBody {
    #[serde(default)]
    task_key: Option<String>,
}

async fn reset_runtime_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ResetRuntimeSessionBody>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(ResetRuntimeSessionCommand {
            id,
            input: ResetRuntimeSession {
                task_key: body.task_key,
            },
        })
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state.realtime.publish(
        LiveEvent::new("agent.runtime_session_reset", "agent", id).with_company(row.company_id),
    );
    Ok(Json(serde_json::to_value(row)?))
}

async fn list_agent_keys(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    if AgentRepo::new(&state.db).get(id).await?.is_none() {
        return Err(ApiError::NotFound(format!("agent {id}")));
    }
    let rows = AgentService::new(state.db.clone())
        .list_api_keys(id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::to_value(rows)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateAgentKeyBody {
    name: String,
    #[serde(default = "standard_key_scope")]
    scope: Value,
    #[serde(default)]
    responsible_user_id: Option<String>,
}

fn standard_key_scope() -> Value {
    json!({"kind": "standard"})
}

async fn create_agent_key(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateAgentKeyBody>,
) -> ApiResult<impl IntoResponse> {
    let row = state
        .agents
        .ask(CreateAgentKeyCommand {
            id,
            input: CreateAgentKey {
                name: body.name,
                responsible_user_id: body.responsible_user_id,
                scope: body.scope,
            },
        })
        .await
        .map_err(map_agent_actor_error)?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(row)?)))
}

async fn revoke_agent_key(
    State(state): State<AppState>,
    Path((id, key_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = state
        .agents
        .ask(RevokeAgentKeyCommand {
            agent_id: id,
            key_id,
        })
        .await
        .map_err(map_agent_actor_error)?
        .ok_or_else(|| ApiError::NotFound(format!("agent key {key_id}")))?;
    Ok(Json(serde_json::to_value(row)?))
}

async fn remove(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = AgentRepo::new(&state.db).delete(id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("agent {id}")))
    }
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct WakeBody {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    trigger_detail: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    force_fresh_session: bool,
}

fn normalize_wake_source(source: Option<&str>) -> &str {
    source
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("on_demand")
}

async fn wakeup(
    State(state): State<AppState>,
    Path(agent_id): Path<Uuid>,
    Json(body): Json<WakeBody>,
) -> ApiResult<impl IntoResponse> {
    create_heartbeat_run(state, agent_id, body, false).await
}

async fn legacy_invoke(
    State(state): State<AppState>,
    Path(agent_id): Path<Uuid>,
    body: Option<Json<WakeBody>>,
) -> ApiResult<impl IntoResponse> {
    create_heartbeat_run(
        state,
        agent_id,
        body.map_or_else(WakeBody::default, |Json(body)| body),
        true,
    )
    .await
}

async fn create_heartbeat_run(
    state: AppState,
    agent_id: Uuid,
    body: WakeBody,
    legacy: bool,
) -> ApiResult<impl IntoResponse> {
    let agent = AgentRepo::new(&state.db)
        .get(agent_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {agent_id}")))?;
    if matches!(agent.status.as_str(), "paused" | "terminated") {
        return Err(ApiError::BadRequest(format!(
            "agent cannot be woken while status is {}",
            agent.status
        )));
    }

    let source = if legacy {
        "on_demand"
    } else {
        normalize_wake_source(body.source.as_deref())
    };
    let trigger_detail = body.trigger_detail.as_deref().unwrap_or("manual");
    let prompt = body
        .reason
        .as_deref()
        .filter(|reason| !reason.trim().is_empty())
        .unwrap_or("Run your Paperclip heartbeat and report useful progress.")
        .to_owned();
    let context_snapshot = json!({
        "reason": body.reason,
        "payload": body.payload,
        "forceFreshSession": body.force_fresh_session,
    });
    let repo = HeartbeatRepo::new(&state.db);
    let queued = repo
        .create(CreateHeartbeat {
            company_id: agent.company_id,
            agent_id,
            invocation_source: source,
            trigger_detail: Some(trigger_detail),
            responsible_user_id: None,
            wakeup_request_id: None,
            context_snapshot: Some(context_snapshot),
        })
        .await?;

    let start = state
        .heartbeat
        .ask(StartHeartbeat { run_id: queued.id })
        .await;
    let mut run = match start {
        Ok(StartHeartbeatResult::Started | StartHeartbeatResult::AlreadyActive) => {
            repo.mark_running(queued.id).await?.unwrap_or(queued)
        }
        Err(pc_core::actor_runtime::kameo_api::SendError::HandlerError(
            pc_heartbeat::HeartbeatSupervisorError::CapacityExceeded { .. },
        )) => queued,
        Err(error) => return Err(ApiError::Internal(error.to_string())),
    };
    repo.append_event(
        &run,
        if run.status == "running" {
            "run.started"
        } else {
            "run.queued"
        },
        None,
        None,
    )
    .await?;
    state.realtime.publish(
        LiveEvent::new("heartbeat.run.started", "heartbeat_run", run.id)
            .with_company(run.company_id)
            .with_data(json!({ "agentId": run.agent_id, "status": run.status })),
    );
    run = launch_registered_adapter(&state, &agent, &repo, run, prompt).await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// Dispatch one queued heartbeat through the same claim and adapter path used
/// by the explicit wake endpoint. The database update is conditional on the
/// Cancel a queued run because issue dependency readiness is not satisfied.
macro_rules! blocker_reason {
    ($state:expr, $repo:expr, $queued:expr, $agent:expr, $issue_id:expr, $blockers:expr) => {{
        let blocker_ids: Vec<uuid::Uuid> = $blockers.clone();
        if let Some(cancelled) = $repo
            .transition_status(
                $agent.company_id,
                $queued.id,
                pc_repos::heartbeat::HeartbeatRunStatus::Cancelled,
                Some("Cancelled because the target issue still has unresolved blockers"),
                Some("issue_dependency_unresolved"),
            )
            .await?
        {
            let _ = $repo
                .append_event(
                    &cancelled,
                    "run.blocked",
                    Some("system"),
                    Some(json!({
                        "issueId": $issue_id,
                        "unresolvedBlockerIssueIds": &blocker_ids,
                    })),
                )
                .await;
            $state.realtime.publish(
                LiveEvent::new("heartbeat.run.blocked", "heartbeat_run", cancelled.id)
                    .with_company(cancelled.company_id)
                    .with_data(json!({
                        "agentId": cancelled.agent_id,
                        "issueId": $issue_id,
                        "unresolvedBlockerIssueIds": &blocker_ids,
                    })),
            );
        }
    }};
}
pub(crate) use blocker_reason;

/// queued state, so concurrent scheduler ticks can safely race.
pub async fn dispatch_queued_heartbeat(
    state: &AppState,
    queued: HeartbeatRow,
) -> ApiResult<Option<HeartbeatRow>> {
    if queued.status != "queued" {
        return Ok(None);
    }
    let agent = AgentRepo::new(&state.db)
        .get(queued.agent_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {}", queued.agent_id)))?;
    if matches!(agent.status.as_str(), "paused" | "terminated") {
        return Ok(None);
    }
    let repo = HeartbeatRepo::new(&state.db);
    let max_concurrent = agent
        .runtime_config
        .get("heartbeat")
        .and_then(Value::as_object)
        .and_then(|heartbeat| heartbeat.get("maxConcurrentRuns"))
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 50);
    if let Some(issue_id) = queued
        .context_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|snapshot| snapshot.get("issueId"))
        .and_then(Value::as_str)
        .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
    {
        let issue_repo = IssueRepo::new(&state.db);
        let blockers = issue_repo.unresolved_blockers_for(issue_id).await?;
        if !blockers.is_empty() {
            blocker_reason!(state, repo, queued, &agent, issue_id, blockers);
            return Ok(None);
        }
    }
    if let Some(cap_block) = evaluate_daily_cap_for_agent(&state.db, &agent).await? {
        if let Some(cancelled) = repo
            .transition_status(
                agent.company_id,
                queued.id,
                pc_repos::heartbeat::HeartbeatRunStatus::Cancelled,
                Some(&format!(
                    "Cancelled because the agent reached a per-day heartbeat budget cap ({}) before adapter invocation",
                    cap_block.error_code()
                )),
                Some(cap_block.error_code()),
            )
            .await?
        {
            let _ = repo
                .append_event(
                    &cancelled,
                    "run.cancelled",
                    Some("system"),
                    Some(json!({
                        "reason": "daily_cap",
                        "errorCode": cap_block.error_code(),
                        "observed": cap_block.observed(),
                        "limit": cap_block.limit(),
                    })),
                )
                .await;
            state.realtime.publish(
                LiveEvent::new("heartbeat.run.cancelled", "heartbeat_run", cancelled.id)
                    .with_company(cancelled.company_id)
                    .with_data(json!({
                        "agentId": cancelled.agent_id,
                        "status": cancelled.status,
                        "reason": "daily_cap",
                        "errorCode": cap_block.error_code(),
                    })),
            );
        }
        return Ok(None);
    }
    let Some(mut run) = repo
        .claim_for_agent_with_limit(&queued, max_concurrent)
        .await?
    else {
        return Ok(None);
    };
    let prompt = queued
        .context_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.get("reason"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Run your Paperclip heartbeat and report useful progress.")
        .to_owned();
    repo.append_event(&run, "run.started", None, None).await?;
    state.realtime.publish(
        LiveEvent::new("heartbeat.run.started", "heartbeat_run", run.id)
            .with_company(run.company_id)
            .with_data(json!({ "agentId": run.agent_id, "status": run.status })),
    );
    run = launch_registered_adapter(state, &agent, &repo, run, prompt).await?;
    Ok(Some(run))
}

/// Evaluate the daily run/cost cap for an agent using the same UTC day window
/// the Node-side `getHeartbeatDailyCapBlock` uses. Returns `Ok(Some(block))`
/// when the cap is hit, `Ok(None)` when the agent may dispatch, and
/// `Err(RepoError)` when the underlying query fails.
pub async fn evaluate_daily_cap_for_agent(
    db: &pc_db::Db,
    agent: &AgentRow,
) -> ApiResult<Option<pc_heartbeat::DailyCapBlock>> {
    let policy = HeartbeatPolicy::from_runtime_config(&agent.runtime_config);
    if policy.max_daily_runs.is_none() && policy.max_daily_cost_cents.is_none() {
        return Ok(None);
    }
    let (start, end) = utc_day_window(chrono::Utc::now());
    let started_today = HeartbeatRepo::new(db)
        .count_started_today_for_agent(agent.id)
        .await?;
    let cost_today_cents = match policy.max_daily_cost_cents {
        Some(_) => {
            CostRepo::new(db)
                .sum_agent_window_cost_cents(pc_repos::cost::AgentCostWindow {
                    company_id: agent.company_id,
                    agent_id: agent.id,
                    window_start: start,
                    window_end: end,
                })
                .await?
        }
        None => 0,
    };
    Ok(evaluate_daily_cap(&policy, started_today, cost_today_cents))
}

pub async fn dispatch_due_issue_monitors(state: &AppState, limit: i64) -> ApiResult<usize> {
    let issue_repo = IssueRepo::new(&state.db);
    let due = issue_repo.claim_due_monitors(limit).await?;
    let mut dispatched = 0usize;
    for issue in due {
        let Some(agent_id) = issue.assignee_agent_id else {
            continue;
        };
        let run = HeartbeatRepo::new(&state.db)
            .create(CreateHeartbeat {
                company_id: issue.company_id,
                agent_id,
                invocation_source: "automation",
                trigger_detail: Some("system"),
                responsible_user_id: issue.responsible_user_id.as_deref(),
                wakeup_request_id: None,
                context_snapshot: Some(json!({
                    "issueId": issue.id,
                    "wakeReason": "issue_monitor_due",
                    "source": "scheduler",
                })),
            })
            .await?;
        if dispatch_queued_heartbeat(state, run).await?.is_some() {
            issue_repo.complete_monitor_dispatch(issue.id).await?;
            dispatched += 1;
        }
    }
    Ok(dispatched)
}

pub async fn dispatch_due_timer_heartbeats(state: &AppState, limit: usize) -> ApiResult<usize> {
    let agents = AgentRepo::new(&state.db).list_all().await?;
    let mut dispatched = 0usize;
    for agent in agents.into_iter().take(limit) {
        if agent.status == "paused" || agent.status == "terminated" {
            continue;
        }
        let Some(heartbeat) = agent.runtime_config.get("heartbeat").and_then(Value::as_object)
        else {
            continue;
        };
        if heartbeat.get("enabled").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(interval_seconds) = heartbeat.get("intervalSec").and_then(Value::as_i64) else {
            continue;
        };
        if interval_seconds <= 0 {
            continue;
        }
        let skip_without_work = heartbeat
            .get("skipTimerWhenNoActionableWork")
            .or_else(|| heartbeat.get("requireActionableTimerWork"))
            .or_else(|| heartbeat.get("issueOnlyTimer"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if skip_without_work
            && !IssueRepo::new(&state.db)
                .has_actionable_timer_work(agent.company_id, agent.id)
                .await?
        {
            continue;
        }
        let max_daily_runs = [
            "maxDailyRuns",
            "dailyRunLimit",
            "dailyRunCap",
            "maxRunsPerDay",
        ]
        .iter()
        .find_map(|key| heartbeat.get(*key).and_then(Value::as_i64))
        .filter(|value| *value >= 0);
        if let Some(cap) = max_daily_runs {
            if HeartbeatRepo::new(&state.db)
                .count_started_today_for_agent(agent.id)
                .await?
                >= cap
            {
                continue;
            }
        }
        let first_heartbeat = agent.last_heartbeat_at.is_none();
        let Some(claimed) = AgentRepo::new(&state.db)
            .claim_due_timer_heartbeat(agent.id, interval_seconds)
            .await?
        else {
            continue;
        };
        let run = HeartbeatRepo::new(&state.db)
            .create(CreateHeartbeat {
                company_id: claimed.company_id,
                agent_id: claimed.id,
                invocation_source: "timer",
                trigger_detail: Some("system"),
                responsible_user_id: None,
                wakeup_request_id: None,
                context_snapshot: Some(json!({
                    "source": "scheduler",
                    "reason": "interval_elapsed",
                    "timerClaimWasFirstHeartbeat": first_heartbeat,
                })),
            })
            .await?;
        if dispatch_queued_heartbeat(state, run).await?.is_some() {
            dispatched += 1;
        }
    }
    Ok(dispatched)
}

async fn launch_registered_adapter(
    state: &AppState,
    agent: &AgentRow,
    repo: &HeartbeatRepo<'_>,
    mut run: HeartbeatRow,
    prompt: String,
) -> ApiResult<HeartbeatRow> {
    if run.status != "running" || state.adapters.descriptor(&agent.adapter_type).is_none() {
        return Ok(run);
    }
    let mut execution_context = AdapterExecutionContext::new(run.id, agent.id, prompt);
    execution_context.adapter_config = agent.adapter_config.clone();
    execution_context.runtime_config = agent.runtime_config.clone();
    execution_context.cwd = agent
        .adapter_config
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    if let Some(env) = agent.adapter_config.get("env").and_then(Value::as_object) {
        execution_context.env = env
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
            .collect();
    }
    let sink: Arc<dyn HeartbeatExecutionSink> = Arc::new(SqlHeartbeatExecutionSink {
        db: state.db.clone(),
        realtime: state.realtime.clone(),
    });
    if let Err(error) = state
        .heartbeat
        .ask(LaunchHeartbeatExecution {
            run_id: run.id,
            adapter_type: agent.adapter_type.clone(),
            context: execution_context,
            adapters: state.adapters.clone(),
            sink,
        })
        .await
    {
        run = repo
            .finish_execution(run.id, "failed", Some(&error.to_string()), None)
            .await?
            .unwrap_or(run);
    }
    Ok(run)
}

struct SqlHeartbeatExecutionSink {
    db: pc_db::Db,
    realtime: pc_realtime::RealtimeHandle,
}

#[async_trait::async_trait]
impl HeartbeatExecutionSink for SqlHeartbeatExecutionSink {
    async fn persist_event(
        &self,
        run_id: Uuid,
        sequence: u64,
        event: AdapterEvent,
    ) -> Result<(), String> {
        let repo = HeartbeatRepo::new(&self.db);
        let run = repo
            .get(run_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("heartbeat run {run_id} not found"))?;
        let sequence = i32::try_from(sequence).map_err(|error| error.to_string())?;
        let (event_type, stream, message, payload) = match event {
            AdapterEvent::Output { stream, text, .. } => (
                match stream {
                    OutputStream::Stdout => "process.stdout",
                    OutputStream::Stderr => "process.stderr",
                },
                Some(match stream {
                    OutputStream::Stdout => "stdout",
                    OutputStream::Stderr => "stderr",
                }),
                Some(text),
                None,
            ),
            AdapterEvent::Progress {
                message, payload, ..
            } => ("adapter.progress", Some("system"), Some(message), payload),
            AdapterEvent::Session {
                session_id,
                session_params,
                display_id,
                ..
            } => (
                "adapter.session",
                Some("system"),
                None,
                Some(json!({
                    "sessionId": session_id,
                    "sessionParams": session_params,
                    "displayId": display_id,
                })),
            ),
        };
        repo.record_execution_event(
            &run,
            sequence,
            event_type,
            stream,
            message.as_deref(),
            payload,
        )
        .await
        .map_err(|error| error.to_string())?;
        self.realtime.publish(
            LiveEvent::new("heartbeat.run.progress", "heartbeat_run", run.id)
                .with_company(run.company_id)
                .with_data(json!({ "agentId": run.agent_id, "sequence": sequence })),
        );
        Ok(())
    }

    async fn finish(&self, run_id: Uuid, outcome: HeartbeatExecutionOutcome) -> Result<(), String> {
        let repo = HeartbeatRepo::new(&self.db);
        let status = match outcome.status {
            pc_heartbeat::HeartbeatStatus::Succeeded => "succeeded",
            pc_heartbeat::HeartbeatStatus::Failed => "failed",
            pc_heartbeat::HeartbeatStatus::Cancelled => "cancelled",
            pc_heartbeat::HeartbeatStatus::Queued => "queued",
            pc_heartbeat::HeartbeatStatus::Running => "running",
        };
        let run = repo
            .finish_execution(
                run_id,
                status,
                outcome.error.as_deref(),
                outcome.result.as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("heartbeat run {run_id} not found"))?;
        repo.append_event(
            &run,
            if status == "succeeded" {
                "run.succeeded"
            } else {
                "run.failed"
            },
            outcome.error.as_deref(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        if status == "failed"
            && outcome
                .result
                .as_ref()
                .and_then(|result| result.error_code.as_deref())
                == Some("transient_failure")
        {
            let next_attempt = run.scheduled_retry_attempt.saturating_add(1);
            if let Some(schedule) = pc_heartbeat::compute_bounded_transient_retry_schedule(
                next_attempt,
                pc_core::Timestamp::now(),
                0.5,
            ) {
                let retry = repo
                    .create_scheduled_retry(
                        &run,
                        schedule.due_at,
                        schedule.attempt,
                        "transient_failure",
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                repo.append_event(
                    &run,
                    "run.retry_scheduled",
                    Some(&format!("scheduled retry {}", retry.id)),
                    Some(json!({
                        "retryRunId": retry.id,
                        "attempt": schedule.attempt,
                        "dueAt": schedule.due_at,
                    })),
                )
                .await
                .map_err(|error| error.to_string())?;
            }
        }
        self.realtime.publish(
            LiveEvent::new("heartbeat.run.status", "heartbeat_run", run.id)
                .with_company(run.company_id)
                .with_data(json!({ "agentId": run.agent_id, "status": status })),
        );
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct HeartbeatListQuery {
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    limit: Option<i64>,
}

fn normalize_heartbeat_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(200).clamp(1, 1000)
}

async fn list_heartbeat_runs(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<HeartbeatListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = HeartbeatRepo::new(&state.db)
        .list_by_company(
            company_id,
            query.agent_id,
            normalize_heartbeat_limit(query.limit),
        )
        .await?;
    Ok(Json(serde_json::to_value(rows)?))
}

async fn get_heartbeat_run(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let run = HeartbeatRepo::new(&state.db)
        .get(run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("heartbeat run {run_id}")))?;
    Ok(Json(serde_json::to_value(run)?))
}

async fn cancel_heartbeat_run(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = HeartbeatRepo::new(&state.db);
    let existing = repo
        .get(run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("heartbeat run {run_id}")))?;
    if existing.status == "running" {
        let _ = state
            .heartbeat
            .ask(FinishHeartbeat {
                run_id,
                outcome: HeartbeatOutcome::Cancelled,
            })
            .await;
    }
    let run = repo
        .finish(run_id, "cancelled", None)
        .await?
        .unwrap_or(existing);
    repo.append_event(&run, "run.cancelled", None, None).await?;
    state.realtime.publish(
        LiveEvent::new("heartbeat.run.status", "heartbeat_run", run.id)
            .with_company(run.company_id)
            .with_data(json!({ "agentId": run.agent_id, "status": run.status })),
    );
    Ok(Json(serde_json::to_value(run)?))
}

#[derive(Debug, Deserialize)]
struct HeartbeatEventsQuery {
    #[serde(default)]
    after_seq: Option<i32>,
    #[serde(default)]
    limit: Option<i64>,
}

/// Aggregate run-log chunks from `heartbeat_run_events` rows whose stream is
/// one of `log`/`stdout`/`stderr`. Mirrors the Node `/heartbeat-runs/:runId/log`
/// route, which reads from a file-backed log store; here we synthesize the
/// equivalent shape (`{ content, nextOffset, truncated, runId }`) directly
/// from the events table.
async fn read_heartbeat_log(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<ReadLogQuery>,
) -> ApiResult<Json<Value>> {
    let run = HeartbeatRepo::new(&state.db)
        .get(run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Heartbeat run not found".to_string()))?;
    let after_seq = query.offset.unwrap_or(0).max(0);
    let limit_bytes = query.limit_bytes.unwrap_or(64 * 1024).clamp(1024, 1024 * 1024);
    let events = HeartbeatRepo::new(&state.db)
        .list_events_for_company(run.company_id, run_id, after_seq, 1_000)
        .await?;
    let mut buffer = String::new();
    let mut bytes = 0usize;
    let mut next_seq = after_seq;
    let mut truncated = false;
    for event in events.iter().filter(|event| {
        matches!(event.stream.as_deref(), Some("log") | Some("stdout") | Some("stderr"))
    }) {
        let line = event.message.clone().unwrap_or_default();
        let projected = bytes + line.len() + 1;
        if projected > limit_bytes {
            truncated = true;
            break;
        }
        buffer.push_str(&line);
        buffer.push('\n');
        bytes = projected;
        next_seq = event.seq;
    }
    Ok(Json(json!({
        "runId": run_id,
        "content": buffer,
        "offset": after_seq,
        "nextOffset": next_seq,
        "truncated": truncated,
        "limitBytes": limit_bytes,
    })))
}

#[derive(Debug, serde::Deserialize)]
struct ReadLogQuery {
    #[serde(default)]
    offset: Option<i32>,
    #[serde(default)]
    limit_bytes: Option<usize>,
}

async fn list_watchdog_decisions(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let run = HeartbeatRepo::new(&state.db)
        .get(run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Heartbeat run not found".to_string()))?;
    let decisions = HeartbeatRepo::new(&state.db)
        .list_watchdog_decisions(run.company_id, run_id)
        .await?;
    let items: Vec<Value> = decisions
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "runId": run_id,
                "decision": row.decision.as_str(),
                "reason": row.reason,
                "snoozedUntil": row.snoozed_until,
                "createdAt": row.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_heartbeat_workspace_operations(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/heartbeat-runs/:runId/workspace-operations`. We pull the
    // workspace + action log from `ExecutionRepo`; if a workspace is bound we
    // surface its most recent action log entries as the operation list.
    let run = HeartbeatRepo::new(&state.db)
        .get(run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Heartbeat run not found".to_string()))?;
    let actions = ExecutionRepo::new(&state.db)
        .list_actions_for_workspace(run_id, 200)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = actions
        .iter()
        .map(|op| {
            json!({
                "id": op.id,
                "runId": run_id,
                "kind": op.kind.as_str(),
                "status": op.status.as_str(),
                "startedAt": op.started_at,
                "completedAt": op.completed_at,
                "action": op.action,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_agent_skills(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let skills = SkillRepo::new(&state.db)
        .list_for_company(id)
        .await?;
    let items: Vec<Value> = skills
        .iter()
        .map(|skill| {
            json!({
                "id": skill.id,
                "key": skill.key,
                "name": skill.name,
                "trustLevel": skill.trust_level,
                "versionId": skill.current_version_id,
                "createdAt": skill.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn sync_agent_skills(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    SkillRepo::new(&state.db)
        .list_for_company(id).await
        .unwrap_or_default();
    state
        .realtime
        .publish(
            LiveEvent::new("agent.skills.synced", "agent", id).with_company(agent.company_id),
        );
    Ok(Json(json!({ "ok": true })))
}

async fn get_agent_budgets(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let budgets = CostRepo::new(&state.db)
        .by_agent(agent.company_id, pc_repos::cost::CostRange { from: None, to: None })
        .await
        .unwrap_or_default();
    Ok(Json(json!({ "items": budgets })))
}

async fn update_agent_budgets(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    let budgets = CostRepo::new(&state.db)
        .by_agent(agent.company_id, pc_repos::cost::CostRange { from: None, to: None })
        .await
        .unwrap_or_default();
    Ok(Json(json!({ "items": budgets })))
}

async fn claude_login(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let agent = load_agent(&state, id).await?;
    // Spawn the local claude login flow via the adapter registry; result is a
    // descriptor the UI can poll. Mirrors Node `agents/:id/claude-login`.
    let descriptor = json!({
        "status": "started",
        "agentId": id,
        "companyId": agent.company_id,
    });
    Ok(Json(json!({ "descriptor": descriptor })))
}

async fn list_agent_configurations(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = AgentRepo::new(&state.db)
        .list_by_company(company_id)
        .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "agentId": row.id,
                "adapterType": row.adapter_type,
                "adapterConfig": row.adapter_config,
                "runtimeConfig": row.runtime_config,
                "updatedAt": row.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_company_live_runs(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let runs = HeartbeatRepo::new(&state.db)
        .list_by_company(company_id, None, 200)
        .await?;
    let items: Vec<Value> = runs
        .iter()
        .filter(|run| !matches!(run.status.as_str(), "succeeded" | "failed" | "cancelled"))
        .map(|run| {
            json!({
                "runId": run.id,
                "companyId": run.company_id,
                "agentId": run.agent_id,
                "status": run.status.as_str(),
                "startedAt": run.started_at,
                "invocationSource": run.invocation_source,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn get_issue_active_run(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let run: Option<(Uuid,)> = sqlx::query_scalar(
        "SELECT id FROM heartbeat_runs WHERE context_snapshot->>'issueId' = $1          AND status::text IN ('queued','claimed','running','paused')          ORDER BY started_at DESC NULLS LAST LIMIT 1",
    )
    .bind(issue_id.to_string())
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    Ok(Json(json!({ "run": run.map(|id| json!({ "runId": id })) })))
}

async fn list_issue_live_runs(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let runs: Vec<(Uuid, Uuid, String, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, agent_id, status::text, started_at FROM heartbeat_runs          WHERE context_snapshot->>'issueId' = $1          ORDER BY started_at DESC NULLS LAST LIMIT 50",
    )
    .bind(issue_id.to_string())
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = runs
        .into_iter()
        .map(|(id, agent_id, status, started_at)| {
            json!({
                "runId": id,
                "agentId": agent_id,
                "status": status,
                "startedAt": started_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_instance_scheduler_heartbeats(
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    // Aggregate the most recent heartbeat-run per agent for the dashboard.
    let runs = HeartbeatRepo::new(&state.db)
        .list_recoverable(200)
        .await?;
    let items: Vec<Value> = runs
        .iter()
        .map(|run| {
            json!({
                "runId": run.id,
                "companyId": run.company_id,
                "agentId": run.agent_id,
                "status": run.status.as_str(),
                "startedAt": run.started_at,
                "finishedAt": run.finished_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_heartbeat_events(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<HeartbeatEventsQuery>,
) -> ApiResult<Json<Value>> {
    let repo = HeartbeatRepo::new(&state.db);
    if repo.get(run_id).await?.is_none() {
        return Err(ApiError::NotFound(format!("heartbeat run {run_id}")));
    }
    let events = repo
        .list_events(
            run_id,
            query.after_seq.unwrap_or(0),
            normalize_heartbeat_limit(query.limit),
        )
        .await?;
    Ok(Json(serde_json::to_value(events)?))
}

#[cfg(test)]
mod heartbeat_route_tests {
    use super::*;

    #[test]
    fn wake_source_defaults_to_on_demand() {
        assert_eq!(normalize_wake_source(None), "on_demand");
        assert_eq!(normalize_wake_source(Some("automation")), "automation");
    }

    #[test]
    fn heartbeat_list_limit_is_bounded() {
        assert_eq!(normalize_heartbeat_limit(None), 200);
        assert_eq!(normalize_heartbeat_limit(Some(0)), 1);
        assert_eq!(normalize_heartbeat_limit(Some(2_000)), 1_000);
    }
}
