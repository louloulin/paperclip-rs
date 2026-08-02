//! heartbeat_runs / heartbeat_run_events 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatRun {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub status: String,
    pub invocation_source: String,
    pub trigger_detail: Option<String>,
    pub wakeup_request_id: Option<Uuid>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub usage_json: Option<serde_json::Value>,
    pub result_json: Option<serde_json::Value>,
    pub session_id_before: Option<String>,
    pub session_id_after: Option<String>,
    pub log_store: Option<String>,
    pub log_ref: Option<String>,
    pub log_bytes: Option<i64>,
    pub log_sha256: Option<String>,
    pub log_compressed: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct HeartbeatRepo<'a> { pub db: &'a Db }

impl<'a> HeartbeatRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_for_agent(&self, agent_id: Uuid) -> sqlx::Result<Vec<HeartbeatRun>> {
        sqlx::query_as::<_, HeartbeatRun>(
            "SELECT id, company_id, agent_id, status, invocation_source, trigger_detail, wakeup_request_id, started_at, finished_at, exit_code, signal, usage_json, result_json, session_id_before, session_id_after, log_store, log_ref, log_bytes, log_sha256, log_compressed, created_at, updated_at FROM heartbeat_runs WHERE agent_id = $1 ORDER BY started_at DESC",
        ).bind(agent_id).fetch_all(self.db.pool()).await
    }
}
