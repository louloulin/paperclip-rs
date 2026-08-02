//! summary_slots 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummarySlot {
    pub id: Uuid,
    pub company_id: Uuid,
    pub slot: String,
    pub payload: serde_json::Value,
    pub refreshed_at: Timestamp,
}

pub struct SummaryRepo<'a> { pub db: &'a Db }

impl<'a> SummaryRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list(&self, company_id: Uuid) -> sqlx::Result<Vec<SummarySlot>> {
        sqlx::query_as::<_, SummarySlot>(
            "SELECT id, company_id, slot, payload, refreshed_at FROM summary_slots WHERE company_id = $1 ORDER BY refreshed_at DESC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
