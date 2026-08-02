//! routine 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct RoutineRepo<'a> { pub db: &'a Db }

impl<'a> RoutineRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<RoutineRow>> {
        let sql = format!("SELECT id, company_id, 'routine' AS name, '' AS status, created_at, updated_at FROM routines WHERE company_id = $1 ORDER BY created_at DESC");
        sqlx::query_as::<_, RoutineRow>(&sql).bind(company_id).fetch_all(self.db.pool()).await
    }
}
