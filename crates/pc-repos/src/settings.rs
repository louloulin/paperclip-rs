//! `instance_settings` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceSetting {
    pub id: Uuid,
    pub singleton_key: String,
    pub general: serde_json::Value,
    pub experimental: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct SettingsRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SettingsRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn get(&self, _key: &str) -> sqlx::Result<Option<InstanceSetting>> {
        sqlx::query_as::<_, InstanceSetting>(
            "SELECT id, singleton_key, general, experimental, created_at, updated_at FROM instance_settings LIMIT 1",
        ).fetch_optional(self.db.pool()).await
    }
}
