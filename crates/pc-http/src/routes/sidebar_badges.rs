//! 公司侧边栏徽标聚合。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;
use pc_repos::agent::AgentRepo;
use pc_repos::approval::ApprovalRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue::IssueRepo;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/sidebar-badges",
        get(sidebar_badges),
    )
}

async fn sidebar_badges(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    // Agent counts by status
    let (agent_errors, agent_running, agent_paused) = AgentRepo::new(&state.db)
        .status_breakdown(company_id)
        .await
        .unwrap_or((0, 0, 0));

    // Issue counts by status
    let (issue_blocked, issue_in_progress, issue_needs_review) = IssueRepo::new(&state.db)
        .status_breakdown_visible(company_id)
        .await
        .unwrap_or((0, 0, 0));

    // Unread = no assignee_user_id activity in last 7 days
    let issue_unread = IssueRepo::new(&state.db)
        .count_unread_visible(company_id)
        .await
        .unwrap_or(0);

    // Approvals: pending
    let approvals_pending = ApprovalRepo::new(&state.db)
        .count_pending(company_id)
        .await
        .unwrap_or(0);

    // Costs: agents over budget
    let cost_alerts = AgentRepo::new(&state.db)
        .count_over_budget(company_id)
        .await
        .unwrap_or(0);

    // Runs: heartbeat_runs recently failed / currently running
    let (runs_failed_recent, runs_running) = HeartbeatRepo::new(&state.db)
        .status_breakdown(company_id)
        .await
        .unwrap_or((0, 0));

    Json(json!({
        "agents": { "errors": agent_errors, "running": agent_running, "paused": agent_paused },
        "issues": { "blocked": issue_blocked, "inProgress": issue_in_progress, "needsReview": issue_needs_review, "unread": issue_unread },
        "approvals": { "pending": approvals_pending },
        "costs": { "alerts": cost_alerts },
        "runs": { "failedRecent": runs_failed_recent, "running": runs_running }
    }))
}
