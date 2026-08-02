//! activity_log 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityEntry {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_user_id: Option<String>,
    pub actor_agent_id: Option<Uuid>,
    pub kind: String,
    pub summary: String,
    pub payload: serde_json::Value,
    pub created_at: Timestamp,
}

pub struct ActivityRepo<'a> { pub db: &'a Db }

impl<'a> ActivityRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_recent(&self, company_id: Uuid) -> sqlx::Result<Vec<ActivityEntry>> {
        sqlx::query_as::<_, ActivityEntry>(
            "SELECT id, company_id, actor_user_id, actor_agent_id, kind, summary, payload, created_at FROM activity_log WHERE company_id = $1 ORDER BY created_at DESC LIMIT 200",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
