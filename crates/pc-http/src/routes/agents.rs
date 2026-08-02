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
        .route("/api/agents/:id", get(get_one).patch(update).delete(remove))
        .route("/api/agents/:id/wakeup", post(wakeup))
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
struct CreateBody {
    company_id: Uuid,
    name: String,
    #[serde(default = "default_role")]
    role: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default = "default_adapter")]
    adapter_type: String,
    #[serde(default)]
    adapter_config: serde_json::Value,
}
fn default_role() -> String {
    "general".into()
}
fn default_adapter() -> String {
    "process".into()
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let row = AgentRepo::new(&state.db)
        .create(
            body.company_id,
            &body.name,
            &body.role,
            body.title.as_deref(),
            &body.adapter_type,
            body.adapter_config,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("agent.created", "agent", row.id)
            .with_company(row.company_id)
            .with_actor("system"),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.id, "company_id": row.company_id, "name": row.name,
            "role": row.role, "status": row.status
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = AgentRepo::new(&state.db)
        .update(
            id,
            body.name.as_deref(),
            body.role.as_deref(),
            body.title.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("agent.updated", "agent", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
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
