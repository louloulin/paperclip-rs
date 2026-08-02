//! routines 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Routine {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub schedule: String,
    pub payload: serde_json::Value,
    pub enabled: bool,
    pub last_run_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct RoutineRepo<'a> { pub db: &'a Db }

impl<'a> RoutineRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Routine>> {
        sqlx::query_as::<_, Routine>(
            "SELECT id, company_id, name, schedule, payload, enabled, last_run_at, created_at, updated_at FROM routines WHERE company_id = $1 ORDER BY created_at ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
