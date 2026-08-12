//! Runtime service authz DB loaders used by the HTTP layer.
//!
//! Thin wrappers around sqlx queries that hydrate a
//! `pc_authz::RuntimeServiceContext` so the pure-function helpers in
//! `pc-authz::runtime_service` stay IO-free.
//!
//! Mirrors Node `server/src/routes/workspace-runtime-service-authz.ts` DB reads.

//! Runtime service authz loaders.
use sqlx::FromRow;
use uuid::Uuid;

use pc_authz::{AgentContextRow, IssueContextRow, ProjectContextRow, RunContextRow};
use pc_db::Db;

#[derive(Debug, Clone, FromRow)]
struct AgentFieldsRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub role: String,
    pub permissions: serde_json::Value,
}

#[derive(Debug, Clone, FromRow)]
struct RunFieldsRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub context_snapshot: Option<serde_json::Value>,
}

#[derive(Debug, Clone, FromRow)]
struct IssueFieldsRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
    pub execution_workspace_id: Option<Uuid>,
    pub assignee_agent_id: Option<Uuid>,
    pub status: String,
    pub hidden_at: Option<chrono::DateTime<chrono::Utc>>,
    pub execution_policy: Option<serde_json::Value>,
}

#[derive(Debug, Clone, FromRow)]
struct ProjectFieldsRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub execution_workspace_policy: Option<serde_json::Value>,
}

#[derive(Debug, Clone, FromRow)]
struct AgentIdRow {
    pub id: Uuid,
    pub reports_to: Option<Uuid>,
}

const PC_WORKSPACE_RUNTIME_ELIGIBLE_STATUSES: &[&str] =
    &["backlog", "todo", "in_progress", "in_review", "blocked"];

pub struct RuntimeServiceAuthzLoader<'a> {
    db: &'a Db,
}

impl<'a> RuntimeServiceAuthzLoader<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn load_actor_agent(
        &self,
        agent_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Option<AgentContextRow>> {
        let row: Option<AgentFieldsRow> = sqlx::query_as(
            "SELECT id, company_id, role, permissions FROM agents              WHERE id = $1 AND company_id = $2 AND status <> 'terminated'",
        )
        .bind(agent_id).bind(company_id)
        .fetch_optional(self.db.pool()).await?;
        Ok(row.map(|r| AgentContextRow {
            id: r.id,
            company_id: r.company_id,
            role: r.role,
            permissions: Some(r.permissions),
        }))
    }

    pub async fn load_actor_run(
        &self,
        run_id: Uuid,
        company_id: Uuid,
        agent_id: Uuid,
    ) -> sqlx::Result<Option<RunContextRow>> {
        let row: Option<RunFieldsRow> = sqlx::query_as(
            "SELECT id, company_id, agent_id, context_snapshot FROM heartbeat_runs              WHERE id = $1 AND company_id = $2 AND agent_id = $3",
        )
        .bind(run_id).bind(company_id).bind(agent_id)
        .fetch_optional(self.db.pool()).await?;
        Ok(row.map(|r| RunContextRow {
            id: r.id,
            company_id: r.company_id,
            agent_id: r.agent_id,
            context_snapshot: r.context_snapshot,
        }))
    }

    pub async fn load_run_issue(
        &self,
        run_issue_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Option<IssueContextRow>> {
        let row: Option<(Uuid, Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>, String, Option<chrono::DateTime<chrono::Utc>>, Option<serde_json::Value>, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT i.id, i.company_id, i.project_id, i.project_workspace_id,                     i.execution_workspace_id, i.assignee_agent_id, i.status,                     i.hidden_at, i.execution_policy, p.execution_workspace_policy              FROM issues i LEFT JOIN projects p                 ON p.id = i.project_id AND p.company_id = i.company_id              WHERE i.id = $1 AND i.company_id = $2",
        )
        .bind(run_issue_id).bind(company_id)
        .fetch_optional(self.db.pool()).await?;
        Ok(row.map(
            |(id, cid, pid, pwid, ewid, aid, status, hidden, exec_pol, proj_pol)| IssueContextRow {
                id,
                company_id: cid,
                project_id: pid,
                project_workspace_id: pwid,
                execution_workspace_id: ewid,
                assignee_agent_id: aid,
                status,
                hidden_at: hidden.is_some(),
                execution_policy: exec_pol,
                project_execution_workspace_policy: proj_pol,
            },
        ))
    }

    pub async fn list_linked_scope_issues_for_project_workspace(
        &self,
        project_workspace_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<IssueContextRow>> {
        let rows: Vec<IssueFieldsRow> = sqlx::query_as(
            "SELECT id, company_id, project_id, project_workspace_id,                     execution_workspace_id, assignee_agent_id, status,                     hidden_at, execution_policy              FROM issues              WHERE company_id = $1 AND project_workspace_id = $2                 AND status = ANY($3) AND hidden_at IS NULL",
        )
        .bind(company_id).bind(project_workspace_id)
        .bind(PC_WORKSPACE_RUNTIME_ELIGIBLE_STATUSES)
        .fetch_all(self.db.pool()).await?;
        Ok(rows
            .into_iter()
            .map(|r| IssueContextRow {
                id: r.id,
                company_id: r.company_id,
                project_id: r.project_id,
                project_workspace_id: r.project_workspace_id,
                execution_workspace_id: r.execution_workspace_id,
                assignee_agent_id: r.assignee_agent_id,
                status: r.status,
                hidden_at: r.hidden_at.is_some(),
                execution_policy: r.execution_policy,
                project_execution_workspace_policy: None,
            })
            .collect())
    }

    pub async fn list_linked_scope_issues_for_execution_workspace(
        &self,
        execution_workspace_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<IssueContextRow>> {
        let rows: Vec<IssueFieldsRow> = sqlx::query_as(
            "SELECT id, company_id, project_id, project_workspace_id,                     execution_workspace_id, assignee_agent_id, status,                     hidden_at, execution_policy              FROM issues              WHERE company_id = $1 AND execution_workspace_id = $2                 AND status = ANY($3) AND hidden_at IS NULL",
        )
        .bind(company_id).bind(execution_workspace_id)
        .bind(PC_WORKSPACE_RUNTIME_ELIGIBLE_STATUSES)
        .fetch_all(self.db.pool()).await?;
        Ok(rows
            .into_iter()
            .map(|r| IssueContextRow {
                id: r.id,
                company_id: r.company_id,
                project_id: r.project_id,
                project_workspace_id: r.project_workspace_id,
                execution_workspace_id: r.execution_workspace_id,
                assignee_agent_id: r.assignee_agent_id,
                status: r.status,
                hidden_at: r.hidden_at.is_some(),
                execution_policy: r.execution_policy,
                project_execution_workspace_policy: None,
            })
            .collect())
    }

    pub async fn load_linked_assignee_issue_in_workspace(
        &self,
        project_workspace_id: Option<Uuid>,
        execution_workspace_id: Option<Uuid>,
        eligible_agent_ids: &[Uuid],
        company_id: Uuid,
    ) -> sqlx::Result<Option<IssueContextRow>> {
        if eligible_agent_ids.is_empty() {
            return Ok(None);
        }
        let row: Option<IssueFieldsRow> = sqlx::query_as(
            "SELECT id, company_id, project_id, project_workspace_id,                     execution_workspace_id, assignee_agent_id, status,                     hidden_at, execution_policy              FROM issues              WHERE company_id = $1                 AND status = ANY($4) AND hidden_at IS NULL                 AND assignee_agent_id = ANY($5)                 AND (($2::uuid IS NOT NULL AND project_workspace_id = $2)                   OR ($3::uuid IS NOT NULL AND execution_workspace_id = $3))              ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(company_id)
        .bind(project_workspace_id)
        .bind(execution_workspace_id)
        .bind(PC_WORKSPACE_RUNTIME_ELIGIBLE_STATUSES)
        .bind(eligible_agent_ids)
        .fetch_optional(self.db.pool()).await?;
        Ok(row.map(|r| IssueContextRow {
            id: r.id,
            company_id: r.company_id,
            project_id: r.project_id,
            project_workspace_id: r.project_workspace_id,
            execution_workspace_id: r.execution_workspace_id,
            assignee_agent_id: r.assignee_agent_id,
            status: r.status,
            hidden_at: r.hidden_at.is_some(),
            execution_policy: r.execution_policy,
            project_execution_workspace_policy: None,
        }))
    }

    pub async fn load_project(
        &self,
        project_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Option<ProjectContextRow>> {
        let row: Option<ProjectFieldsRow> = sqlx::query_as(
            "SELECT id, company_id, execution_workspace_policy FROM projects              WHERE id = $1 AND company_id = $2",
        )
        .bind(project_id).bind(company_id)
        .fetch_optional(self.db.pool()).await?;
        Ok(row.map(|r| ProjectContextRow {
            id: r.id,
            company_id: r.company_id,
            execution_workspace_policy: r.execution_workspace_policy,
        }))
    }

    pub async fn list_reporting_subtree_agent_ids(
        &self,
        company_id: Uuid,
        actor_agent_id: Uuid,
    ) -> sqlx::Result<Vec<Uuid>> {
        let rows: Vec<AgentIdRow> = sqlx::query_as(
            "SELECT id, reports_to FROM agents              WHERE company_id = $1 AND status <> 'terminated'",
        )
        .bind(company_id)
        .fetch_all(self.db.pool()).await?;
        let mut reports_by_manager: std::collections::HashMap<Uuid, Vec<Uuid>> =
            std::collections::HashMap::new();
        for r in &rows {
            if let Some(mgr) = r.reports_to {
                reports_by_manager.entry(mgr).or_default().push(r.id);
            }
        }
        let mut visited: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
        visited.insert(actor_agent_id);
        let mut queue: std::collections::VecDeque<Uuid> = std::collections::VecDeque::new();
        queue.push_back(actor_agent_id);
        while let Some(cur) = queue.pop_front() {
            if let Some(reports) = reports_by_manager.get(&cur) {
                for r in reports {
                    if visited.insert(*r) {
                        queue.push_back(*r);
                    }
                }
            }
        }
        Ok(visited.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eligible_statuses_match_authz_constant() {
        assert_eq!(
            PC_WORKSPACE_RUNTIME_ELIGIBLE_STATUSES,
            pc_authz::WORKSPACE_RUNTIME_ELIGIBLE_ISSUE_STATUSES,
        );
    }
}
