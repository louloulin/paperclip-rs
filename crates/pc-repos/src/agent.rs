//! `agent` 域。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{types::Json, FromRow};
use uuid::Uuid;

use pc_core::Timestamp;

use crate::approval::ApprovalRow;
use crate::Db;

const AGENT_COLUMNS: &str = "id, company_id, name, role, title, icon, status, reports_to, capabilities, \
adapter_type, adapter_config, runtime_config, default_environment_id, budget_monthly_cents, \
spent_monthly_cents, pause_reason, paused_at, error_reason, permissions, last_heartbeat_at, \
metadata, created_at, updated_at";

/// 组织架构视图用 agent 投影：仅 6 个核心字段。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgChartAgentRow {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub reports_to: Option<Uuid>,
    pub status: String,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatInvocationSource {
    Timer,
    Assignment,
    OnDemand,
    Automation,
}

impl HeartbeatInvocationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::Assignment => "assignment",
            Self::OnDemand => "on_demand",
            Self::Automation => "automation",
        }
    }
}

impl std::str::FromStr for HeartbeatInvocationSource {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "timer" => Ok(Self::Timer),
            "assignment" => Ok(Self::Assignment),
            "on_demand" => Ok(Self::OnDemand),
            "automation" => Ok(Self::Automation),
            _ => Err("invalid heartbeat invocation source"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupTriggerDetail {
    Manual,
    Ping,
    Callback,
    System,
}

impl WakeupTriggerDetail {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Ping => "ping",
            Self::Callback => "callback",
            Self::System => "system",
        }
    }
}

impl std::str::FromStr for WakeupTriggerDetail {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "manual" => Ok(Self::Manual),
            "ping" => Ok(Self::Ping),
            "callback" => Ok(Self::Callback),
            "system" => Ok(Self::System),
            _ => Err("invalid wakeup trigger detail"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupRequestStatus {
    Queued,
    DeferredIssueExecution,
    Claimed,
    Coalesced,
    Skipped,
    Completed,
    Failed,
    Cancelled,
}

impl WakeupRequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::DeferredIssueExecution => "deferred_issue_execution",
            Self::Claimed => "claimed",
            Self::Coalesced => "coalesced",
            Self::Skipped => "skipped",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Coalesced | Self::Skipped | Self::Completed | Self::Failed | Self::Cancelled
        )
    }

    pub fn can_transition_to(self, target: Self) -> bool {
        if self == target {
            return true;
        }
        match self {
            Self::Queued => matches!(
                target,
                Self::DeferredIssueExecution | Self::Claimed | Self::Skipped | Self::Cancelled
            ),
            Self::DeferredIssueExecution => matches!(
                target,
                Self::Queued | Self::Claimed | Self::Skipped | Self::Cancelled
            ),
            Self::Claimed => matches!(
                target,
                Self::Skipped | Self::Completed | Self::Failed | Self::Cancelled
            ),
            Self::Coalesced
            | Self::Skipped
            | Self::Completed
            | Self::Failed
            | Self::Cancelled => false,
        }
    }

    fn allowed_predecessors(self) -> &'static [&'static str] {
        match self {
            Self::Queued => &["queued", "deferred_issue_execution"],
            Self::DeferredIssueExecution => &["queued", "deferred_issue_execution"],
            Self::Claimed => &["queued", "deferred_issue_execution", "claimed"],
            Self::Coalesced => &["coalesced"],
            Self::Skipped => &["queued", "deferred_issue_execution", "claimed", "skipped"],
            Self::Completed => &["claimed", "completed"],
            Self::Failed => &["claimed", "failed"],
            Self::Cancelled => &[
                "queued",
                "deferred_issue_execution",
                "claimed",
                "cancelled",
            ],
        }
    }
}

impl std::str::FromStr for WakeupRequestStatus {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "deferred_issue_execution" => Ok(Self::DeferredIssueExecution),
            "claimed" => Ok(Self::Claimed),
            "coalesced" => Ok(Self::Coalesced),
            "skipped" => Ok(Self::Skipped),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err("invalid wakeup request status"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupActorType {
    User,
    Agent,
    System,
}

impl WakeupActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWakeupRequestRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub source: String,
    pub trigger_detail: Option<String>,
    pub reason: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub status: String,
    pub coalesced_count: i32,
    pub requested_by_actor_type: Option<String>,
    pub requested_by_actor_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub run_id: Option<Uuid>,
    pub requested_at: Timestamp,
    pub claimed_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl AgentWakeupRequestRow {
    pub fn wakeup_status(&self) -> Option<WakeupRequestStatus> {
        self.status.parse().ok()
    }

    pub fn invocation_source(&self) -> Option<HeartbeatInvocationSource> {
        self.source.parse().ok()
    }

    pub fn trigger(&self) -> Option<WakeupTriggerDetail> {
        self.trigger_detail
            .as_deref()
            .and_then(|value| value.parse().ok())
    }
}

#[derive(Debug, Clone)]
pub struct NewAgentWakeupRequest {
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub source: HeartbeatInvocationSource,
    pub trigger_detail: Option<WakeupTriggerDetail>,
    pub reason: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub status: WakeupRequestStatus,
    pub coalesced_count: i32,
    pub requested_by_actor_type: Option<WakeupActorType>,
    pub requested_by_actor_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub run_id: Option<Uuid>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentWakeupFilter {
    pub status: Option<WakeupRequestStatus>,
    pub run_id: Option<Uuid>,
    pub limit: Option<i64>,
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

const WAKEUP_COLS: &str = "id, company_id, agent_id, source, trigger_detail, reason, payload, \
                           status, coalesced_count, requested_by_actor_type, requested_by_actor_id, \
                           idempotency_key, run_id, requested_at, claimed_at, finished_at, error, \
                           created_at, updated_at";

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

    /// 公司组织架构用的最小列投影：仅返回 (id, name, role, title, reports_to, status)。
    /// 路由层 (`GET /api/companies/:id/org` + `/org.svg`) 用此构造节点 / 边 / SVG。
    pub async fn list_for_org_chart(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<OrgChartAgentRow>> {
        sqlx::query_as::<_, OrgChartAgentRow>(
            "SELECT id, name, role, title, reports_to, status \
             FROM agents WHERE company_id = $1 ORDER BY name",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// 公司内创建 agent 的精简路径（`POST /api/companies/:id/agents`）。
    /// 修复原路由 inline SQL 使用了不存在的 `adapter_kind` 列（实际是 `adapter_type`）。
    pub async fn create_simple(
        &self,
        company_id: Uuid,
        name: &str,
        role: &str,
    ) -> sqlx::Result<AgentRow> {
        sqlx::query_as::<_, AgentRow>(
            "INSERT INTO agents (company_id, name, role, status, adapter_type) \
             VALUES ($1, $2, $3, 'active', 'codex_local') \
             RETURNING id, company_id, name, role, title, icon, status, reports_to, capabilities, \
                       adapter_type, adapter_config, runtime_config, default_environment_id, \
                       budget_monthly_cents, spent_monthly_cents, pause_reason, paused_at, \
                       error_reason, permissions, last_heartbeat_at, metadata, created_at, updated_at",
        )
        .bind(company_id)
        .bind(name)
        .bind(role)
        .fetch_one(self.db.pool())
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

    /// Round 154: 列出最近 N 个 agent 的轻量投影（id, name, role）。
    /// tool-connections list_test_agents 用（schema 实际列 target_id 是 text，无法 join）。
    pub async fn list_recent_lightweight(
        &self,
        limit: i64,
    ) -> sqlx::Result<Vec<(Uuid, String, String)>> {
        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, name, role FROM agents ORDER BY name LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
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

    pub async fn claim_due_timer_heartbeat(
        &self,
        agent_id: Uuid,
        interval_seconds: i64,
    ) -> sqlx::Result<Option<AgentRow>> {
        let query = format!(
            "UPDATE agents SET last_heartbeat_at=now(), updated_at=now() \
             WHERE id=$1 AND status NOT IN ('paused','terminated') \
               AND (last_heartbeat_at <= now() - ($2 * interval '1 second') \
                    OR (last_heartbeat_at IS NULL AND created_at <= now() - ($2 * interval '1 second'))) \
             RETURNING {AGENT_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRow>(&query)
            .bind(agent_id)
            .bind(interval_seconds.clamp(1, 86_400))
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
        let sql = format!("SELECT {RUNTIME_STATE_COLS} FROM agent_runtime_state WHERE agent_id=$1");
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
            sqlx::query("DELETE FROM agent_task_sessions WHERE company_id=$1 AND agent_id=$2")
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

    /// Round 183: live_events auth -- by key_hash find agent_api_key (id, company_id).
    /// Only returns non-revoked (revoked_at IS NULL) keys.
    pub async fn find_api_key_id_company_by_hash(
        &self,
        key_hash: &str,
    ) -> sqlx::Result<Option<(Uuid, Uuid)>> {
        let row: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, company_id FROM agent_api_keys \
             WHERE key_hash = $1 AND revoked_at IS NULL",
        )
        .bind(key_hash)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
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

    pub async fn create_wakeup_request(
        &self,
        input: NewAgentWakeupRequest,
    ) -> sqlx::Result<AgentWakeupRequestRow> {
        let sql = format!(
            "INSERT INTO agent_wakeup_requests \
                (company_id, agent_id, source, trigger_detail, reason, payload, status, \
                 coalesced_count, requested_by_actor_type, requested_by_actor_id, idempotency_key, \
                 run_id, claimed_at, finished_at, error) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, \
                 CASE WHEN $7='claimed' THEN now() ELSE NULL END, \
                 CASE WHEN $7 IN ('coalesced','skipped','completed','failed','cancelled') \
                      THEN now() ELSE NULL END, $13) \
             RETURNING {WAKEUP_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(input.company_id)
            .bind(input.agent_id)
            .bind(input.source.as_str())
            .bind(input.trigger_detail.map(WakeupTriggerDetail::as_str))
            .bind(input.reason)
            .bind(input.payload)
            .bind(input.status.as_str())
            .bind(input.coalesced_count.max(0))
            .bind(input.requested_by_actor_type.map(WakeupActorType::as_str))
            .bind(input.requested_by_actor_id)
            .bind(input.idempotency_key)
            .bind(input.run_id)
            .bind(input.error)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn get_wakeup_request(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> sqlx::Result<Option<AgentWakeupRequestRow>> {
        let sql = format!(
            "SELECT {WAKEUP_COLS} FROM agent_wakeup_requests WHERE company_id=$1 AND id=$2"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }


    /// Round 174: 实例统计用 —— 统计某公司的 agent 数。
    pub async fn count_for_company(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM agents WHERE company_id=$1",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }

    /// Round 175: 取指定 agent 的 adapter_config（按公司隔离）。
    pub async fn get_adapter_config(
        &self,
        agent_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Option<serde_json::Value>> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT adapter_config FROM agents WHERE id = $1 AND company_id = $2",
        )
        .bind(agent_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Round 176: 统计某公司 paused 状态的 agent 数。
    pub async fn count_paused_for_company(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM agents WHERE company_id = $1 AND status = 'paused'",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }

    /// Round 176: 设置 agent 的 budget_monthly_cents，返回更新后的 (id, company_id, budget, spent) 元组。
    pub async fn set_budget(
        &self,
        agent_id: Uuid,
        budget_monthly_cents: i32,
    ) -> sqlx::Result<Option<(Uuid, Uuid, i32, i32)>> {
        let row: Option<(Uuid, Uuid, i32, i32)> = sqlx::query_as(
            "UPDATE agents SET budget_monthly_cents = $2, updated_at = now() \
             WHERE id = $1 RETURNING id, company_id, budget_monthly_cents, spent_monthly_cents",
        )
        .bind(agent_id)
        .bind(budget_monthly_cents)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    pub async fn list_wakeup_requests(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
        filter: &AgentWakeupFilter,
    ) -> sqlx::Result<Vec<AgentWakeupRequestRow>> {
        let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(format!(
            "SELECT {WAKEUP_COLS} FROM agent_wakeup_requests WHERE company_id="
        ));
        query.push_bind(company_id);
        query.push(" AND agent_id=").push_bind(agent_id);
        if let Some(status) = filter.status {
            query.push(" AND status=").push_bind(status.as_str());
        }
        if let Some(run_id) = filter.run_id {
            query.push(" AND run_id=").push_bind(run_id);
        }
        query
            .push(" ORDER BY requested_at DESC, created_at DESC LIMIT ")
            .push_bind(filter.limit.unwrap_or(200).clamp(1, 1_000));
        query
            .build_query_as::<AgentWakeupRequestRow>()
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn find_wakeup_by_idempotency_key(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
        idempotency_key: &str,
    ) -> sqlx::Result<Option<AgentWakeupRequestRow>> {
        let sql = format!(
            "SELECT {WAKEUP_COLS} FROM agent_wakeup_requests \
             WHERE company_id=$1 AND agent_id=$2 AND idempotency_key=$3 \
             ORDER BY requested_at DESC, created_at DESC LIMIT 1"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(agent_id)
            .bind(idempotency_key)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn transition_wakeup_request(
        &self,
        company_id: Uuid,
        id: Uuid,
        target: WakeupRequestStatus,
        run_id: Option<Uuid>,
        error: Option<&str>,
    ) -> sqlx::Result<Option<AgentWakeupRequestRow>> {
        let allowed_predecessors: Vec<String> = target
            .allowed_predecessors()
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        let sql = format!(
            "UPDATE agent_wakeup_requests SET \
                status=$3, run_id=COALESCE($4, run_id), \
                claimed_at=CASE WHEN $3='claimed' THEN COALESCE(claimed_at, now()) ELSE claimed_at END, \
                finished_at=CASE \
                    WHEN $3 IN ('coalesced','skipped','completed','failed','cancelled') \
                    THEN COALESCE(finished_at, now()) ELSE NULL END, \
                error=CASE WHEN $3 IN ('completed','coalesced') THEN NULL ELSE $5 END, \
                updated_at=now() \
             WHERE company_id=$1 AND id=$2 AND status=ANY($6::text[]) \
             RETURNING {WAKEUP_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(id)
            .bind(target.as_str())
            .bind(run_id)
            .bind(error)
            .bind(allowed_predecessors)
            .fetch_optional(self.db.pool())
            .await
    }

    /// Look up the active (non-terminal) wakeup request for an agent, if any.
    /// Mirrors the Node-side `findActiveWakeupRequest` used to coalesce
    /// repeated wakeups into the existing queued request.
    pub async fn find_active_wakeup_request(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
    ) -> sqlx::Result<Option<AgentWakeupRequestRow>> {
        let sql = format!(
            "SELECT {WAKEUP_COLS} FROM agent_wakeup_requests              WHERE company_id=$1 AND agent_id=$2                AND status IN ('requested', 'claimed')              ORDER BY requested_at DESC, created_at DESC LIMIT 1"
        )
        .to_string();
        sqlx::query_as::<_, AgentWakeupRequestRow>(&sql)
            .bind(company_id)
            .bind(agent_id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// Atomically claim stale wakeup requests whose `claimed_at` is older than
    /// the given threshold. Mirrors Node `recoverStaleWakeupClaims` /
    /// `tickStaleWakeupClaims`. Returns the number of rows that were reset.
    pub async fn recover_stale_wakeup_claims(
        &self,
        stale_threshold_seconds: i64,
    ) -> sqlx::Result<u64> {
        let result = sqlx::query(
            "UPDATE agent_wakeup_requests SET status='requested', claimed_at=NULL,                 updated_at=now()              WHERE status='claimed'                AND claimed_at IS NOT NULL                AND claimed_at <= now() - ($1 * interval '1 second')",
        )
        .bind(stale_threshold_seconds.clamp(1, 86_400))
        .execute(self.db.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn increment_wakeup_coalesced_count(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> sqlx::Result<Option<AgentWakeupRequestRow>> {
        let sql = format!(
            "UPDATE agent_wakeup_requests SET coalesced_count=coalesced_count+1, updated_at=now() \
             WHERE company_id=$1 AND id=$2 \
             AND status IN ('queued','deferred_issue_execution','claimed') \
             RETURNING {WAKEUP_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(id)
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

    /// Round 168: 按 company_id 统计 agents 的 status 分布。
    pub async fn count_by_status(&self, company_id: Uuid) -> sqlx::Result<Vec<(String, i64)>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT status, COUNT(*)::bigint FROM agents WHERE company_id = $1 GROUP BY status",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    // ---- Round 169: built_in_agents route 仓储化新增方法 ----

    /// Round 169: 列出 company 全部 builtInKey（distinct）。
    pub async fn list_built_in_keys(&self, company_id: Uuid) -> sqlx::Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT metadata->>'builtInKey' FROM agents \
             WHERE company_id = $1 AND metadata->>'builtInKey' IS NOT NULL",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows.into_iter().map(|(k,)| k).collect())
    }

    /// Round 169: 找 builtInKey 对应 agent 的 id（可能没有）。
    pub async fn find_built_in_agent_id(
        &self,
        company_id: Uuid,
        key: &str,
    ) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM agents WHERE company_id = $1 \
             AND metadata->>'builtInKey' = $2 LIMIT 1",
        )
        .bind(company_id)
        .bind(key)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    /// Round 169: 触摸 built-in agent 的 updated_at。
    pub async fn touch_built_in(
        &self,
        company_id: Uuid,
        key: &str,
    ) -> sqlx::Result<u64> {
        let n = sqlx::query(
            "UPDATE agents SET updated_at = now() \
             WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
        )
        .bind(company_id)
        .bind(key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Round 169: 安装一个 built-in agent（幂等，ON CONFLICT DO NOTHING）。
    pub async fn install_built_in(
        &self,
        company_id: Uuid,
        name: &str,
        role: &str,
        metadata: &Value,
    ) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO agents (company_id, name, role, status, adapter_type, metadata) \
             VALUES ($1, $2, $3, 'idle', 'codex_local', $4) \
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(company_id)
        .bind(name)
        .bind(role)
        .bind(metadata)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    /// Round 169: 重置 built-in agent（status=idle, 清空 pause 字段）。
    pub async fn reset_built_in(
        &self,
        company_id: Uuid,
        key: &str,
    ) -> sqlx::Result<u64> {
        let n = sqlx::query(
            "UPDATE agents SET status = 'idle', pause_reason = NULL, paused_at = NULL, updated_at = now() \
             WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
        )
        .bind(company_id)
        .bind(key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Round 169: 归档 built-in agent。
    pub async fn archive_built_in(
        &self,
        company_id: Uuid,
        key: &str,
    ) -> sqlx::Result<u64> {
        let n = sqlx::query(
            "UPDATE agents SET status = 'archived', archived_at = now(), updated_at = now() \
             WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
        )
        .bind(company_id)
        .bind(key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Round 169: 恢复 built-in agent。
    pub async fn restore_built_in(
        &self,
        company_id: Uuid,
        key: &str,
    ) -> sqlx::Result<u64> {
        let n = sqlx::query(
            "UPDATE agents SET status = 'idle', archived_at = NULL, updated_at = now() \
             WHERE company_id = $1 AND metadata->>'builtInKey' = $2",
        )
        .bind(company_id)
        .bind(key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Round 171: 按 status 拆分 agents 计数（error/running/paused）。
    pub async fn status_breakdown(&self, company_id: Uuid) -> sqlx::Result<(i64, i64, i64)> {
        let row: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
                COUNT(*) FILTER (WHERE status = 'error')::bigint, \
                COUNT(*) FILTER (WHERE status = 'running')::bigint, \
                COUNT(*) FILTER (WHERE status = 'paused')::bigint \
             FROM agents WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await
        .unwrap_or((0, 0, 0));
        Ok(row)
    }

    /// Round 171: 统计 over-budget agents（spent >= budget）。
    pub async fn count_over_budget(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM agents WHERE company_id = $1 \
             AND budget_monthly_cents > 0 AND spent_monthly_cents >= budget_monthly_cents",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await
        .unwrap_or(0);
        Ok(n)
    }
    /// Round 188: org_chart_svg -- minimal agent projection for SVG render.
    pub async fn list_org_chart_simple(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<(Uuid, String, String, String, Option<Uuid>)>> {
        sqlx::query_as(
            "SELECT id, name, role, status, reports_to FROM agents \
             WHERE company_id = $1 ORDER BY created_at ASC",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

}

#[cfg(test)]
mod tests {
    use super::{HeartbeatInvocationSource, WakeupRequestStatus, WakeupTriggerDetail};

    #[test]
    fn wakeup_contract_values_round_trip() {
        let statuses = [
            WakeupRequestStatus::Queued,
            WakeupRequestStatus::DeferredIssueExecution,
            WakeupRequestStatus::Claimed,
            WakeupRequestStatus::Coalesced,
            WakeupRequestStatus::Skipped,
            WakeupRequestStatus::Completed,
            WakeupRequestStatus::Failed,
            WakeupRequestStatus::Cancelled,
        ];
        for status in statuses {
            assert_eq!(status.as_str().parse(), Ok(status));
        }

        let sources = [
            HeartbeatInvocationSource::Timer,
            HeartbeatInvocationSource::Assignment,
            HeartbeatInvocationSource::OnDemand,
            HeartbeatInvocationSource::Automation,
        ];
        for source in sources {
            assert_eq!(source.as_str().parse(), Ok(source));
        }

        let trigger_details = [
            WakeupTriggerDetail::Manual,
            WakeupTriggerDetail::Ping,
            WakeupTriggerDetail::Callback,
            WakeupTriggerDetail::System,
        ];
        for trigger_detail in trigger_details {
            assert_eq!(trigger_detail.as_str().parse(), Ok(trigger_detail));
        }
    }

    #[test]
    fn wakeup_lifecycle_rejects_terminal_reactivation() {
        assert!(WakeupRequestStatus::Queued.can_transition_to(WakeupRequestStatus::Claimed));
        assert!(WakeupRequestStatus::Queued
            .can_transition_to(WakeupRequestStatus::DeferredIssueExecution));
        assert!(WakeupRequestStatus::DeferredIssueExecution
            .can_transition_to(WakeupRequestStatus::Queued));
        assert!(WakeupRequestStatus::Claimed.can_transition_to(WakeupRequestStatus::Completed));
        assert!(WakeupRequestStatus::Claimed.can_transition_to(WakeupRequestStatus::Failed));
        assert!(WakeupRequestStatus::Claimed.can_transition_to(WakeupRequestStatus::Cancelled));

        for terminal in [
            WakeupRequestStatus::Coalesced,
            WakeupRequestStatus::Skipped,
            WakeupRequestStatus::Completed,
            WakeupRequestStatus::Failed,
            WakeupRequestStatus::Cancelled,
        ] {
            assert!(terminal.is_terminal());
            assert!(!terminal.can_transition_to(WakeupRequestStatus::Queued));
            assert!(terminal.can_transition_to(terminal));
        }
    }
}
