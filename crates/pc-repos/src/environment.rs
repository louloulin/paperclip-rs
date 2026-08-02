//! `environment` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct EnvironmentRepo<'a> {
    pub db: &'a Db,
}

impl<'a> EnvironmentRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list(&self) -> sqlx::Result<Vec<EnvironmentRow>> {
        let sql = "SELECT id, '00000000-0000-0000-0000-000000000000'::uuid AS company_id, name, '' AS status, created_at, updated_at FROM environments ORDER BY created_at DESC";
        sqlx::query_as::<_, EnvironmentRow>(sql)
            .fetch_all(self.db.pool())
            .await
    }
}
