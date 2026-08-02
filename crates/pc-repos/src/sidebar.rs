//! sidebar preferences 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidebarPreference {
    pub user_id: String,
    pub updated_at: Timestamp,
}

pub struct SidebarRepo<'a> { pub db: &'a Db }

impl<'a> SidebarRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn get(&self, user_id: &str) -> sqlx::Result<Option<SidebarPreference>> {
        sqlx::query_as::<_, SidebarPreference>(
            "SELECT user_id, updated_at FROM company_user_sidebar_preferences WHERE user_id = $1",
        ).bind(user_id).fetch_optional(self.db.pool()).await
    }
}
