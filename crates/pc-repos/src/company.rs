//! company 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompanyRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct CompanyRepo<'a> { pub db: &'a Db }

impl<'a> CompanyRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list(&self) -> sqlx::Result<Vec<CompanyRow>> {
        let sql = format!("SELECT id, '00000000-0000-0000-0000-000000000000'::uuid AS company_id, name, status, created_at, updated_at FROM companies ORDER BY created_at DESC");
        sqlx::query_as::<_, CompanyRow>(&sql).fetch_all(self.db.pool()).await
    }
}
