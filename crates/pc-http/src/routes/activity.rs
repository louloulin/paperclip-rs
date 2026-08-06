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
use pc_repos::activity::{ActivityRepo, NewActivity};
use pc_repos::company_member::CompanyMemberRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue::IssueRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/activity/emit", post(emit_event))
        .route("/api/activity/list", get(query_events))
        // ── Round 43: heartbeat-runs/issues 关联 ──
        .route(
            "/api/heartbeat-runs/:run_id/issues",
            get(heartbeat_run_issues),
        )
        // ── Round 209: batch emit + run-scoped list ──
        .route("/api/activity/emit/batch", post(emit_events_batch))
        .route("/api/activity/runs/:run_id", get(list_run_activity))
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
    let row = HeartbeatRepo::new(&state.db)
        .get_company_and_context(run_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (company_id, context_snapshot) = match row {
        Some(v) => v,
        None => return Ok(Json(json!([]))),
    };

    // Cross-tenant check: must be a member of the run's company.
    if !CompanyMemberRepo::new(&state.db)
        .has_active_membership(company_id, &_user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
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
    let rows = IssueRepo::new(&state.db)
        .list_for_run(company_id, run_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    use std::collections::BTreeSet;
    let mut seen: BTreeSet<String> = rows.iter().map(|r| r.id.to_string()).collect();
    let mut items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "identifier": r.identifier,
                "title": r.title,
                "status": r.status,
                "priority": r.priority,
                "kind": r.kind,
            })
        })
        .collect();

    // Optionally include context-snapshot issue if missing from set.
    if let Some(ctx_id) = context_issue_id {
        if !seen.contains(&ctx_id) {
            if let Ok(uuid) = Uuid::parse_str(&ctx_id) {
                if let Ok(Some(r)) = IssueRepo::new(&state.db)
                    .get_run_link_summary(company_id, uuid)
                    .await
                {
                    items.push(json!({
                        "id": r.id,
                        "identifier": r.identifier,
                        "title": r.title,
                        "status": r.status,
                        "priority": r.priority,
                        "kind": r.kind,
                    }));
                }
            }
        }
    }

    Ok(Json(Value::Array(items)))
}

// ============================================================================
// Round 209: batch emit + run-scoped list
// ============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BatchEmitItem {
    kind: String,
    #[serde(default)]
    actor_type: Option<String>,
    #[serde(default)]
    actor_id: Option<String>,
    #[serde(default)]
    actor_label: Option<String>,
    subject_kind: String,
    subject_id: Uuid,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    run_id: Option<Uuid>,
    #[serde(default)]
    responsible_user_id: Option<String>,
}

fn parse_actor_type(s: Option<&str>) -> pc_repos::activity::ActorType {
    use pc_repos::activity::ActorType;
    match s.unwrap_or("system") {
        "user" => ActorType::User,
        "agent" => ActorType::Agent,
        "board" => ActorType::Board,
        "api_key" => ActorType::ApiKey,
        "plugin" => ActorType::Plugin,
        _ => ActorType::System,
    }
}

fn batch_item_to_new_activity(item: BatchEmitItem) -> ApiResult<NewActivity> {
    let _kind = parse_kind(&item.kind)?;
    Ok(NewActivity {
        company_id: item.company_id.unwrap_or_else(Uuid::nil),
        actor_type: parse_actor_type(item.actor_type.as_deref()),
        actor_id: item.actor_id.unwrap_or_default(),
        action: item.kind,
        entity_type: item.subject_kind,
        entity_id: item.subject_id.to_string(),
        agent_id: item.agent_id,
        run_id: item.run_id,
        responsible_user_id: item.responsible_user_id,
        details: item.payload,
    })
}

/// `POST /api/activity/emit/batch` — 批量写入 activity events。
/// 使用 `record_batch` 一次性 INSERT，减少 round-trip。
async fn emit_events_batch(
    State(state): State<AppState>,
    Json(items): Json<Vec<BatchEmitItem>>,
) -> ApiResult<Json<Value>> {
    if items.len() > 500 {
        return Err(ApiError::BadRequest("batch size must be <= 500".into()));
    }
    let new_items: Vec<NewActivity> = items
        .into_iter()
        .map(batch_item_to_new_activity)
        .collect::<ApiResult<Vec<_>>>()?;
    let n = ActivityRepo::new(&state.db)
        .record_batch(&new_items)
        .await?;
    Ok(Json(json!({
        "inserted": n,
        "requested": new_items.len(),
    })))
}

fn activity_row_json(row: &pc_repos::activity::ActivityRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "actorType": row.actor_type,
        "actorId": row.actor_id,
        "action": row.action,
        "entityType": row.entity_type,
        "entityId": row.entity_id,
        "agentId": row.agent_id,
        "runId": row.run_id,
        "responsibleUserId": row.responsible_user_id,
        "details": row.details,
        "createdAt": row.created_at,
    })
}

/// `GET /api/activity/runs/:run_id` — 列出指定 run_id 的所有 activity。
async fn list_run_activity(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ActivityRepo::new(&state.db).list_for_run(run_id).await?;
    let items: Vec<Value> = rows.iter().map(activity_row_json).collect();
    Ok(Json(json!({
        "runId": run_id,
        "total": items.len(),
        "items": items,
    })))
}

#[cfg(test)]
mod round209_tests {
    use super::*;

    #[test]
    fn parse_actor_type_known_values() {
        use pc_repos::activity::ActorType;
        assert_eq!(parse_actor_type(Some("user")), ActorType::User);
        assert_eq!(parse_actor_type(Some("agent")), ActorType::Agent);
        assert_eq!(parse_actor_type(Some("board")), ActorType::Board);
        assert_eq!(parse_actor_type(Some("api_key")), ActorType::ApiKey);
        assert_eq!(parse_actor_type(Some("plugin")), ActorType::Plugin);
    }

    #[test]
    fn parse_actor_type_defaults_to_system() {
        use pc_repos::activity::ActorType;
        assert_eq!(parse_actor_type(None), ActorType::System);
        assert_eq!(parse_actor_type(Some("unknown")), ActorType::System);
        assert_eq!(parse_actor_type(Some("")), ActorType::System);
    }

    #[test]
    fn activity_row_json_uses_camel_case_keys() {
        use pc_repos::activity::{ActivityRow, ActorType};
        let row = ActivityRow {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            actor_type: "agent".to_owned(),
            actor_id: "agent-1".to_owned(),
            action: "issue.assigned".to_owned(),
            entity_type: "issue".to_owned(),
            entity_id: "i-1".to_owned(),
            agent_id: Some(Uuid::nil()),
            run_id: None,
            responsible_user_id: None,
            details: None,
            created_at: pc_core::Timestamp::from_dt(Utc::now()),
        };
        let v = activity_row_json(&row);
        assert_eq!(v["actorType"], "agent");
        assert_eq!(v["entityId"], "i-1");
        assert_eq!(v["action"], "issue.assigned");
        // ActorType enum is constructed for compilation, not used in test
        let _ = ActorType::Agent;
    }
}
