//! 统一注意力队列。

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_repos::approval::ApprovalRepo;
use pc_repos::company::CompanyRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue::IssueRepo;

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
            item(ItemInput {
                source_kind: "blocker_attention",
                id: issue.id,
                company_id,
                subject: &subject,
                title: &title,
                severity: "high",
                activity_at: issue.updated_at,
                detail: &detail,
            })
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

async fn list(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<AttentionQuery>,
) -> ApiResult<Json<Value>> {
    if !CompanyRepo::new(&state.db)
        .exists(company_id)
        .await?
    {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    let _include_dismissed = query.include_dismissed.unwrap_or(false);
    let approvals = fetch_approvals(&state, company_id).await?;
    let blocked = fetch_blocked(&state, company_id).await?;
    let failed_runs = fetch_failed_runs(&state, company_id).await?;
    let mut items = Vec::new();
    items.extend(build_approval_items(approvals, company_id));
    items.extend(build_blocker_items(blocked, company_id));
    items.extend(build_failed_run_items(failed_runs, company_id));
    items.sort_by(|left, right| {
        right["activityAt"]
            .as_str()
            .cmp(&left["activityAt"].as_str())
    });
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
