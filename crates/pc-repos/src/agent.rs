//! agents 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Agent {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub role: String,
    pub status: String,
    pub adapter_type: String,
    pub adapter_config_json: serde_json::Value,
    pub runtime_config: serde_json::Value,
    pub last_heartbeat_at: Option<Timestamp>,
    pub monitor_next_check_at: Option<Timestamp>,
    pub max_concurrent_runs: i32,
    pub last_error: Option<String>,
    pub paused_reason: Option<String>,
    pub paused_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const SELECT: &str = "id, company_id, name, role, status, adapter_type, adapter_config_json, runtime_config, last_heartbeat_at, monitor_next_check_at, max_concurrent_runs, last_error, paused_reason, paused_at, created_at, updated_at";

pub struct AgentRepo<'a> { pub db: &'a Db }

impl<'a> AgentRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<Agent>> {
        let sql = format!("SELECT {SELECT} FROM agents WHERE company_id = $1 ORDER BY created_at ASC");
        Ok(sqlx::query_as::<_, Agent>(&sql).bind(company_id).fetch_all(self.db.pool()).await?)
    }
    pub async fn find(&self, id: Uuid) -> RepoResult<Option<Agent>> {
        let sql = format!("SELECT {SELECT} FROM agents WHERE id = $1");
        Ok(sqlx::query_as::<_, Agent>(&sql).bind(id).fetch_optional(self.db.pool()).await?)
    }
    pub async fn create(&self, company_id: Uuid, name: &str, role: &str, adapter_type: &str, adapter_config: serde_json::Value) -> RepoResult<Agent> {
        if name.trim().is_empty() {
            return Err(RepoError::Invalid("agent name cannot be empty".into()));
        }
        let sql = format!("INSERT INTO agents (company_id, name, role, status, adapter_type, adapter_config_json) VALUES ($1, $2, $3, 'active', $4, $5) RETURNING {SELECT}");
        Ok(sqlx::query_as::<_, Agent>(&sql).bind(company_id).bind(name).bind(role).bind(adapter_type).bind(adapter_config).fetch_one(self.db.pool()).await?)
    }
}
