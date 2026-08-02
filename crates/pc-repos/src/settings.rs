//! instance_settings 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceSetting {
    pub key: String,
    pub value: serde_json::Value,
    pub updated_at: Timestamp,
}

pub struct SettingsRepo<'a> { pub db: &'a Db }

impl<'a> SettingsRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn get(&self, key: &str) -> sqlx::Result<Option<InstanceSetting>> {
        sqlx::query_as::<_, InstanceSetting>(
            "SELECT key, value, updated_at FROM instance_settings WHERE key = $1",
        ).bind(key).fetch_optional(self.db.pool()).await
    }
}
