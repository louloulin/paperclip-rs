//! `/api/activity/*` 路由：暴露 pc-activity 日志。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_activity::{ActivityActor, ActivityEvent, ActivityFilter, ActivityKind};

use crate::{require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/activity/emit", post(emit_event))
        .route("/api/activity/list", get(query_events))
        // ── Round 43: heartbeat-runs/issues 关联 ──
        .route("/api/heartbeat-runs/:run_id/issues", get(heartbeat_run_issues))
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

/// `GET /api/heartbeat-runs/:run_id/issues`
///
/// Mirrors Node `/heartbeat-runs/:runId/issues`. Cross-tenant existence is
/// hidden behind an indistinguishable empty array (200) so we don't leak
/// run-id presence to unauthorized callers.
async fn heartbeat_run_issues(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let _user_id = require_user_id(&state, &headers).await?;

    // Resolve the run + company in one query.
    let row: Option<(Uuid, Option<Value>)> = sqlx::query_as(
        "SELECT company_id, context_snapshot FROM heartbeat_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (company_id, context_snapshot) = match row {
        Some(v) => v,
        None => return Ok(Json(json!([]))),
    };

    // Cross-tenant check: must be a member of the run's company.
    let membership: Option<(Uuid,)> = sqlx::query_as(
        "SELECT company_id FROM company_memberships WHERE company_id = $1 AND principal_id = $2 AND status = 'active'",
    )
    .bind(company_id)
    .bind(&_user_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    if membership.is_none() {
        // Indistinguishable 200 [] for cross-tenant.
        return Ok(Json(json!([])));
    }

    // Resolve context-snapshot issue if present and not in payload set.
    let context_issue_id: Option<String> = context_snapshot
        .as_ref()
        .and_then(|v| v.get("issueId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    // Fetch issues linked to this run via execution_run_id / checkout_run_id.
    let rows: Vec<(Uuid, Option<String>, String, String, String, String)> = sqlx::query_as(
        "SELECT i.id, i.identifier, i.title, i.status::text, i.priority::text, COALESCE(i.kind::text,'issue')          FROM issues i          WHERE i.company_id = $1            AND (i.execution_run_id = $2 OR i.checkout_run_id = $2)          ORDER BY i.updated_at DESC          LIMIT 200",
    )
    .bind(company_id)
    .bind(run_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    use std::collections::BTreeSet;
    let mut seen: BTreeSet<String> = rows.iter().map(|(id, _, _, _, _, _)| id.to_string()).collect();
    let mut items: Vec<Value> = rows
        .into_iter()
        .map(|(id, identifier, title, status, priority, kind)| {
            json!({
                "id": id,
                "identifier": identifier,
                "title": title,
                "status": status,
                "priority": priority,
                "kind": kind,
            })
        })
        .collect();

    // Optionally include context-snapshot issue if missing from set.
    if let Some(ctx_id) = context_issue_id {
        if !seen.contains(&ctx_id) {
            if let Ok(uuid) = Uuid::parse_str(&ctx_id) {
                if let Some((id, identifier, title, status, priority, kind)) = sqlx::query_as::<_, (Uuid, Option<String>, String, String, String, String)>(
                    "SELECT i.id, i.identifier, i.title, i.status::text, i.priority::text, COALESCE(i.kind::text,'issue')                      FROM issues i WHERE i.company_id = $1 AND i.id = $2",
                )
                .bind(company_id)
                .bind(uuid)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                {
                    items.push(json!({
                        "id": id,
                        "identifier": identifier,
                        "title": title,
                        "status": status,
                        "priority": priority,
                        "kind": kind,
                    }));
                }
            }
        }
    }

    Ok(Json(Value::Array(items)))
}
