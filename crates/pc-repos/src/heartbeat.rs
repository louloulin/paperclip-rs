//! heartbeat_runs 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct HeartbeatRepo<'a> { pub db: &'a Db }

impl<'a> HeartbeatRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_for_agent(&self, agent_id: Uuid) -> sqlx::Result<Vec<HeartbeatRow>> {
        sqlx::query_as::<_, HeartbeatRow>(
            "SELECT id, company_id, '' AS name, status, started_at AS created_at, COALESCE(finished_at, started_at) AS updated_at FROM heartbeat_runs WHERE agent_id = $1 ORDER BY started_at DESC",
        ).bind(agent_id).fetch_all(self.db.pool()).await
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<HeartbeatRow>> {
        sqlx::query_as::<_, HeartbeatRow>(
            "SELECT id, company_id, '' AS name, status, started_at AS created_at, COALESCE(finished_at, started_at) AS updated_at FROM heartbeat_runs WHERE company_id = $1 ORDER BY started_at DESC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
