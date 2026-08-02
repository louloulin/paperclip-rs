//! goal 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct GoalRepo<'a> { pub db: &'a Db }

impl<'a> GoalRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<GoalRow>> {
        let sql = format!("SELECT id, company_id, title AS name, status, created_at, updated_at FROM goals WHERE company_id = $1 ORDER BY created_at DESC");
        sqlx::query_as::<_, GoalRow>(&sql).bind(company_id).fetch_all(self.db.pool()).await
    }
}
