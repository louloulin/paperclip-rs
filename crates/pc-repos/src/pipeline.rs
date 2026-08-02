//! `pipeline` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PipelineRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub enforce_transitions: bool,
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, project_id, key, name, description, enforce_transitions, \
    created_by_user_id, created_by_agent_id, archived_at, created_at, updated_at";

pub struct PipelineRepo<'a> {
    pub db: &'a Db,
}

impl<'a> PipelineRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<PipelineRow>> {
        let sql =
            format!("SELECT {COLS} FROM pipelines WHERE company_id = $1 ORDER BY created_at DESC");
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<PipelineRow>> {
        let sql = format!("SELECT {COLS} FROM pipelines WHERE id = $1");
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        key: &str,
        name: &str,
        description: Option<&str>,
    ) -> sqlx::Result<PipelineRow> {
        let sql = format!(
            "INSERT INTO pipelines (company_id, key, name, description) VALUES ($1,$2,$3,$4) RETURNING {COLS}"
        );
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(company_id)
            .bind(key)
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
    ) -> sqlx::Result<Option<PipelineRow>> {
        let sql = format!(
            "UPDATE pipelines SET name=COALESCE($2,name), description=COALESCE($3,description), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(id)
            .bind(name)
            .bind(description)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM pipelines WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
