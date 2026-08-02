//! activity_log 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct ActivityRepo<'a> { pub db: &'a Db }

impl<'a> ActivityRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_recent(&self, company_id: Uuid) -> sqlx::Result<Vec<ActivityRow>> {
        sqlx::query_as::<_, ActivityRow>(
            "SELECT id, company_id, actor_type AS name, '' AS status, created_at, created_at AS updated_at FROM activity_log WHERE company_id = $1 ORDER BY created_at DESC LIMIT 200",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<ActivityRow>> {
        self.list_recent(company_id).await
    }
}
