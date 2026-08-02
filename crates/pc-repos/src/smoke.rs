//! `smoke_runs` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmokeRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct SmokeRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SmokeRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list(&self, company_id: Uuid) -> sqlx::Result<Vec<SmokeRow>> {
        sqlx::query_as::<_, SmokeRow>(
            "SELECT id, company_id, trigger AS name, status, created_at, updated_at FROM smoke_runs WHERE company_id = $1 ORDER BY started_at DESC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<SmokeRow>> {
        self.list(company_id).await
    }
}
