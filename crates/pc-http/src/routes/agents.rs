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
    ClearAgentErrorCommand, CreateAgent, CreateAgentCommand, CreateAgentKey,
    CreateAgentKeyCommand, HireAgentCommand, InstructionAgent, InstructionsBundleUpdate,
    PauseAgentCommand, PauseReason, ResetRuntimeSession, ResetRuntimeSessionCommand,
    ResumeAgentCommand, RevisionContext, RevokeAgentKeyCommand, RollbackConfigRevisionCommand,
    TerminateAgentCommand, UpdateAgentCommand, UpdateAgentPermissionsCommand,
};
use pc_core::actor_runtime::kameo_api::SendError;
use pc_heartbeat::{
    FinishHeartbeat, HeartbeatExecutionOutcome, HeartbeatExecutionSink, HeartbeatOutcome,
    LaunchHeartbeatExecution, StartHeartbeat, StartHeartbeatResult,
};
use pc_realtime::LiveEvent;
use pc_repos::agent::{AgentRepo, AgentRow};
use pc_repos::heartbeat::{CreateHeartbeat, HeartbeatRepo, HeartbeatRow};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agents", get(list).post(create))
        .route(
            "/api/companies/:company_id/agents",
            get(list_company_agents).post(create_company_agent),
        )
        .route(
            "/api/companies/:company_id/agent-hires",
            post(hire_agent),
        )
        .route("/api/agents/:id", get(get_one).patch(update).delete(remove))
        .route(
            "/api/agents/:id/configuration",
            get(get_configuration),
        )
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
        .route(
            "/api/agents/:id/runtime-state",
            get(get_runtime_state),
        )
        .route(
            "/api/agents/:id/task-sessions",
            get(list_task_sessions),
        )
        .route(
            "/api/agents/:id/runtime-state/reset-session",
            post(reset_runtime_session),
        )
        .route(
            "/api/agents/:id/keys",
            get(list_agent_keys).post(create_agent_key),
        )
        .route(
            "/api/agents/:id/keys/:key_id",
            delete(revoke_agent_key),
        )
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
}

async fn list_company_agents(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Vec<AgentRow>>> {
    let rows = AgentRepo::new(&state.db).list_by_company(company_id).await?;
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
    Ok((
        StatusCode::CREATED,
        Json(row),
    ))
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
    state.realtime.publish(
        LiveEvent::new("agent.error_cleared", "agent", id).with_company(row.company_id),
    );
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
    state.realtime.publish(
        LiveEvent::new("agent.terminated", "agent", id).with_company(row.company_id),
    );
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
        LiveEvent::new("agent.permissions_updated", "agent", id)
            .with_company(row.company_id),
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
        LiveEvent::new("agent.runtime_session_reset", "agent", id)
            .with_company(row.company_id),
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
