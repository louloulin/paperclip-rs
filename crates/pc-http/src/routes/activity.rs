//! `/api/activity/*` 路由：暴露 pc-activity 日志。

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_activity::{ActivityActor, ActivityEvent, ActivityFilter, ActivityKind};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/activity/emit", post(emit_event))
        .route("/api/activity/list", get(query_events))
}

#[derive(Debug, Deserialize)]
struct EmitBody {
    kind: String,
    actor_type: Option<String>,
    actor_id: Option<String>,
    actor_label: Option<String>,
    subject_kind: String,
    subject_id: Uuid,
    company_id: Option<Uuid>,
    payload: Option<Value>,
}

fn parse_kind(s: &str) -> ApiResult<ActivityKind> {
    match s {
        "issue.created" => Ok(ActivityKind::IssueCreated),
        "issue.updated" => Ok(ActivityKind::IssueUpdated),
        "issue.assigned" => Ok(ActivityKind::IssueAssigned),
        "issue.closed" => Ok(ActivityKind::IssueClosed),
        "decision.proposed" => Ok(ActivityKind::DecisionProposed),
        "decision.approved" => Ok(ActivityKind::DecisionApproved),
        "decision.rejected" => Ok(ActivityKind::DecisionRejected),
        "approval.requested" => Ok(ActivityKind::ApprovalRequested),
        "approval.granted" => Ok(ActivityKind::ApprovalGranted),
        "approval.denied" => Ok(ActivityKind::ApprovalDenied),
        "agent.started" => Ok(ActivityKind::AgentStarted),
        "agent.stopped" => Ok(ActivityKind::AgentStopped),
        "agent.heartbeat" => Ok(ActivityKind::AgentHeartbeat),
        "agent.error" => Ok(ActivityKind::AgentError),
        "plugin.installed" => Ok(ActivityKind::PluginInstalled),
        "plugin.enabled" => Ok(ActivityKind::PluginEnabled),
        "plugin.disabled" => Ok(ActivityKind::PluginDisabled),
        "plugin.error" => Ok(ActivityKind::PluginError),
        "cost.recorded" => Ok(ActivityKind::CostRecorded),
        "secret.accessed" => Ok(ActivityKind::SecretAccessed),
        "document.annotated" => Ok(ActivityKind::DocumentAnnotated),
        "routine.ran" => Ok(ActivityKind::RoutineRan),
        "pipeline.ran" => Ok(ActivityKind::PipelineRan),
        other => Err(ApiError::BadRequest(format!(
            "unknown activity kind: {other}"
        ))),
    }
}

fn actor_from(body: &EmitBody) -> ApiResult<ActivityActor> {
    match body.actor_type.as_deref().unwrap_or("system") {
        "user" => Ok(ActivityActor::User {
            id: body
                .actor_id
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| ApiError::BadRequest("user actor_id required".into()))?,
            name: body
                .actor_label
                .clone()
                .unwrap_or_else(|| "user".to_string()),
        }),
        "agent" => Ok(ActivityActor::Agent {
            id: body
                .actor_id
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| ApiError::BadRequest("agent actor_id required".into()))?,
            name: body
                .actor_label
                .clone()
                .unwrap_or_else(|| "agent".to_string()),
        }),
        "plugin" => Ok(ActivityActor::Plugin {
            plugin_id: body
                .actor_id
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| ApiError::BadRequest("plugin actor_id required".into()))?,
            plugin_key: body
                .actor_label
                .clone()
                .unwrap_or_else(|| "plugin".to_string()),
        }),
        "system" => Ok(ActivityActor::System {
            component: body
                .actor_label
                .clone()
                .unwrap_or_else(|| "system".to_string()),
        }),
        _ => Ok(ActivityActor::Anonymous),
    }
}

async fn emit_event(
    State(state): State<AppState>,
    Json(body): Json<EmitBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let kind = parse_kind(&body.kind)?;
    let actor = actor_from(&body)?;
    let mut event = ActivityEvent::new(kind, actor, body.subject_kind.clone(), body.subject_id);
    if let Some(c) = body.company_id {
        event = event.with_company(c);
    }
    if let Some(p) = body.payload {
        event = event.with_payload(p);
    }
    let id = state
        .activity
        .emit(event)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id.to_string() }))))
}

#[derive(Debug, Deserialize, Default)]
struct ListQuery {
    company_id: Option<Uuid>,
    kind: Option<String>,
    since: Option<DateTime<Utc>>,
    limit: Option<usize>,
}

async fn query_events(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let kind = q.kind.as_deref().map(parse_kind);
    let kind = kind.transpose()?.map(Box::new);
    let filter = ActivityFilter {
        company_id: q.company_id,
        kind: kind.map(|b| *b),
        actor_id: None,
        subject_kind: None,
        since: q.since,
        limit: q.limit,
    };
    let events = state
        .activity
        .query(filter)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = events
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id.to_string(),
                "kind": e.kind.as_str(),
                "actor": match e.actor {
                    ActivityActor::User { id, name } => json!({"type":"user","id":id,"name":name}),
                    ActivityActor::Agent { id, name } => json!({"type":"agent","id":id,"name":name}),
                    ActivityActor::System { component } => json!({"type":"system","component":component}),
                    ActivityActor::Plugin { plugin_id, plugin_key } => json!({"type":"plugin","id":plugin_id,"key":plugin_key}),
                    ActivityActor::Anonymous => json!({"type":"anonymous"}),
                },
                "subjectKind": e.subject_kind,
                "subjectId": e.subject_id,
                "companyId": e.company_id,
                "payload": e.payload,
                "occurredAt": e.occurred_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}
