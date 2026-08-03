//! 公司侧边栏徽标聚合。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

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
    let pool = state.db.pool();

    // Agent counts by status
    let (agent_errors, agent_running, agent_paused): (i64, i64, i64) = sqlx::query_as(
        "SELECT \
            COUNT(*) FILTER (WHERE status = 'error')::bigint, \
            COUNT(*) FILTER (WHERE status = 'running')::bigint, \
            COUNT(*) FILTER (WHERE status = 'paused')::bigint \
         FROM agents WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .unwrap_or((0, 0, 0));

    // Issue counts by status
    let (issue_blocked, issue_in_progress, issue_needs_review): (i64, i64, i64) = sqlx::query_as(
        "SELECT \
            COUNT(*) FILTER (WHERE status = 'blocked')::bigint, \
            COUNT(*) FILTER (WHERE status = 'in_progress')::bigint, \
            COUNT(*) FILTER (WHERE status = 'needs_review')::bigint \
         FROM issues WHERE company_id = $1 AND hidden_at IS NULL",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .unwrap_or((0, 0, 0));

    // Unread = no last_seen_at >= created_at for current user (simplified: just count issues with no assignee_user_id activity)
    let issue_unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM issues WHERE company_id = $1 AND hidden_at IS NULL \
         AND (assignee_user_id IS NULL OR assignee_user_id = '') \
         AND created_at > now() - interval '7 days'",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    // Approvals: pending
    let approvals_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM approvals WHERE company_id = $1 AND status = 'pending'",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    // Costs: agents over budget
    let cost_alerts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM agents WHERE company_id = $1 \
         AND budget_monthly_cents > 0 AND spent_monthly_cents >= budget_monthly_cents",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    // Runs: heartbeat_runs recently failed / currently running
    let (runs_failed_recent, runs_running): (i64, i64) = sqlx::query_as(
        "SELECT \
            COUNT(*) FILTER (WHERE status = 'failed' AND created_at > now() - interval '24 hours')::bigint, \
            COUNT(*) FILTER (WHERE status IN ('queued','running'))::bigint \
         FROM heartbeat_runs WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_one(pool)
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
