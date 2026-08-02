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
    pub default_environment_id: Option<Uuid>,
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

    pub async fn get(&self, _key: &str) -> sqlx::Result<InstanceSetting> {
        sqlx::query_as(
            "INSERT INTO instance_settings (singleton_key) VALUES ('default') \
             ON CONFLICT (singleton_key) DO UPDATE SET singleton_key=EXCLUDED.singleton_key \
             RETURNING id, singleton_key, default_environment_id, general, experimental, created_at, updated_at",
        )
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn patch(
        &self,
        default_environment_id: Option<Uuid>,
        general: Option<serde_json::Value>,
        experimental: Option<serde_json::Value>,
    ) -> sqlx::Result<InstanceSetting> {
        self.get("default").await?;
        sqlx::query_as(
            "UPDATE instance_settings SET default_environment_id=COALESCE($1,default_environment_id), \
             general=general || COALESCE($2,'{}'::jsonb), experimental=experimental || COALESCE($3,'{}'::jsonb), \
             updated_at=now() WHERE singleton_key='default' RETURNING id, singleton_key, default_environment_id, \
             general, experimental, created_at, updated_at",
        )
        .bind(default_environment_id)
        .bind(general)
        .bind(experimental)
        .fetch_one(self.db.pool())
        .await
    }
}
