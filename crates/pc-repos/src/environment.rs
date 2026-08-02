//! `environment` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct EnvironmentRow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub driver: String,
    pub status: String,
    pub config: serde_json::Value,
    pub env_vars: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str =
    "id, name, description, driver, status, config, env_vars, metadata, created_at, updated_at";

pub struct EnvironmentRepo<'a> {
    pub db: &'a Db,
}

impl<'a> EnvironmentRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list(&self) -> sqlx::Result<Vec<EnvironmentRow>> {
        let sql = format!("SELECT {COLS} FROM environments ORDER BY created_at DESC");
        sqlx::query_as::<_, EnvironmentRow>(&sql)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<EnvironmentRow>> {
        let sql = format!("SELECT {COLS} FROM environments WHERE id = $1");
        sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        name: &str,
        driver: &str,
        config: serde_json::Value,
    ) -> sqlx::Result<EnvironmentRow> {
        let sql = format!(
            "INSERT INTO environments (name, driver, config) VALUES ($1,$2,$3) RETURNING {COLS}"
        );
        sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(name)
            .bind(driver)
            .bind(config)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn update(
        &self,
        id: Uuid,
        name: Option<&str>,
        status: Option<&str>,
        config: Option<serde_json::Value>,
    ) -> sqlx::Result<Option<EnvironmentRow>> {
        let sql = format!(
            "UPDATE environments SET name=COALESCE($2,name), status=COALESCE($3,status), \
             config=COALESCE($4,config), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(id)
            .bind(name)
            .bind(status)
            .bind(config)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM environments WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
