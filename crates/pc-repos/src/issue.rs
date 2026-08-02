//! issues 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub approval_state: String,
    pub checkout_session_id: Option<String>,
    pub checkout_at: Option<Timestamp>,
    pub due_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const SELECT: &str = "id, company_id, project_id, identifier, title, description, status, priority, assignee_agent_id, assignee_user_id, created_by_user_id, created_by_agent_id, approval_state, checkout_session_id, checkout_at, due_at, closed_at, created_at, updated_at";

pub struct IssueRepo<'a> { pub db: &'a Db }

impl<'a> IssueRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<Issue>> {
        let sql = format!("SELECT {SELECT} FROM issues WHERE company_id = $1 ORDER BY created_at DESC");
        Ok(sqlx::query_as::<_, Issue>(&sql).bind(company_id).fetch_all(self.db.pool()).await?)
    }
    pub async fn find(&self, id: Uuid) -> RepoResult<Option<Issue>> {
        let sql = format!("SELECT {SELECT} FROM issues WHERE id = $1");
        Ok(sqlx::query_as::<_, Issue>(&sql).bind(id).fetch_optional(self.db.pool()).await?)
    }
    pub async fn create(&self, company_id: Uuid, identifier: &str, title: &str, description: Option<&str>, priority: &str) -> RepoResult<Issue> {
        if title.trim().is_empty() {
            return Err(RepoError::Invalid("issue title cannot be empty".into()));
        }
        let sql = format!("INSERT INTO issues (company_id, identifier, title, description, priority) VALUES ($1, $2, $3, $4, $5) RETURNING {SELECT}");
        Ok(sqlx::query_as::<_, Issue>(&sql).bind(company_id).bind(identifier).bind(title).bind(description).bind(priority).fetch_one(self.db.pool()).await?)
    }
    pub async fn assign_agent(&self, issue_id: Uuid, agent_id: Uuid) -> RepoResult<Issue> {
        let sql = format!("UPDATE issues SET assignee_agent_id = $2, updated_at = now() WHERE id = $1 RETURNING {SELECT}");
        sqlx::query_as::<_, Issue>(&sql).bind(issue_id).bind(agent_id).fetch_optional(self.db.pool()).await?
            .ok_or(RepoError::NotFound { entity: "issue", id: issue_id.to_string() })
    }
}
