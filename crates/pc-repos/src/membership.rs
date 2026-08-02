//! 项目、Agent、文档的用户成员关系仓储。

use chrono::Utc;
use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ProjectMembershipRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub state: String,
    pub starred_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    pub project_archived_at: Option<Timestamp>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentMembershipRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub user_id: String,
    pub state: String,
    pub starred_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    pub agent_status: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct DocumentMembershipRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub document_id: Uuid,
    pub user_id: String,
    pub starred_at: Option<Timestamp>,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceMembershipSnapshot {
    pub project_memberships: std::collections::BTreeMap<String, String>,
    pub agent_memberships: std::collections::BTreeMap<String, String>,
    pub starred_project_ids: Vec<String>,
    pub starred_agent_ids: Vec<String>,
    pub starred_document_ids: Vec<String>,
    pub project_starred_at: std::collections::BTreeMap<String, Timestamp>,
    pub agent_starred_at: std::collections::BTreeMap<String, Timestamp>,
    pub document_starred_at: std::collections::BTreeMap<String, Timestamp>,
    pub updated_at: Option<Timestamp>,
}

pub struct MembershipRepo<'a> {
    pub db: &'a Db,
}

impl<'a> MembershipRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn has_active_company_access(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM company_memberships \
             WHERE company_id = $1 AND principal_type = 'user' \
               AND principal_id = $2 AND status = 'active' LIMIT 1",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    pub async fn list_projects(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Vec<ProjectMembershipRow>> {
        sqlx::query_as::<_, ProjectMembershipRow>(
            "SELECT pm.id, pm.company_id, pm.project_id, pm.user_id, pm.state, \
                    pm.starred_at, pm.updated_at, p.archived_at AS project_archived_at \
             FROM project_memberships pm \
             INNER JOIN projects p ON p.id = pm.project_id AND p.company_id = pm.company_id \
             WHERE pm.company_id = $1 AND pm.user_id = $2",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn list_agents(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Vec<AgentMembershipRow>> {
        sqlx::query_as::<_, AgentMembershipRow>(
            "SELECT am.id, am.company_id, am.agent_id, am.user_id, am.state, \
                    am.starred_at, am.updated_at, a.status AS agent_status \
             FROM agent_memberships am \
             INNER JOIN agents a ON a.id = am.agent_id AND a.company_id = am.company_id \
             WHERE am.company_id = $1 AND am.user_id = $2",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn list_documents(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Vec<DocumentMembershipRow>> {
        sqlx::query_as::<_, DocumentMembershipRow>(
            "SELECT dm.id, dm.company_id, dm.document_id, dm.user_id, \
                    dm.starred_at, dm.updated_at \
             FROM document_memberships dm \
             INNER JOIN documents d ON d.id = dm.document_id AND d.company_id = dm.company_id \
             WHERE dm.company_id = $1 AND dm.user_id = $2",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn snapshot(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<ResourceMembershipSnapshot> {
        let (project_rows, agent_rows, document_rows) = tokio::try_join!(
            self.list_projects(company_id, user_id),
            self.list_agents(company_id, user_id),
            self.list_documents(company_id, user_id),
        )?;
        let mut project_memberships = std::collections::BTreeMap::new();
        let mut agent_memberships = std::collections::BTreeMap::new();
        let mut starred_project_rows = Vec::new();
        let mut starred_agent_rows = Vec::new();
        let mut starred_document_rows = Vec::new();
        let mut updated_at = None;

        for row in &project_rows {
            project_memberships.insert(
                row.project_id.to_string(),
                if row.state == "left" {
                    "left"
                } else {
                    "joined"
                }
                .to_owned(),
            );
            updated_at = latest_timestamp(updated_at, Some(row.updated_at));
            if let Some(starred_at) = row.starred_at {
                if row.project_archived_at.is_none() {
                    starred_project_rows.push((row.project_id, starred_at));
                }
            }
        }
        for row in &agent_rows {
            agent_memberships.insert(
                row.agent_id.to_string(),
                if row.state == "left" {
                    "left"
                } else {
                    "joined"
                }
                .to_owned(),
            );
            updated_at = latest_timestamp(updated_at, Some(row.updated_at));
            if let Some(starred_at) = row.starred_at {
                if row.agent_status != "terminated" {
                    starred_agent_rows.push((row.agent_id, starred_at));
                }
            }
        }
        for row in &document_rows {
            updated_at = latest_timestamp(updated_at, Some(row.updated_at));
            if let Some(starred_at) = row.starred_at {
                starred_document_rows.push((row.document_id, starred_at));
            }
        }

        starred_project_rows
            .sort_by_key(|(_, timestamp)| std::cmp::Reverse(timestamp.as_datetime()));
        starred_agent_rows.sort_by_key(|(_, timestamp)| std::cmp::Reverse(timestamp.as_datetime()));
        starred_document_rows
            .sort_by_key(|(_, timestamp)| std::cmp::Reverse(timestamp.as_datetime()));

        let starred_project_ids = starred_project_rows
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        let starred_agent_ids = starred_agent_rows
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        let starred_document_ids = starred_document_rows
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        let project_starred_at = starred_project_rows
            .into_iter()
            .map(|(id, timestamp)| (id.to_string(), timestamp))
            .collect();
        let agent_starred_at = starred_agent_rows
            .into_iter()
            .map(|(id, timestamp)| (id.to_string(), timestamp))
            .collect();
        let document_starred_at = starred_document_rows
            .into_iter()
            .map(|(id, timestamp)| (id.to_string(), timestamp))
            .collect();

        Ok(ResourceMembershipSnapshot {
            project_memberships,
            agent_memberships,
            starred_project_ids,
            starred_agent_ids,
            starred_document_ids,
            project_starred_at,
            agent_starred_at,
            document_starred_at,
            updated_at,
        })
    }

    pub async fn project_exists(&self, company_id: Uuid, project_id: Uuid) -> sqlx::Result<bool> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM projects WHERE id = $1 AND company_id = $2 AND archived_at IS NULL",
        )
        .bind(project_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    pub async fn agent_exists(&self, company_id: Uuid, agent_id: Uuid) -> sqlx::Result<bool> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM agents WHERE id = $1 AND company_id = $2 AND status <> 'terminated'",
        )
        .bind(agent_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    pub async fn document_exists(&self, company_id: Uuid, document_id: Uuid) -> sqlx::Result<bool> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM documents WHERE id = $1 AND company_id = $2")
                .bind(document_id)
                .bind(company_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.is_some())
    }

    pub async fn get_project(
        &self,
        company_id: Uuid,
        user_id: &str,
        project_id: Uuid,
    ) -> sqlx::Result<Option<ProjectMembershipRow>> {
        sqlx::query_as::<_, ProjectMembershipRow>(
            "SELECT pm.id, pm.company_id, pm.project_id, pm.user_id, pm.state, \
                    pm.starred_at, pm.updated_at, p.archived_at AS project_archived_at \
             FROM project_memberships pm \
             INNER JOIN projects p ON p.id = pm.project_id AND p.company_id = pm.company_id \
             WHERE pm.company_id = $1 AND pm.user_id = $2 AND pm.project_id = $3",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(project_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn get_agent(
        &self,
        company_id: Uuid,
        user_id: &str,
        agent_id: Uuid,
    ) -> sqlx::Result<Option<AgentMembershipRow>> {
        sqlx::query_as::<_, AgentMembershipRow>(
            "SELECT am.id, am.company_id, am.agent_id, am.user_id, am.state, \
                    am.starred_at, am.updated_at, a.status AS agent_status \
             FROM agent_memberships am \
             INNER JOIN agents a ON a.id = am.agent_id AND a.company_id = am.company_id \
             WHERE am.company_id = $1 AND am.user_id = $2 AND am.agent_id = $3",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(agent_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn get_document(
        &self,
        company_id: Uuid,
        user_id: &str,
        document_id: Uuid,
    ) -> sqlx::Result<Option<DocumentMembershipRow>> {
        sqlx::query_as::<_, DocumentMembershipRow>(
            "SELECT dm.id, dm.company_id, dm.document_id, dm.user_id, \
                    dm.starred_at, dm.updated_at \
             FROM document_memberships dm \
             INNER JOIN documents d ON d.id = dm.document_id AND d.company_id = dm.company_id \
             WHERE dm.company_id = $1 AND dm.user_id = $2 AND dm.document_id = $3",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(document_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn upsert_project(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        user_id: &str,
        state: &str,
        starred_at: Option<Timestamp>,
    ) -> sqlx::Result<ProjectMembershipRow> {
        sqlx::query_as::<_, ProjectMembershipRow>(
            "INSERT INTO project_memberships \
                (company_id, project_id, user_id, state, starred_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now()) \
             ON CONFLICT (company_id, user_id, project_id) DO UPDATE SET \
                state = EXCLUDED.state, starred_at = EXCLUDED.starred_at, updated_at = now() \
             RETURNING id, company_id, project_id, user_id, state, starred_at, updated_at, \
                (SELECT archived_at FROM projects WHERE id = project_id) AS project_archived_at",
        )
        .bind(company_id)
        .bind(project_id)
        .bind(user_id)
        .bind(state)
        .bind(starred_at)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn upsert_agent(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
        user_id: &str,
        state: &str,
        starred_at: Option<Timestamp>,
    ) -> sqlx::Result<AgentMembershipRow> {
        sqlx::query_as::<_, AgentMembershipRow>(
            "INSERT INTO agent_memberships \
                (company_id, agent_id, user_id, state, starred_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now()) \
             ON CONFLICT (company_id, user_id, agent_id) DO UPDATE SET \
                state = EXCLUDED.state, starred_at = EXCLUDED.starred_at, updated_at = now() \
             RETURNING id, company_id, agent_id, user_id, state, starred_at, updated_at, \
                (SELECT status FROM agents WHERE id = agent_id) AS agent_status",
        )
        .bind(company_id)
        .bind(agent_id)
        .bind(user_id)
        .bind(state)
        .bind(starred_at)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn upsert_document(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        user_id: &str,
        starred_at: Timestamp,
    ) -> sqlx::Result<DocumentMembershipRow> {
        sqlx::query_as::<_, DocumentMembershipRow>(
            "INSERT INTO document_memberships \
                (company_id, document_id, user_id, starred_at, updated_at) \
             VALUES ($1, $2, $3, $4, now()) \
             ON CONFLICT (company_id, user_id, document_id) DO UPDATE SET \
                starred_at = COALESCE(document_memberships.starred_at, EXCLUDED.starred_at), \
                updated_at = CASE WHEN document_memberships.starred_at IS NULL THEN now() \
                                  ELSE document_memberships.updated_at END \
             RETURNING id, company_id, document_id, user_id, starred_at, updated_at",
        )
        .bind(company_id)
        .bind(document_id)
        .bind(user_id)
        .bind(starred_at)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn delete_document(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<bool> {
        Ok(sqlx::query(
            "DELETE FROM document_memberships \
             WHERE company_id = $1 AND document_id = $2 AND user_id = $3",
        )
        .bind(company_id)
        .bind(document_id)
        .bind(user_id)
        .execute(self.db.pool())
        .await?
        .rows_affected()
            > 0)
    }

    pub fn now_timestamp() -> Timestamp {
        Timestamp::from_dt(Utc::now())
    }
}

fn latest_timestamp(current: Option<Timestamp>, candidate: Option<Timestamp>) -> Option<Timestamp> {
    match (current, candidate) {
        (None, next) => next,
        (Some(previous), None) => Some(previous),
        (Some(previous), Some(next)) => Some(if next.as_datetime() > previous.as_datetime() {
            next
        } else {
            previous
        }),
    }
}
