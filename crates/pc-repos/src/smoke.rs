//! smoke_lab 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmokeRun {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

pub struct SmokeRepo<'a> { pub db: &'a Db }

impl<'a> SmokeRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list(&self, company_id: Uuid) -> sqlx::Result<Vec<SmokeRun>> {
        sqlx::query_as::<_, SmokeRun>(
            "SELECT id, company_id, name, status, started_at, finished_at FROM smoke_lab WHERE company_id = $1 ORDER BY started_at DESC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
