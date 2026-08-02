//! tool_access 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolAccess {
    pub id: Uuid,
    pub company_id: Uuid,
    pub tool_name: String,
    pub agent_id: Option<Uuid>,
    pub policy: serde_json::Value,
    pub updated_at: Timestamp,
}

pub struct ToolRepo<'a> { pub db: &'a Db }

impl<'a> ToolRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<ToolAccess>> {
        sqlx::query_as::<_, ToolAccess>(
            "SELECT id, company_id, tool_name, agent_id, policy, updated_at FROM tool_access WHERE company_id = $1 ORDER BY tool_name ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
