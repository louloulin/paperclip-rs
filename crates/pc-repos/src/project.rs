//! projects 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub color: String,
    pub archived: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct ProjectRepo<'a> { pub db: &'a Db }

impl<'a> ProjectRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Project>> {
        sqlx::query_as::<_, Project>(
            "SELECT id, company_id, name, color, archived, created_at, updated_at FROM projects WHERE company_id = $1 ORDER BY created_at ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
