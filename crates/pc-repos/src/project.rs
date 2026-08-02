//! `project` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ProjectRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub goal_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub lead_agent_id: Option<Uuid>,
    pub target_date: Option<chrono::NaiveDate>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub env: Option<serde_json::Value>,
    pub pause_reason: Option<String>,
    pub paused_at: Option<Timestamp>,
    pub execution_workspace_policy: Option<serde_json::Value>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, goal_id, name, description, status, lead_agent_id, \
    target_date, color, icon, env, pause_reason, paused_at, execution_workspace_policy, \
    archived_at, created_at, updated_at";

pub struct ProjectRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ProjectRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<ProjectRow>> {
        let sql =
            format!("SELECT {COLS} FROM projects WHERE company_id = $1 ORDER BY created_at DESC");
        sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<ProjectRow>> {
        let sql = format!("SELECT {COLS} FROM projects WHERE id = $1");
        sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        name: &str,
        description: Option<&str>,
    ) -> sqlx::Result<ProjectRow> {
        let sql = format!(
            "INSERT INTO projects (company_id, name, description) VALUES ($1,$2,$3) RETURNING {COLS}"
        );
        sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .bind(name)
            .bind(description)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn update(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
    ) -> sqlx::Result<Option<ProjectRow>> {
        let sql = format!(
            "UPDATE projects SET name=COALESCE($2,name), description=COALESCE($3,description), \
             status=COALESCE($4,status), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(id)
            .bind(name)
            .bind(description)
            .bind(status)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
