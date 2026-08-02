//! plugins 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct PluginRepo<'a> { pub db: &'a Db }

impl<'a> PluginRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list(&self) -> sqlx::Result<Vec<PluginRow>> {
        sqlx::query_as::<_, PluginRow>(
            "SELECT id, '00000000-0000-0000-0000-000000000000'::uuid AS company_id, '' AS name, '' AS status, updated_at AS created_at, updated_at FROM plugins ORDER BY updated_at DESC",
        ).fetch_all(self.db.pool()).await
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<PluginRow>> {
        sqlx::query_as::<_, PluginRow>(
            "SELECT id, '00000000-0000-0000-0000-000000000000'::uuid AS company_id, '' AS name, '' AS status, updated_at AS created_at, updated_at FROM plugins ORDER BY updated_at DESC",
        ).fetch_all(self.db.pool()).await
    }
}
