//! Issue-graph liveness DB loader.
//!
//! 1:1 alignment with the Node loader that backs
//! `IssueGraphLivenessInput` for the pure-function classifier in
//! `crate::liveness::classifier`. The Node loader lives in the
//! recovery service; here it is split out so callers (reconciler,
//! scheduler, CLI debug command) can fetch the snapshot without
//! pulling the orchestrator.
//!
//! Behavior contract (mirrors Node):
//! - issues: company-scoped, not hidden
//! - relations: company-scoped (no type filter; the type is blocks today)
//! - agents: company-scoped, all statuses (the invokability filter runs in
//!   the classifier, not in the loader)
//! - active_runs: heartbeat_runs in execution-path statuses (queued,
//!   running, scheduled_retry)
//! - queued_wake_requests: agent_wakeup_requests in queued or
//!   deferred_issue_execution
//! - pending_interactions: issue_thread_interactions in open / pending
//! - pending_approvals: approvals in pending
//! - open_recovery_issues: issues with origin_kind =
//!   harness_liveness_escalation and status not in done / cancelled
//!
//! Loader is read-only and does not mutate DB. It returns the assembled
//! `IssueGraphLivenessInput` so the caller can pass it straight to
//! `classify_issue_graph_liveness`.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use pc_core::Timestamp;
use pc_repos::Db;

use super::types::{
    IssueGraphLivenessInput, IssueLivenessAgentInput, IssueLivenessExecutionPathInput,
    IssueLivenessIssueInput, IssueLivenessRelationInput, IssueLivenessWaitingPathInput,
};

/// Liveness load error. Wraps sqlx errors with a tag for easier routing.
#[derive(Debug, thiserror::Error)]
pub enum IssueGraphLivenessLoadError {
    #[error("sqlx: {0}")]
    Sqlx(String),
}

impl From<sqlx::Error> for IssueGraphLivenessLoadError {
    fn from(value: sqlx::Error) -> Self {
        Self::Sqlx(value.to_string())
    }
}

const ISSUE_COLS: &str = "    id, company_id, identifier, title, status,     project_id, goal_id, parent_id,     assignee_agent_id, assignee_user_id,     created_by_agent_id, created_by_user_id,     execution_policy, execution_state,     monitor_next_check_at, monitor_attempt_count";

const AGENT_COLS: &str = "    id, company_id, name, role, title, status, reports_to";

const HEARTBEAT_RUN_PATH_COLS: &str = "    company_id, context_snapshot->>'issueId' AS issue_id, agent_id, status";

const WAKEUP_PATH_COLS: &str = "    company_id, payload->>'issueId' AS issue_id, agent_id, status";

const INTERACTION_PATH_COLS: &str = "    company_id, issue_id, status";

const APPROVAL_PATH_COLS: &str = "    company_id, payload->>'issueId' AS issue_id, status";

const RECOVERY_ISSUE_COLS: &str = "    company_id, id AS issue_id, status";

fn issue_input_from_row(row: &sqlx::postgres::PgRow) -> Result<IssueLivenessIssueInput, sqlx::Error> {
    Ok(IssueLivenessIssueInput {
        id: row.try_get("id")?,
        company_id: row.try_get("company_id")?,
        identifier: row.try_get("identifier")?,
        title: row.try_get("title")?,
        status: row.try_get("status")?,
        project_id: row.try_get("project_id")?,
        goal_id: row.try_get("goal_id")?,
        parent_id: row.try_get("parent_id")?,
        assignee_agent_id: row.try_get("assignee_agent_id")?,
        assignee_user_id: row.try_get("assignee_user_id")?,
        created_by_agent_id: row.try_get("created_by_agent_id")?,
        created_by_user_id: row.try_get("created_by_user_id")?,
        execution_policy: row.try_get("execution_policy")?,
        execution_state: row.try_get("execution_state")?,
        monitor_next_check_at: row.try_get("monitor_next_check_at")?,
        monitor_attempt_count: row.try_get("monitor_attempt_count")?,
    })
}

/// Load the full liveness input for a company. The returned input is
/// ready to feed into `classify_issue_graph_liveness`.
pub async fn load_issue_graph_liveness_input(
    db: &Db,
    company_id: Uuid,
    now: DateTime<Utc>,
) -> Result<IssueGraphLivenessInput, IssueGraphLivenessLoadError> {
    let issues = sqlx::query(&format!(
        "SELECT {ISSUE_COLS} FROM issues          WHERE company_id =  AND hidden_at IS NULL"
    ))
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let issues: Vec<IssueLivenessIssueInput> = issues
        .iter()
        .map(issue_input_from_row)
        .collect::<Result<_, _>>()?;

    let relation_rows = sqlx::query(
        "SELECT company_id, issue_id AS blocker_issue_id,                 related_issue_id AS blocked_issue_id          FROM issue_relations          WHERE company_id = ",
    )
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let relations: Vec<IssueLivenessRelationInput> = relation_rows
        .iter()
        .map(|r| -> Result<_, sqlx::Error> {
            Ok(IssueLivenessRelationInput {
                company_id: r.try_get("company_id")?,
                blocker_issue_id: r.try_get("blocker_issue_id")?,
                blocked_issue_id: r.try_get("blocked_issue_id")?,
            })
        })
        .collect::<Result<_, _>>()?;

    let agent_rows = sqlx::query(&format!(
        "SELECT {AGENT_COLS} FROM agents WHERE company_id = "
    ))
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let agents: Vec<IssueLivenessAgentInput> = agent_rows
        .iter()
        .map(|r| -> Result<_, sqlx::Error> {
            Ok(IssueLivenessAgentInput {
                id: r.try_get("id")?,
                company_id: r.try_get("company_id")?,
                name: r.try_get("name")?,
                role: r.try_get("role")?,
                title: r.try_get("title")?,
                status: r.try_get("status")?,
                reports_to: r.try_get("reports_to")?,
            })
        })
        .collect::<Result<_, _>>()?;

    let active_run_rows = sqlx::query(&format!(
        "SELECT {HEARTBEAT_RUN_PATH_COLS} FROM heartbeat_runs          WHERE company_id =             AND status IN ('queued','running','scheduled_retry')            AND context_snapshot ? 'issueId'"
    ))
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let active_runs: Vec<IssueLivenessExecutionPathInput> = active_run_rows
        .iter()
        .map(|r| parse_execution_path(r))
        .collect::<Result<_, _>>()?;

    let wakeup_rows = sqlx::query(&format!(
        "SELECT {WAKEUP_PATH_COLS} FROM agent_wakeup_requests          WHERE company_id =             AND status IN ('queued','deferred_issue_execution')            AND payload ? 'issueId'"
    ))
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let queued_wake_requests: Vec<IssueLivenessExecutionPathInput> = wakeup_rows
        .iter()
        .map(|r| parse_execution_path(r))
        .collect::<Result<_, _>>()?;

    let interaction_rows = sqlx::query(&format!(
        "SELECT {INTERACTION_PATH_COLS} FROM issue_thread_interactions          WHERE company_id =             AND status IN ('open','pending')"
    ))
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let pending_interactions: Vec<IssueLivenessWaitingPathInput> = interaction_rows
        .iter()
        .map(|r| parse_waiting_path(r))
        .collect::<Result<_, _>>()?;

    let approval_rows = sqlx::query(&format!(
        "SELECT {APPROVAL_PATH_COLS} FROM approvals          WHERE company_id =  AND status = 'pending'            AND payload ? 'issueId'"
    ))
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let pending_approvals: Vec<IssueLivenessWaitingPathInput> = approval_rows
        .iter()
        .map(|r| parse_waiting_path(r))
        .collect::<Result<_, _>>()?;

    let recovery_rows = sqlx::query(&format!(
        "SELECT {RECOVERY_ISSUE_COLS} FROM issues          WHERE company_id =             AND origin_kind = 'harness_liveness_escalation'            AND status NOT IN ('done','cancelled')            AND hidden_at IS NULL"
    ))
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    let open_recovery_issues: Vec<IssueLivenessWaitingPathInput> = recovery_rows
        .iter()
        .map(|r| -> Result<_, sqlx::Error> {
            Ok(IssueLivenessWaitingPathInput {
                company_id: r.try_get("company_id")?,
                issue_id: r.try_get("issue_id")?,
                status: r.try_get("status")?,
            })
        })
        .collect::<Result<_, _>>()?;

    Ok(IssueGraphLivenessInput {
        issues,
        relations,
        agents,
        active_runs: Some(active_runs),
        queued_wake_requests: Some(queued_wake_requests),
        pending_interactions: Some(pending_interactions),
        pending_approvals: Some(pending_approvals),
        open_recovery_issues: Some(open_recovery_issues),
        now: Some(Timestamp::from_dt(now)),
    })
}

fn parse_execution_path(r: &sqlx::postgres::PgRow) -> Result<IssueLivenessExecutionPathInput, sqlx::Error> {
    Ok(IssueLivenessExecutionPathInput {
        company_id: r.try_get("company_id")?,
        issue_id: r.try_get::<Option<String>, _>("issue_id")?.and_then(|s| Uuid::parse_str(&s).ok()),
        agent_id: r.try_get("agent_id")?,
        status: r.try_get("status")?,
    })
}

fn parse_waiting_path(r: &sqlx::postgres::PgRow) -> Result<IssueLivenessWaitingPathInput, sqlx::Error> {
    let issue_id_str: String = r.try_get("issue_id")?;
    let issue_id = Uuid::parse_str(&issue_id_str).map_err(|e| sqlx::Error::ColumnDecode {
        index: "issue_id".into(),
        source: Box::new(e),
    })?;
    Ok(IssueLivenessWaitingPathInput {
        company_id: r.try_get("company_id")?,
        issue_id,
        status: r.try_get("status")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_error_displays_sqlx() {
        let err = IssueGraphLivenessLoadError::Sqlx("boom".into());
        assert!(err.to_string().contains("boom"));
    }
}
