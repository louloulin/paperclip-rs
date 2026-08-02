//! `summary_slots` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummaryRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct SummaryRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SummaryRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list(&self, company_id: Uuid) -> sqlx::Result<Vec<SummaryRow>> {
        sqlx::query_as::<_, SummaryRow>(
            "SELECT id, company_id, slot_key AS name, status, COALESCE(last_generated_at, now()) AS created_at, COALESCE(last_generated_at, now()) AS updated_at FROM summary_slots WHERE company_id = $1 ORDER BY last_generated_at DESC NULLS LAST",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<SummaryRow>> {
        self.list(company_id).await
    }
}
