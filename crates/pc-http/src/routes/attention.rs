//! 统一注意力队列。

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::state::require_user_id;
use crate::{ApiError, ApiResult, AppState};
use pc_repos::agent::{AgentRepo, AgentRow};
use pc_repos::approval::ApprovalRepo;
use pc_repos::budget::{BudgetRepo, IncidentRow};
use pc_repos::company::CompanyRepo;
use pc_repos::decision::{DecisionRepo, DecisionRow};
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::inbox::InboxRepo;
use pc_repos::issue::{
    IssueRecoveryActionRow, IssueRepo, IssueThreadInteractionRow, ProductivityReviewAttentionRow,
    ReviewAttentionRow,
};
use pc_repos::join_request::{JoinRequestRepo, JoinRequestRow};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/companies/:company_id/attention", get(list))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AttentionQuery {
    include_dismissed: Option<bool>,
}

#[derive(Debug, FromRow)]
#[allow(dead_code)]
struct ApprovalRow {
    id: Uuid,
    approval_type: String,
    payload: Value,
    updated_at: pc_core::Timestamp,
}

#[derive(Debug, FromRow)]
#[allow(dead_code)]
struct BlockedIssueRow {
    id: Uuid,
    identifier: Option<String>,
    title: String,
    priority: String,
    updated_at: pc_core::Timestamp,
    sample_blocker_identifier: Option<String>,
}

#[derive(Debug, FromRow)]
#[allow(dead_code)]
struct FailedRunRow {
    id: Uuid,
    agent_name: String,
    error: Option<String>,
    updated_at: pc_core::Timestamp,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct ItemInput<'a> {
    source_kind: &'a str,
    id: Uuid,
    company_id: Uuid,
    subject: &'a Value,
    title: &'a str,
    severity: &'a str,
    activity_at: pc_core::Timestamp,
    detail: &'a Value,
}

fn subject(
    kind: &str,
    id: Uuid,
    company_id: Uuid,
    title: Option<&str>,
    identifier: Option<&str>,
    status: Option<&str>,
) -> Value {
    json!({ "kind": kind, "id": id, "companyId": company_id, "title": title, "identifier": identifier, "status": status, "href": null })
}

fn item(input: ItemInput<'_>) -> Value {
    let ItemInput {
        source_kind,
        id,
        company_id,
        subject,
        title,
        severity,
        activity_at,
        detail,
    } = input;
    let timestamp = activity_at.to_string();
    json!({
        "id": format!("{source_kind}:{id}"),
        "companyId": company_id,
        "sourceKind": source_kind,
        "subject": subject,
        "whyNow": title,
        "decisionVerbs": [],
        "inlineResolvable": false,
        "entryRule": "record is open",
        "exitRule": "record is resolved",
        "dedupKey": id,
        "dismissalKey": format!("{source_kind}:{id}"),
        "dismissal": null,
        "severity": severity,
        "rank": 0,
        "activityAt": timestamp,
        "createdAt": activity_at,
        "updatedAt": activity_at,
        "relatedIssue": null,
        "project": null,
        "workspace": null,
        "detail": detail,
        "trainingExampleId": null
    })
}

async fn fetch_approvals(state: &AppState, company_id: Uuid) -> ApiResult<Vec<ApprovalRow>> {
    Ok(ApprovalRepo::new(&state.db)
        .list_pending_attention(company_id)
        .await?
        .into_iter()
        .map(|row| ApprovalRow {
            id: row.id,
            approval_type: row.approval_type,
            payload: row.payload,
            updated_at: row.updated_at,
        })
        .collect())
}

async fn fetch_blocked(state: &AppState, company_id: Uuid) -> ApiResult<Vec<BlockedIssueRow>> {
    Ok(IssueRepo::new(&state.db)
        .list_blocked_attention(company_id)
        .await?
        .into_iter()
        .map(|row| BlockedIssueRow {
            id: row.id,
            identifier: row.identifier,
            title: row.title,
            priority: row.priority,
            updated_at: row.updated_at,
            sample_blocker_identifier: row.sample_blocker_identifier,
        })
        .collect())
}

async fn fetch_failed_runs(state: &AppState, company_id: Uuid) -> ApiResult<Vec<FailedRunRow>> {
    Ok(HeartbeatRepo::new(&state.db)
        .list_failed_attention(company_id)
        .await?
        .into_iter()
        .map(|row| FailedRunRow {
            id: row.id,
            agent_name: row.agent_name,
            error: row.error,
            updated_at: row.updated_at,
        })
        .collect())
}

async fn fetch_open_decisions(state: &AppState, company_id: Uuid) -> ApiResult<Vec<DecisionRow>> {
    Ok(DecisionRepo::new(&state.db)
        .list_open_attention(company_id, 100)
        .await?)
}

async fn fetch_pending_interactions(
    state: &AppState,
    company_id: Uuid,
) -> ApiResult<Vec<IssueThreadInteractionRow>> {
    Ok(IssueRepo::new(&state.db)
        .list_pending_interactions_attention(company_id)
        .await?)
}

async fn fetch_pending_joins(state: &AppState, company_id: Uuid) -> ApiResult<Vec<JoinRequestRow>> {
    Ok(JoinRequestRepo::new(&state.db)
        .list_pending_attention(company_id)
        .await?)
}

async fn fetch_recovery_actions(
    state: &AppState,
    company_id: Uuid,
) -> ApiResult<Vec<IssueRecoveryActionRow>> {
    Ok(IssueRepo::new(&state.db)
        .list_open_human_recovery_actions(company_id)
        .await?)
}

async fn fetch_productivity_reviews(
    state: &AppState,
    company_id: Uuid,
) -> ApiResult<Vec<ProductivityReviewAttentionRow>> {
    Ok(IssueRepo::new(&state.db)
        .list_productivity_review_attention(company_id)
        .await?)
}

async fn fetch_reviews(state: &AppState, company_id: Uuid) -> ApiResult<Vec<ReviewAttentionRow>> {
    Ok(IssueRepo::new(&state.db)
        .list_review_attention(company_id)
        .await?)
}

async fn fetch_budget_incidents(state: &AppState, company_id: Uuid) -> ApiResult<Vec<IncidentRow>> {
    Ok(BudgetRepo::new(&state.db)
        .list_open_attention(company_id)
        .await?)
}

async fn fetch_error_agents(state: &AppState, company_id: Uuid) -> ApiResult<Vec<AgentRow>> {
    Ok(AgentRepo::new(&state.db)
        .list_error_attention(company_id)
        .await?)
}

fn build_approval_items(approvals: Vec<ApprovalRow>, company_id: Uuid) -> Vec<Value> {
    approvals
        .into_iter()
        .map(|approval| {
            let subject = subject(
                "approval",
                approval.id,
                company_id,
                Some(&approval.approval_type),
                None,
                Some("pending"),
            );
            let detail = json!({
                "kind": "approval",
                "approvalType": approval.approval_type,
                "summaryExcerpt": approval.payload.get("summary"),
                "images": []
            });
            item(ItemInput {
                source_kind: "approval",
                id: approval.id,
                company_id,
                subject: &subject,
                title: "Approval requires a decision",
                severity: "high",
                activity_at: approval.updated_at,
                detail: &detail,
            })
        })
        .collect()
}

fn build_blocker_items(issues: Vec<BlockedIssueRow>, company_id: Uuid) -> Vec<Value> {
    issues
        .into_iter()
        .map(|issue| {
            let subject = subject(
                "issue",
                issue.id,
                company_id,
                Some(&issue.title),
                issue.identifier.as_deref(),
                Some("blocked"),
            );
            let detail = json!({
                "kind": "blocker",
                "blockingIssue": null,
                "images": []
            });
            let title = format!("Blocked issue needs attention ({})", issue.priority);
            let mut result = item(ItemInput {
                source_kind: "blocker_attention",
                id: issue.id,
                company_id,
                subject: &subject,
                title: &title,
                severity: "high",
                activity_at: issue.updated_at,
                detail: &detail,
            });
            result["dedupKey"] = json!(format!(
                "blocker:{}:{}",
                issue.id,
                issue
                    .sample_blocker_identifier
                    .as_deref()
                    .or(issue.identifier.as_deref())
                    .unwrap_or(&issue.id.to_string())
            ));
            result
        })
        .collect()
}

fn build_failed_run_items(runs: Vec<FailedRunRow>, company_id: Uuid) -> Vec<Value> {
    runs.into_iter()
        .map(|run| {
            let subject = subject(
                "run",
                run.id,
                company_id,
                Some(&run.agent_name),
                None,
                Some("failed"),
            );
            let detail = json!({
                "kind": "failed_run",
                "agentName": run.agent_name,
                "failureReasonExcerpt": run.error,
                "images": []
            });
            item(ItemInput {
                source_kind: "failed_run",
                id: run.id,
                company_id,
                subject: &subject,
                title: "Agent run failed",
                severity: "high",
                activity_at: run.updated_at,
                detail: &detail,
            })
        })
        .collect()
}

fn build_decision_items(decisions: Vec<DecisionRow>, company_id: Uuid) -> Vec<Value> {
    decisions
        .into_iter()
        .map(|decision| {
            let subject = subject(
                "decision",
                decision.id,
                company_id,
                Some(&decision.title),
                None,
                Some(&decision.status),
            );
            let detail = json!({
                "kind": "generic",
                "summaryExcerpt": decision.body.chars().take(240).collect::<String>(),
                "images": []
            });
            item(ItemInput {
                source_kind: "decision",
                id: decision.id,
                company_id,
                subject: &subject,
                title: "An agent decision is waiting for a board response.",
                severity: "medium",
                activity_at: decision.updated_at,
                detail: &detail,
            })
        })
        .collect()
}

fn build_interaction_items(
    interactions: Vec<IssueThreadInteractionRow>,
    company_id: Uuid,
) -> Vec<Value> {
    interactions
        .into_iter()
        .map(|interaction| {
            let label = interaction
                .title
                .clone()
                .or(interaction.summary.clone())
                .unwrap_or_else(|| {
                    pc_core::attention::interaction_label(&interaction.kind).to_owned()
                });
            let subject = subject(
                "interaction",
                interaction.id,
                company_id,
                Some(&label),
                None,
                Some(&interaction.status),
            );
            let detail =
                pc_core::attention::interaction_detail(&interaction.kind, &interaction.payload);
            let mut attention_item = item(ItemInput {
                source_kind: "issue_thread_interaction",
                id: interaction.id,
                company_id,
                subject: &subject,
                title: &format!(
                    "{} on an issue thread.",
                    pc_core::attention::interaction_label(&interaction.kind)
                ),
                severity: "medium",
                activity_at: interaction.updated_at,
                detail: &detail,
            });
            attention_item["decisionVerbs"] = json!(pc_core::attention::interaction_verbs(
                &interaction.kind,
                &interaction.payload,
            ));
            attention_item
        })
        .collect()
}

fn build_join_items(rows: Vec<JoinRequestRow>, company_id: Uuid) -> Vec<Value> {
    rows.into_iter()
        .map(|join| {
            let label = if join.request_type == "agent" {
                join.agent_name
                    .clone()
                    .unwrap_or_else(|| "Agent join request".to_owned())
            } else {
                join.request_email_snapshot
                    .clone()
                    .or(join.requesting_user_id.clone())
                    .unwrap_or_else(|| "Human join request".to_owned())
            };
            let subject = subject(
                "join_request",
                join.id,
                company_id,
                Some(&label),
                None,
                Some(&join.status),
            );
            let detail = json!({"kind":"generic","summaryExcerpt":label,"images":[]});
            let mut result = item(ItemInput {
                source_kind: "join_request",
                id: join.id,
                company_id,
                subject: &subject,
                title: "Join request is pending approval.",
                severity: "medium",
                activity_at: join.updated_at,
                detail: &detail,
            });
            result["decisionVerbs"] = json!([
                {"id":"approve","label":"Approve","description":"Approve this join request."},
                {"id":"reject","label":"Reject","description":"Reject this join request."}
            ]);
            result
        })
        .collect()
}

fn build_recovery_items(rows: Vec<IssueRecoveryActionRow>, company_id: Uuid) -> Vec<Value> {
    rows.into_iter().map(|recovery| {
        let dedup_key = format!("recovery:{}:{}:{}:{}", recovery.kind, recovery.source_issue_id, recovery.cause, recovery.fingerprint);
        let detail = json!({"kind":"generic","summaryExcerpt":recovery.next_action,"images":[]});
        let mut result = item(ItemInput { source_kind: "recovery_action", id: recovery.id, company_id, subject: &subject("recovery_action", recovery.id, company_id, Some(&recovery.next_action), None, Some(&recovery.status)), title: if recovery.status == "escalated" { "Recovery action escalated to a human owner." } else { "Recovery action is assigned to a human owner." }, severity: if recovery.status == "escalated" { "high" } else { "medium" }, activity_at: recovery.updated_at, detail: &detail });
        result["decisionVerbs"] = json!([
            {"id":"resolve","label":"Resolve","description":"Record the recovery outcome."},
            {"id":"reassign","label":"Reassign","description":"Move the recovery to another owner."},
            {"id":"cancel","label":"Cancel","description":"Cancel the recovery action."}
        ]);
        result["dedupKey"] = json!(dedup_key);
        result
    }).collect()
}

fn build_budget_items(rows: Vec<IncidentRow>, company_id: Uuid) -> Vec<Value> {
    rows.into_iter().filter_map(|incident| {
        if incident.amount_limit <= 0 { return None; }
        let observed_percent = ((incident.amount_observed as f64 / incident.amount_limit as f64) * 100.0).round();
        if incident.threshold_type != "hard" && observed_percent < 85.0 { return None; }
        let title = format!("{} budget {}", incident.scope_type, if incident.threshold_type == "hard" { "hard stop" } else { "warning" });
        let detail = json!({"kind":"budget","observedPercent":observed_percent,"amountObserved":incident.amount_observed,"amountLimit":incident.amount_limit,"images":[]});
        let mut result = item(ItemInput { source_kind: "budget_alert", id: incident.id, company_id, subject: &subject("budget_incident", incident.id, company_id, Some(&title), None, Some(&incident.status)), title: if incident.threshold_type == "hard" { "Budget hard stop was reached." } else { "Budget crossed the 85% warning threshold." }, severity: if incident.threshold_type == "hard" { "high" } else { "medium" }, activity_at: incident.updated_at, detail: &detail });
        result["decisionVerbs"] = json!([{"id":"raise_budget_and_resume","label":"Raise budget","description":"Raise the budget and resume paused work."},{"id":"keep_paused","label":"Keep paused","description":"Dismiss or keep the budget stop in place."}]);
        result["dedupKey"] = json!(format!("budget:{}:{}:{}", incident.policy_id, incident.window_start, incident.threshold_type));
        Some(result)
    }).collect()
}

fn build_agent_error_items(rows: Vec<AgentRow>, company_id: Uuid) -> Vec<Value> {
    rows.into_iter().map(|agent| {
        let detail = json!({"kind":"agent_error","agentName":agent.name,"failureReasonExcerpt":agent.error_reason,"images":[]});
        let mut result = item(ItemInput { source_kind: "agent_error_alert", id: agent.id, company_id, subject: &subject("agent", agent.id, company_id, Some(&agent.name), None, Some(&agent.status)), title: "Agent is in error status and needs operator action or dismissal.", severity: "high", activity_at: agent.updated_at, detail: &detail });
        result["decisionVerbs"] = json!([{"id":"inspect","label":"Inspect","description":"Inspect the agent error."},{"id":"dismiss","label":"Dismiss","description":"Dismiss this alert."}]);
        result
    }).collect()
}

fn build_productivity_items(
    rows: Vec<ProductivityReviewAttentionRow>,
    company_id: Uuid,
) -> Vec<Value> {
    rows.into_iter().map(|review| {
        let stable_origin = review.origin_fingerprint.clone().or_else(|| review.origin_id.map(|id| id.to_string())).unwrap_or_else(|| review.id.to_string());
        let mut result = item(ItemInput { source_kind: "productivity_review", id: review.id, company_id, subject: &subject("issue", review.id, company_id, Some(&review.title), review.identifier.as_deref(), Some(&review.status)), title: "Productivity review is awaiting a human decision.", severity: if review.priority == "critical" { "critical" } else if review.priority == "high" { "high" } else { "medium" }, activity_at: review.updated_at, detail: &json!({"kind":"generic","summaryExcerpt":review.title,"images":[]}) });
        result["decisionVerbs"] = json!([{"id":"resolve","label":"Resolve","description":"Record a productivity review outcome."},{"id":"dismiss","label":"Dismiss","description":"Dismiss this review for now."},{"id":"reassign","label":"Reassign","description":"Move the review to another owner."}]);
        result["dedupKey"] = json!(format!("productivity_review:{stable_origin}"));
        result
    }).collect()
}

fn build_review_items(rows: Vec<ReviewAttentionRow>, company_id: Uuid) -> Vec<Value> {
    rows.into_iter().filter(|review| {
        review.assignee_user_id.is_some() || review.has_pending_approval ||
            review.execution_state.as_ref().is_some_and(|state| {
                state.get("status").and_then(Value::as_str) == Some("pending") &&
                state.get("currentParticipant").and_then(Value::as_object)
                    .and_then(|participant| participant.get("type"))
                    .and_then(Value::as_str) == Some("user")
            })
    }).map(|review| {
        let title = if review.has_pending_approval { "Issue is in review with a linked pending approval." } else { "Issue is in review and assigned to a user." };
        let mut result = item(ItemInput { source_kind: "review", id: review.id, company_id, subject: &subject("issue", review.id, company_id, Some(&review.title), review.identifier.as_deref(), Some(&review.status)), title, severity: "medium", activity_at: review.updated_at, detail: &json!({"kind":"generic","summaryExcerpt":review.title,"images":[]}) });
        result["decisionVerbs"] = json!([{"id":"approve","label":"Approve","description":"Approve the review and advance the issue."},{"id":"request_changes","label":"Request changes","description":"Return the issue to the assignee with changes requested."}]);
        result
    }).collect()
}

fn summarize_counts(items: &[Value]) -> Map<String, Value> {
    let mut counts = Map::new();
    for kind in [
        "approval",
        "decision",
        "issue_thread_interaction",
        "join_request",
        "recovery_action",
        "productivity_review",
        "blocker_attention",
        "review",
        "failed_run",
        "budget_alert",
        "agent_error_alert",
    ] {
        let count = items
            .iter()
            .filter(|item| item["sourceKind"] == kind)
            .count();
        counts.insert(kind.to_owned(), json!(count));
    }
    counts
}

fn attention_source_rank(source: &str) -> u8 {
    match source {
        "failed_run" => 0,
        "recovery_action" => 1,
        "blocker_attention" => 2,
        "budget_alert" => 3,
        "agent_error_alert" => 4,
        "approval" => 5,
        "decision" => 6,
        "issue_thread_interaction" => 7,
        "review" => 8,
        "productivity_review" => 9,
        "join_request" => 10,
        _ => 255,
    }
}

fn attention_severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 255,
    }
}

fn compare_attention_json(left: &Value, right: &Value) -> std::cmp::Ordering {
    let time = right["activityAt"]
        .as_str()
        .cmp(&left["activityAt"].as_str());
    if time != std::cmp::Ordering::Equal {
        return time;
    }
    let severity = attention_severity_rank(left["severity"].as_str().unwrap_or("")).cmp(
        &attention_severity_rank(right["severity"].as_str().unwrap_or("")),
    );
    if severity != std::cmp::Ordering::Equal {
        return severity;
    }
    let source = attention_source_rank(left["sourceKind"].as_str().unwrap_or("")).cmp(
        &attention_source_rank(right["sourceKind"].as_str().unwrap_or("")),
    );
    if source != std::cmp::Ordering::Equal {
        return source;
    }
    left["dedupKey"]
        .to_string()
        .cmp(&right["dedupKey"].to_string())
}

fn normalize_attention_identity(item: &mut Value) {
    let source = item["sourceKind"].as_str().unwrap_or_default().to_owned();
    let id = item["id"]
        .as_str()
        .and_then(|value| value.rsplit(':').next())
        .unwrap_or_default()
        .to_owned();
    let existing = item["dedupKey"].as_str().unwrap_or_default();
    let dedup_key = if existing.is_empty() || existing == id {
        format!("{source}:{id}")
    } else {
        existing.to_owned()
    };
    item["dedupKey"] = json!(dedup_key);
    item["dismissalKey"] = json!(format!("attention:{dedup_key}"));
    item["id"] = json!(format!("{source}:{dedup_key}"));
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Query(query): Query<AttentionQuery>,
) -> ApiResult<Json<Value>> {
    if !CompanyRepo::new(&state.db).exists(company_id).await? {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    let include_dismissed = query.include_dismissed.unwrap_or(false);
    let approvals = fetch_approvals(&state, company_id).await?;
    let blocked = fetch_blocked(&state, company_id).await?;
    let failed_runs = fetch_failed_runs(&state, company_id).await?;
    let decisions = fetch_open_decisions(&state, company_id).await?;
    let interactions = fetch_pending_interactions(&state, company_id).await?;
    let joins = fetch_pending_joins(&state, company_id).await?;
    let recovery_actions = fetch_recovery_actions(&state, company_id).await?;
    let budget_incidents = fetch_budget_incidents(&state, company_id).await?;
    let error_agents = fetch_error_agents(&state, company_id).await?;
    let productivity_reviews = fetch_productivity_reviews(&state, company_id).await?;
    let reviews = fetch_reviews(&state, company_id).await?;
    let mut items = Vec::new();
    items.extend(build_approval_items(approvals, company_id));
    items.extend(build_blocker_items(blocked, company_id));
    items.extend(build_failed_run_items(failed_runs, company_id));
    items.extend(build_decision_items(decisions, company_id));
    items.extend(build_interaction_items(interactions, company_id));
    items.extend(build_join_items(joins, company_id));
    items.extend(build_recovery_items(recovery_actions, company_id));
    items.extend(build_budget_items(budget_incidents, company_id));
    items.extend(build_agent_error_items(error_agents, company_id));
    items.extend(build_productivity_items(productivity_reviews, company_id));
    items.extend(build_review_items(reviews, company_id));
    for current in &mut items {
        normalize_attention_identity(current);
    }

    let dismissal_rows = match require_user_id(&state, &headers).await {
        Ok(user_id) => {
            InboxRepo::new(&state.db)
                .list_for_user(company_id, &user_id)
                .await?
        }
        Err(_) => Vec::new(),
    };
    let now = Utc::now();
    let dismissals: std::collections::HashMap<_, _> = dismissal_rows
        .into_iter()
        .map(|row| (row.item_key.clone(), row))
        .collect();
    items.retain_mut(|current| {
        let key = current["dismissalKey"].as_str().unwrap_or_default();
        let Some(row) = dismissals.get(key) else { return true; };
        let active = if row.kind == "snooze" {
            row.snoozed_until.map(|until| until.as_datetime() > now).unwrap_or(false)
        } else {
            row.dismissed_at.as_datetime() >= current["activityAt"].as_str().and_then(|value| value.parse::<DateTime<Utc>>().ok()).unwrap_or_default()
        };
        current["dismissal"] = json!({"kind":row.kind,"dismissedAt":row.dismissed_at,"snoozedUntil":row.snoozed_until,"isActive":active});
        include_dismissed || !active
    });
    items.sort_by(compare_attention_json);
    let mut deduped = std::collections::BTreeMap::<String, Value>::new();
    for current in items {
        let key = current["dedupKey"].to_string();
        match deduped.get(&key) {
            Some(existing)
                if compare_attention_json(existing, &current) != std::cmp::Ordering::Greater => {}
            _ => {
                deduped.insert(key, current);
            }
        }
    }
    let mut items: Vec<Value> = deduped.into_values().collect();
    items.sort_by(compare_attention_json);
    for (rank, current) in items.iter_mut().enumerate() {
        current["rank"] = json!(rank);
    }
    let counts = summarize_counts(&items);
    Ok(Json(json!({
        "companyId": company_id,
        "generatedAt": Utc::now(),
        "totalCount": items.len(),
        "countsBySourceKind": counts,
        "items": items
    })))
}
