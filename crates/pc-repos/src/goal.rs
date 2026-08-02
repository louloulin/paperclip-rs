//! goals 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Goal {
    pub id: Uuid,
    pub company_id: Uuid,
    pub level: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct GoalRepo<'a> { pub db: &'a Db }

impl<'a> GoalRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Goal>> {
        sqlx::query_as::<_, Goal>(
            "SELECT id, company_id, level, title, description, status, created_at, updated_at FROM goals WHERE company_id = $1 ORDER BY created_at ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
