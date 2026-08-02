//! `agent` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub role: String,
    pub title: Option<String>,
    pub icon: Option<String>,
    pub status: String,
    pub reports_to: Option<Uuid>,
    pub capabilities: Option<String>,
    pub adapter_type: String,
    pub adapter_config: serde_json::Value,
    pub runtime_config: serde_json::Value,
    pub default_environment_id: Option<Uuid>,
    pub budget_monthly_cents: i32,
    pub spent_monthly_cents: i32,
    pub pause_reason: Option<String>,
    pub paused_at: Option<Timestamp>,
    pub error_reason: Option<String>,
    pub permissions: serde_json::Value,
    pub last_heartbeat_at: Option<Timestamp>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct AgentRepo<'a> {
    pub db: &'a Db,
}

impl<'a> AgentRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<AgentRow>> {
        sqlx::query_as::<_, AgentRow>(
            "SELECT id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                    adapter_type, adapter_config, runtime_config, default_environment_id, \
                    budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                    error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at \
             FROM agents WHERE company_id = $1 ORDER BY created_at DESC",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<AgentRow>> {
        sqlx::query_as::<_, AgentRow>(
            "SELECT id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                    adapter_type, adapter_config, runtime_config, default_environment_id, \
                    budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                    error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at \
             FROM agents WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        name: &str,
        role: &str,
        title: Option<&str>,
        adapter_type: &str,
        adapter_config: serde_json::Value,
    ) -> sqlx::Result<AgentRow> {
        sqlx::query_as::<_, AgentRow>(
            "INSERT INTO agents (company_id, name, role, title, adapter_type, adapter_config) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(company_id).bind(name).bind(role).bind(title)
        .bind(adapter_type).bind(adapter_config)
        .fetch_one(self.db.pool()).await
    }

    pub async fn update(
        &self,
        id: Uuid,
        name: Option<&str>,
        role: Option<&str>,
        title: Option<&str>,
        status: Option<&str>,
    ) -> sqlx::Result<Option<AgentRow>> {
        sqlx::query_as::<_, AgentRow>(
            "UPDATE agents SET \
                name=COALESCE($2,name), role=COALESCE($3,role), title=COALESCE($4,title), \
                status=COALESCE($5,status), updated_at=now() \
             WHERE id=$1 \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id).bind(name).bind(role).bind(title).bind(status)
        .fetch_optional(self.db.pool()).await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM agents WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
