//! plugins 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plugin {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub manifest: serde_json::Value,
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct PluginRepo<'a> { pub db: &'a Db }

impl<'a> PluginRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Plugin>> {
        sqlx::query_as::<_, Plugin>(
            "SELECT id, company_id, name, manifest, enabled, created_at, updated_at FROM plugins WHERE company_id = $1 ORDER BY created_at ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
