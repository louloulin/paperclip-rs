//! `agent` 域。

use serde::{Deserialize, Serialize};
use sqlx::{types::Json, FromRow};
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;
use crate::approval::ApprovalRow;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone)]
pub struct CreateAgentRecord {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub role: String,
    pub title: Option<String>,
    pub icon: Option<String>,
    pub reports_to: Option<Uuid>,
    pub capabilities: Option<String>,
    pub adapter_type: String,
    pub adapter_config: serde_json::Value,
    pub runtime_config: serde_json::Value,
    pub default_environment_id: Option<Uuid>,
    pub budget_monthly_cents: i32,
    pub permissions: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct AgentConfigRecord {
    pub name: String,
    pub role: String,
    pub title: Option<String>,
    pub icon: Option<String>,
    pub reports_to: Option<Uuid>,
    pub capabilities: Option<String>,
    pub adapter_type: String,
    pub adapter_config: serde_json::Value,
    pub runtime_config: serde_json::Value,
    pub default_environment_id: Option<Uuid>,
    pub budget_monthly_cents: i32,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct NewAgentConfigRevision {
    pub company_id: Uuid,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub source: String,
    pub rolled_back_from_revision_id: Option<Uuid>,
    pub changed_keys: Vec<String>,
    pub before_config: serde_json::Value,
    pub after_config: serde_json::Value,
}

#[derive(Debug, Clone, FromRow)]
pub struct AgentConfigRevisionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub source: String,
    pub rolled_back_from_revision_id: Option<Uuid>,
    pub changed_keys: Json<Vec<String>>,
    pub before_config: serde_json::Value,
    pub after_config: serde_json::Value,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeStateRow {
    pub agent_id: Uuid,
    pub company_id: Uuid,
    pub adapter_type: String,
    pub session_id: Option<String>,
    pub state_json: serde_json::Value,
    pub last_run_id: Option<Uuid>,
    pub last_run_status: Option<String>,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cached_input_tokens: i64,
    pub total_cost_cents: i64,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskSessionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub adapter_type: String,
    pub task_key: String,
    pub session_params_json: Option<serde_json::Value>,
    pub session_display_id: Option<String>,
    pub last_run_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow)]
pub struct AgentApiKeyRow {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub key_hash: String,
    pub responsible_user_id: Option<String>,
    pub scope_config: Option<serde_json::Value>,
    pub last_used_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct CreateAgentApiKeyRecord {
    pub agent_id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub key_hash: String,
    pub responsible_user_id: Option<String>,
    pub scope_config: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct NewHireApproval {
    pub requested_by_agent_id: Option<Uuid>,
    pub requested_by_user_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct AgentHireRecord {
    pub agent: AgentRow,
    pub approval: Option<ApprovalRow>,
}

const RUNTIME_STATE_COLS: &str = "agent_id, company_id, adapter_type, session_id, state_json, \
    last_run_id, last_run_status, total_input_tokens, total_output_tokens, \
    total_cached_input_tokens, total_cost_cents, last_error, created_at, updated_at";

const TASK_SESSION_COLS: &str = "id, company_id, agent_id, adapter_type, task_key, \
    session_params_json, session_display_id, last_run_id, last_error, created_at, updated_at";

const API_KEY_COLS: &str = "id, agent_id, company_id, name, key_hash, responsible_user_id, \
    scope_config, last_used_at, revoked_at, created_at";

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

    pub async fn list_all(&self) -> sqlx::Result<Vec<AgentRow>> {
        sqlx::query_as::<_, AgentRow>(
            "SELECT id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                    adapter_type, adapter_config, runtime_config, default_environment_id, \
                    budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                    error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at \
             FROM agents ORDER BY created_at DESC",
        )
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

    pub async fn create_full(&self, input: CreateAgentRecord) -> sqlx::Result<AgentRow> {
        sqlx::query_as::<_, AgentRow>(
            "INSERT INTO agents (id, company_id, name, role, title, icon, status, reports_to, \
                capabilities, adapter_type, adapter_config, runtime_config, default_environment_id, \
                budget_monthly_cents, permissions, metadata) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(input.id)
        .bind(input.company_id)
        .bind(input.name)
        .bind(input.role)
        .bind(input.title)
        .bind(input.icon)
        .bind(input.status)
        .bind(input.reports_to)
        .bind(input.capabilities)
        .bind(input.adapter_type)
        .bind(input.adapter_config)
        .bind(input.runtime_config)
        .bind(input.default_environment_id)
        .bind(input.budget_monthly_cents)
        .bind(input.permissions)
        .bind(input.metadata)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn create_hire(
        &self,
        input: CreateAgentRecord,
        approval: Option<NewHireApproval>,
    ) -> sqlx::Result<AgentHireRecord> {
        let mut transaction = self.db.pool().begin().await?;
        let agent = sqlx::query_as::<_, AgentRow>(
            "INSERT INTO agents (id, company_id, name, role, title, icon, status, reports_to, \
                capabilities, adapter_type, adapter_config, runtime_config, default_environment_id, \
                budget_monthly_cents, permissions, metadata) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(input.id)
        .bind(input.company_id)
        .bind(input.name)
        .bind(input.role)
        .bind(input.title)
        .bind(input.icon)
        .bind(input.status)
        .bind(input.reports_to)
        .bind(input.capabilities)
        .bind(input.adapter_type)
        .bind(input.adapter_config)
        .bind(input.runtime_config)
        .bind(input.default_environment_id)
        .bind(input.budget_monthly_cents)
        .bind(input.permissions)
        .bind(input.metadata)
        .fetch_one(&mut *transaction)
        .await?;
        let approval = if let Some(approval) = approval {
            Some(
                sqlx::query_as::<_, ApprovalRow>(
                    "INSERT INTO approvals (company_id, type, requested_by_agent_id, \
                        requested_by_user_id, status, payload) \
                     VALUES ($1, 'hire_agent', $2, $3, 'pending', $4) \
                     RETURNING id, company_id, type AS approval_type, requested_by_agent_id, \
                        requested_by_user_id, status, payload, decision_note, decided_by_user_id, \
                        decided_at, created_at, updated_at",
                )
                .bind(agent.company_id)
                .bind(approval.requested_by_agent_id)
                .bind(approval.requested_by_user_id)
                .bind(approval.payload)
                .fetch_one(&mut *transaction)
                .await?,
            )
        } else {
            None
        };
        transaction.commit().await?;
        Ok(AgentHireRecord { agent, approval })
    }

    pub async fn replace_config_with_revision(
        &self,
        id: Uuid,
        config: AgentConfigRecord,
        revision: Option<NewAgentConfigRevision>,
    ) -> sqlx::Result<Option<AgentRow>> {
        let mut transaction = self.db.pool().begin().await?;
        let row = sqlx::query_as::<_, AgentRow>(
            "UPDATE agents SET \
                name=$2, role=$3, title=$4, icon=$5, reports_to=$6, capabilities=$7, \
                adapter_type=$8, adapter_config=$9, runtime_config=$10, \
                default_environment_id=$11, budget_monthly_cents=$12, metadata=$13, updated_at=now() \
             WHERE id=$1 \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .bind(config.name)
        .bind(config.role)
        .bind(config.title)
        .bind(config.icon)
        .bind(config.reports_to)
        .bind(config.capabilities)
        .bind(config.adapter_type)
        .bind(config.adapter_config)
        .bind(config.runtime_config)
        .bind(config.default_environment_id)
        .bind(config.budget_monthly_cents)
        .bind(config.metadata)
        .fetch_optional(&mut *transaction)
        .await?;

        if row.is_some() {
            if let Some(revision) = revision {
                sqlx::query(
                    "INSERT INTO agent_config_revisions (company_id, agent_id, created_by_agent_id, \
                        created_by_user_id, source, rolled_back_from_revision_id, changed_keys, \
                        before_config, after_config) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                )
                .bind(revision.company_id)
                .bind(id)
                .bind(revision.created_by_agent_id)
                .bind(revision.created_by_user_id)
                .bind(revision.source)
                .bind(revision.rolled_back_from_revision_id)
                .bind(Json(revision.changed_keys))
                .bind(revision.before_config)
                .bind(revision.after_config)
                .execute(&mut *transaction)
                .await?;
            }
        }
        transaction.commit().await?;
        Ok(row)
    }

    pub async fn list_config_revisions(
        &self,
        agent_id: Uuid,
    ) -> sqlx::Result<Vec<AgentConfigRevisionRow>> {
        sqlx::query_as(
            "SELECT id, company_id, agent_id, created_by_agent_id, created_by_user_id, source, \
                    rolled_back_from_revision_id, changed_keys, before_config, after_config, created_at \
             FROM agent_config_revisions WHERE agent_id=$1 ORDER BY created_at DESC, id DESC",
        )
        .bind(agent_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_config_revision(
        &self,
        agent_id: Uuid,
        revision_id: Uuid,
    ) -> sqlx::Result<Option<AgentConfigRevisionRow>> {
        sqlx::query_as(
            "SELECT id, company_id, agent_id, created_by_agent_id, created_by_user_id, source, \
                    rolled_back_from_revision_id, changed_keys, before_config, after_config, created_at \
             FROM agent_config_revisions WHERE agent_id=$1 AND id=$2",
        )
        .bind(agent_id)
        .bind(revision_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn ensure_runtime_state(
        &self,
        agent: &AgentRow,
    ) -> sqlx::Result<AgentRuntimeStateRow> {
        sqlx::query(
            "INSERT INTO agent_runtime_state (agent_id, company_id, adapter_type) \
             VALUES ($1,$2,$3) ON CONFLICT (agent_id) DO NOTHING",
        )
        .bind(agent.id)
        .bind(agent.company_id)
        .bind(&agent.adapter_type)
        .execute(self.db.pool())
        .await?;
        self.get_runtime_state(agent.id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)
    }

    pub async fn get_runtime_state(
        &self,
        agent_id: Uuid,
    ) -> sqlx::Result<Option<AgentRuntimeStateRow>> {
        let sql = format!(
            "SELECT {RUNTIME_STATE_COLS} FROM agent_runtime_state WHERE agent_id=$1"
        );
        sqlx::query_as(&sql)
            .bind(agent_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn list_task_sessions(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
    ) -> sqlx::Result<Vec<AgentTaskSessionRow>> {
        let sql = format!(
            "SELECT {TASK_SESSION_COLS} FROM agent_task_sessions \
             WHERE company_id=$1 AND agent_id=$2 ORDER BY updated_at DESC, created_at DESC"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(agent_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn latest_task_session(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
    ) -> sqlx::Result<Option<AgentTaskSessionRow>> {
        let sql = format!(
            "SELECT {TASK_SESSION_COLS} FROM agent_task_sessions \
             WHERE company_id=$1 AND agent_id=$2 ORDER BY updated_at DESC, created_at DESC LIMIT 1"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(agent_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn reset_runtime_session(
        &self,
        agent: &AgentRow,
        task_key: Option<&str>,
    ) -> sqlx::Result<(AgentRuntimeStateRow, u64)> {
        let mut transaction = self.db.pool().begin().await?;
        sqlx::query(
            "INSERT INTO agent_runtime_state (agent_id, company_id, adapter_type) \
             VALUES ($1,$2,$3) ON CONFLICT (agent_id) DO NOTHING",
        )
        .bind(agent.id)
        .bind(agent.company_id)
        .bind(&agent.adapter_type)
        .execute(&mut *transaction)
        .await?;

        let deleted = if let Some(task_key) = task_key {
            sqlx::query(
                "DELETE FROM agent_task_sessions WHERE company_id=$1 AND agent_id=$2 \
                 AND adapter_type=$3 AND task_key=$4",
            )
            .bind(agent.company_id)
            .bind(agent.id)
            .bind(&agent.adapter_type)
            .bind(task_key)
            .execute(&mut *transaction)
            .await?
        } else {
            sqlx::query(
                "DELETE FROM agent_task_sessions WHERE company_id=$1 AND agent_id=$2",
            )
            .bind(agent.company_id)
            .bind(agent.id)
            .execute(&mut *transaction)
            .await?
        };

        let sql = if task_key.is_some() {
            format!(
                "UPDATE agent_runtime_state SET session_id=NULL, last_error=NULL, updated_at=now() \
                 WHERE agent_id=$1 RETURNING {RUNTIME_STATE_COLS}"
            )
        } else {
            format!(
                "UPDATE agent_runtime_state SET session_id=NULL, state_json='{{}}'::jsonb, \
                    last_error=NULL, updated_at=now() WHERE agent_id=$1 \
                 RETURNING {RUNTIME_STATE_COLS}"
            )
        };
        let state = sqlx::query_as(&sql)
            .bind(agent.id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok((state, deleted.rows_affected()))
    }

    pub async fn pause(&self, id: Uuid, reason: &str) -> sqlx::Result<Option<AgentRow>> {
        sqlx::query_as(
            "UPDATE agents SET status='paused', pause_reason=$2, paused_at=now(), error_reason=NULL, \
                updated_at=now() WHERE id=$1 \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .bind(reason)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn resume(&self, id: Uuid) -> sqlx::Result<Option<AgentRow>> {
        sqlx::query_as(
            "UPDATE agents SET status='idle', pause_reason=NULL, paused_at=NULL, error_reason=NULL, \
                updated_at=now() WHERE id=$1 \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn clear_error(&self, id: Uuid) -> sqlx::Result<Option<AgentRow>> {
        sqlx::query_as(
            "UPDATE agents SET status='idle', pause_reason=NULL, paused_at=NULL, error_reason=NULL, \
                updated_at=now() WHERE id=$1 AND status='error' \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn terminate(&self, id: Uuid) -> sqlx::Result<Option<AgentRow>> {
        let mut transaction = self.db.pool().begin().await?;
        let row = sqlx::query_as(
            "UPDATE agents SET status='terminated', pause_reason=NULL, paused_at=NULL, \
                error_reason=NULL, updated_at=now() WHERE id=$1 \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?;
        if row.is_some() {
            sqlx::query(
                "UPDATE agent_api_keys SET revoked_at=COALESCE(revoked_at, now()) WHERE agent_id=$1",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(row)
    }

    pub async fn update_permissions(
        &self,
        id: Uuid,
        permissions: serde_json::Value,
    ) -> sqlx::Result<Option<AgentRow>> {
        sqlx::query_as(
            "UPDATE agents SET permissions=$2, updated_at=now() WHERE id=$1 \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .bind(permissions)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn update_permissions_and_access(
        &self,
        id: Uuid,
        permissions: serde_json::Value,
        can_assign_tasks: bool,
        granted_by_user_id: Option<&str>,
    ) -> sqlx::Result<Option<AgentRow>> {
        let mut transaction = self.db.pool().begin().await?;
        let row = sqlx::query_as::<_, AgentRow>(
            "UPDATE agents SET permissions=$2, updated_at=now() WHERE id=$1 \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .bind(permissions)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(agent) = &row {
            let principal_id = agent.id.to_string();
            sqlx::query(
                "INSERT INTO company_memberships \
                    (company_id, principal_type, principal_id, status, membership_role) \
                 VALUES ($1, 'agent', $2, 'active', 'member') \
                 ON CONFLICT (company_id, principal_type, principal_id) DO UPDATE SET \
                    status='active', membership_role=COALESCE(company_memberships.membership_role, 'member'), \
                    updated_at=now()",
            )
            .bind(agent.company_id)
            .bind(&principal_id)
            .execute(&mut *transaction)
            .await?;
            if can_assign_tasks {
                sqlx::query(
                    "INSERT INTO principal_permission_grants \
                        (company_id, principal_type, principal_id, permission_key, scope, granted_by_user_id) \
                     VALUES ($1, 'agent', $2, 'tasks:assign', NULL, $3) \
                     ON CONFLICT (company_id, principal_type, principal_id, permission_key) \
                     DO UPDATE SET scope=NULL, granted_by_user_id=EXCLUDED.granted_by_user_id, updated_at=now()",
                )
                .bind(agent.company_id)
                .bind(&principal_id)
                .bind(granted_by_user_id)
                .execute(&mut *transaction)
                .await?;
            } else {
                sqlx::query(
                    "DELETE FROM principal_permission_grants WHERE company_id=$1 \
                     AND principal_type='agent' AND principal_id=$2 AND permission_key='tasks:assign'",
                )
                .bind(agent.company_id)
                .bind(&principal_id)
                .execute(&mut *transaction)
                .await?;
            }
        }
        transaction.commit().await?;
        Ok(row)
    }

    pub async fn approve_pending(&self, id: Uuid) -> sqlx::Result<Option<AgentRow>> {
        sqlx::query_as(
            "UPDATE agents SET status='idle', updated_at=now() \
             WHERE id=$1 AND status='pending_approval' \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_api_key(
        &self,
        input: CreateAgentApiKeyRecord,
    ) -> sqlx::Result<AgentApiKeyRow> {
        let sql = format!(
            "INSERT INTO agent_api_keys (agent_id, company_id, name, key_hash, responsible_user_id, scope_config) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING {API_KEY_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(input.agent_id)
            .bind(input.company_id)
            .bind(input.name)
            .bind(input.key_hash)
            .bind(input.responsible_user_id)
            .bind(input.scope_config)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn list_api_keys(&self, agent_id: Uuid) -> sqlx::Result<Vec<AgentApiKeyRow>> {
        let sql = format!(
            "SELECT {API_KEY_COLS} FROM agent_api_keys WHERE agent_id=$1 ORDER BY created_at DESC"
        );
        sqlx::query_as(&sql)
            .bind(agent_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn revoke_api_key(
        &self,
        agent_id: Uuid,
        key_id: Uuid,
    ) -> sqlx::Result<Option<AgentApiKeyRow>> {
        let sql = format!(
            "UPDATE agent_api_keys SET revoked_at=COALESCE(revoked_at, now()) \
             WHERE id=$1 AND agent_id=$2 RETURNING {API_KEY_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(key_id)
            .bind(agent_id)
            .fetch_optional(self.db.pool())
            .await
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
