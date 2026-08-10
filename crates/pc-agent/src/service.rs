use async_trait::async_trait;
use pc_errors::{conflict, internal, unprocessable, validation, Error, Result};
use pc_repos::{
    agent::{
        AgentConfigRecord, AgentConfigRevisionRow, AgentRepo, AgentRow, AgentRuntimeStateRow,
        AgentTaskSessionRow, CreateAgentApiKeyRecord, CreateAgentRecord, NewAgentConfigRevision,
        NewHireApproval,
    },
    approval::ApprovalRow,
    company::CompanyRepo,
    Db,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// R594: agent 生命周期事件。
///
/// hook 可以订阅以触发副作用（暂停相关 workflow / audit log / 创建 approval）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLifecycleEvent {
    /// Agent 被终止。
    Terminated { id: Uuid, company_id: Uuid, role: String },
    /// Agent 被暂停（含原因）。
    Paused { id: Uuid, company_id: Uuid, reason: String },
    /// Agent 被恢复。
    Resumed { id: Uuid, company_id: Uuid },
}

/// R594: agent lifecycle hook trait。
///
/// 默认全部 noop — 调用方可选择性实现。
#[async_trait]
pub trait AgentHook: Send + Sync {
    async fn on_lifecycle(
        &self,
        _event: AgentLifecycleEvent,
    ) -> Result<()> {
        Ok(())
    }

    /// R604: 组织架构图已计算完成（含 agent 数）。
    async fn on_org_chart_computed(
        &self,
        _company_id: Uuid,
        _count: i64,
    ) -> Result<()> {
        Ok(())
    }
}

/// R594: Noop hook — 默认不触发任何副作用。
pub struct NoopAgentHook;
#[async_trait]
impl AgentHook for NoopAgentHook {}

/// R594: 记录 hook 调用 — 测试用。
#[derive(Default)]
pub struct RecordingAgentHook {
    pub events: std::sync::Mutex<Vec<AgentLifecycleEvent>>,
    /// R604: 记录组织架构图计算事件 — (company_id, count)
    pub org_chart_computed: std::sync::Mutex<Vec<(Uuid, i64)>>,
}

#[async_trait]
impl AgentHook for RecordingAgentHook {
    async fn on_lifecycle(
        &self,
        event: AgentLifecycleEvent,
    ) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }

    async fn on_org_chart_computed(
        &self,
        company_id: Uuid,
        count: i64,
    ) -> Result<()> {
        self.org_chart_computed
            .lock()
            .expect("lock")
            .push((company_id, count));
        Ok(())
    }
}

use crate::{contains_redacted_marker, AgentConfigSnapshot};

#[derive(Debug, Clone)]
pub struct CreateAgent {
    pub id: Option<Uuid>,
    pub company_id: Uuid,
    pub name: String,
    pub role: String,
    pub title: Option<String>,
    pub icon: Option<String>,
    pub reports_to: Option<Uuid>,
    pub capabilities: Option<String>,
    pub adapter_type: String,
    pub adapter_config: Value,
    pub runtime_config: Value,
    pub default_environment_id: Option<Uuid>,
    pub budget_monthly_cents: i32,
    pub permissions: Value,
    pub metadata: Option<Value>,
    pub status: String,
}

impl Default for CreateAgent {
    fn default() -> Self {
        Self {
            id: None,
            company_id: Uuid::nil(),
            name: String::new(),
            role: "general".into(),
            title: None,
            icon: None,
            reports_to: None,
            capabilities: None,
            adapter_type: "process".into(),
            adapter_config: json!({}),
            runtime_config: json!({}),
            default_environment_id: None,
            budget_monthly_cents: 0,
            permissions: json!({}),
            metadata: None,
            status: "idle".into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentPatch {
    pub name: Option<String>,
    pub role: Option<String>,
    pub title: Option<Option<String>>,
    pub icon: Option<Option<String>>,
    pub reports_to: Option<Option<Uuid>>,
    pub capabilities: Option<Option<String>>,
    pub adapter_type: Option<String>,
    pub adapter_config: Option<Value>,
    pub runtime_config: Option<Value>,
    pub default_environment_id: Option<Option<Uuid>>,
    pub budget_monthly_cents: Option<i32>,
    pub metadata: Option<Option<Value>>,
}

impl AgentPatch {
    fn apply_to(self, target: &mut AgentConfigSnapshot) {
        if let Some(value) = self.name {
            target.name = value;
        }
        if let Some(value) = self.role {
            target.role = value;
        }
        if let Some(value) = self.title {
            target.title = value;
        }
        if let Some(value) = self.icon {
            target.icon = value;
        }
        if let Some(value) = self.reports_to {
            target.reports_to = value;
        }
        if let Some(value) = self.capabilities {
            target.capabilities = value;
        }
        if let Some(value) = self.adapter_type {
            target.adapter_type = value;
        }
        if let Some(value) = self.adapter_config {
            target.adapter_config = value;
        }
        if let Some(value) = self.runtime_config {
            target.runtime_config = value;
        }
        if let Some(value) = self.default_environment_id {
            target.default_environment_id = value;
        }
        if let Some(value) = self.budget_monthly_cents {
            target.budget_monthly_cents = value;
        }
        if let Some(value) = self.metadata {
            target.metadata = value;
        }
    }

    fn from_snapshot(snapshot: AgentConfigSnapshot) -> Self {
        Self {
            name: Some(snapshot.name),
            role: Some(snapshot.role),
            title: Some(snapshot.title),
            icon: Some(snapshot.icon),
            reports_to: Some(snapshot.reports_to),
            capabilities: Some(snapshot.capabilities),
            adapter_type: Some(snapshot.adapter_type),
            adapter_config: Some(snapshot.adapter_config),
            runtime_config: Some(snapshot.runtime_config),
            default_environment_id: Some(snapshot.default_environment_id),
            budget_monthly_cents: Some(snapshot.budget_monthly_cents),
            metadata: Some(snapshot.metadata),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RevisionContext {
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub source: String,
    pub rolled_back_from_revision_id: Option<Uuid>,
}

impl RevisionContext {
    pub fn user(user_id: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            created_by_user_id: Some(user_id.into()),
            source: source.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfigRevision {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub source: String,
    pub rolled_back_from_revision_id: Option<Uuid>,
    pub changed_keys: Vec<String>,
    pub before_config: Value,
    pub after_config: Value,
    pub created_at: pc_core::Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    Manual,
    Budget,
    System,
}

impl PauseReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Budget => "budget",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateAgentKey {
    pub name: String,
    pub responsible_user_id: Option<String>,
    pub scope: Value,
}

impl Default for CreateAgentKey {
    fn default() -> Self {
        Self {
            name: String::new(),
            responsible_user_id: None,
            scope: json!({"kind": "standard"}),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentApiKey {
    pub id: Uuid,
    pub name: String,
    pub responsible_user_id: Option<String>,
    pub scope: Value,
    pub created_at: pc_core::Timestamp,
    pub revoked_at: Option<pc_core::Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentKeyCreated {
    pub id: Uuid,
    pub name: String,
    pub responsible_user_id: Option<String>,
    pub scope: Value,
    pub token: String,
    pub created_at: pc_core::Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHire {
    pub agent: AgentRow,
    pub approval: Option<ApprovalRow>,
}

#[derive(Debug, Clone)]
pub struct AgentPermissionUpdate {
    pub can_create_agents: bool,
    pub can_create_skills: Option<bool>,
    pub can_assign_tasks: bool,
    pub trust_preset: Option<Value>,
    pub authorization_policy: Option<Value>,
    pub granted_by_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeState {
    pub agent_id: Uuid,
    pub company_id: Uuid,
    pub adapter_type: String,
    pub session_id: Option<String>,
    pub session_display_id: Option<String>,
    pub session_params_json: Option<Value>,
    pub state_json: Value,
    pub last_run_id: Option<Uuid>,
    pub last_run_status: Option<String>,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cached_input_tokens: i64,
    pub total_cost_cents: i64,
    pub last_error: Option<String>,
    pub created_at: pc_core::Timestamp,
    pub updated_at: pc_core::Timestamp,
}

#[derive(Debug, Clone, Default)]
pub struct ResetRuntimeSession {
    pub task_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetRuntimeState {
    #[serde(flatten)]
    pub state: AgentRuntimeState,
    pub cleared_task_sessions: u64,
}

impl std::ops::Deref for ResetRuntimeState {
    type Target = AgentRuntimeState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl From<AgentConfigRevisionRow> for AgentConfigRevision {
    fn from(row: AgentConfigRevisionRow) -> Self {
        Self {
            id: row.id,
            company_id: row.company_id,
            agent_id: row.agent_id,
            created_by_agent_id: row.created_by_agent_id,
            created_by_user_id: row.created_by_user_id,
            source: row.source,
            rolled_back_from_revision_id: row.rolled_back_from_revision_id,
            changed_keys: row.changed_keys.0,
            before_config: row.before_config,
            after_config: row.after_config,
            created_at: row.created_at,
        }
    }
}

// ============================================================
// R604: 组织架构图节点（递归 reports_to 关系）
// ============================================================
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgChartNode {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_to: Option<Uuid>,
    /// 子节点（递归）。无下属时为空数组。
    #[serde(default)]
    pub reports: Vec<OrgChartNode>,
}

// ============================================================
// R604: 命令链节点（id/name/role/title）
// ============================================================
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChainOfCommandNode {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

// ============================================================
// R604: resolveByReference 解析结果
// ============================================================
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolveByRefResult {
    /// 唯一匹配到一个 agent
    Found { agent: AgentRow },
    /// 没找到
    NotFound,
    /// urlKey 匹配到多个 agent（ambiguous）
    Ambiguous { candidates: Vec<AgentRow> },
}

#[derive(Clone)]
pub struct AgentService {
    db: Db,
    hooks: Vec<std::sync::Arc<dyn AgentHook>>,
}

impl AgentService {
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    /// R594: 构造带 hook 的 service。
    #[must_use]
    pub fn with_hooks(db: Db, hooks: Vec<std::sync::Arc<dyn AgentHook>>) -> Self {
        Self { db, hooks }
    }

    /// R594: 链式添加 hook。
    #[must_use]
    pub fn add_hook(mut self, hook: std::sync::Arc<dyn AgentHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// R594: 触发 lifecycle event 给所有 hook。
    async fn dispatch_lifecycle(
        &self,
        event: AgentLifecycleEvent,
    ) -> Result<()> {
        for hook in &self.hooks {
            if let Err(e) = hook.on_lifecycle(event.clone()).await {
                tracing::warn!(
                    agent_id = ?match &event {
                        AgentLifecycleEvent::Terminated { id, .. }
                        | AgentLifecycleEvent::Paused { id, .. }
                        | AgentLifecycleEvent::Resumed { id, .. } => id,
                    },
                    error = %e,
                    "agent lifecycle hook failed"
                );
            }
        }
        Ok(())
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<AgentRow>> {
        AgentRepo::new(&self.db)
            .get(id)
            .await
            .map_err(map_sql_error)
    }

    /// R588: 列出公司下的 agent（route service 化）
    pub async fn list_by_company(&self, company_id: Uuid) -> Result<Vec<AgentRow>> {
        AgentRepo::new(&self.db)
            .list_by_company(company_id)
            .await
            .map_err(map_sql_error)
    }

    /// R588: 列出所有 agent（route service 化）
    pub async fn list_all(&self) -> Result<Vec<AgentRow>> {
        AgentRepo::new(&self.db)
            .list_all()
            .await
            .map_err(map_sql_error)
    }

    /// R588: 删除 agent（route service 化）。
    /// 返回 true 表示实际删除了一行，false 表示 agent 不存在。
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        AgentRepo::new(&self.db)
            .delete(id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn create(&self, input: CreateAgent) -> Result<AgentRow> {
        if input.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        if input.name.trim().is_empty() {
            return Err(validation("name must not be empty"));
        }
        let normalized =
            crate::permissions::normalize_agent_permissions(input.permissions.clone(), &input.role);
        let permissions = normalized.to_value();
        AgentRepo::new(&self.db)
            .create_full(CreateAgentRecord {
                id: input.id.unwrap_or_else(Uuid::new_v4),
                company_id: input.company_id,
                name: input.name.trim().to_owned(),
                role: input.role,
                title: input.title,
                icon: input.icon,
                reports_to: input.reports_to,
                capabilities: input.capabilities,
                adapter_type: input.adapter_type,
                adapter_config: input.adapter_config,
                runtime_config: input.runtime_config,
                default_environment_id: input.default_environment_id,
                budget_monthly_cents: input.budget_monthly_cents,
                permissions,
                metadata: input.metadata,
                status: input.status,
            })
            .await
            .map_err(map_sql_error)
    }

    pub async fn hire(&self, mut input: CreateAgent, actor: RevisionContext) -> Result<AgentHire> {
        if input.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        if input.name.trim().is_empty() {
            return Err(validation("name must not be empty"));
        }
        let company = CompanyRepo::new(&self.db)
            .get(input.company_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| pc_errors::not_found("Company"))?;
        input.status = if company.require_board_approval_for_new_agents {
            "pending_approval".into()
        } else {
            "idle".into()
        };
        let id = input.id.unwrap_or_else(Uuid::new_v4);
        let normalized =
            crate::permissions::normalize_agent_permissions(input.permissions.clone(), &input.role);
        let permissions = normalized.to_value();
        let payload = json!({
            "name": input.name,
            "role": input.role,
            "title": input.title,
            "icon": input.icon,
            "reportsTo": input.reports_to,
            "capabilities": input.capabilities,
            "adapterType": input.adapter_type,
            "adapterConfig": crate::sanitize_snapshot_value(&input.adapter_config),
            "runtimeConfig": crate::sanitize_snapshot_value(&input.runtime_config),
            "budgetMonthlyCents": input.budget_monthly_cents,
            "metadata": input.metadata.as_ref().map(crate::sanitize_snapshot_value),
            "agentId": id,
        });
        let approval = company
            .require_board_approval_for_new_agents
            .then(|| NewHireApproval {
                requested_by_agent_id: actor.created_by_agent_id,
                requested_by_user_id: actor.created_by_user_id,
                payload,
            });
        let record = AgentRepo::new(&self.db)
            .create_hire(
                CreateAgentRecord {
                    id,
                    company_id: input.company_id,
                    name: input.name.trim().to_owned(),
                    role: input.role,
                    title: input.title,
                    icon: input.icon,
                    reports_to: input.reports_to,
                    capabilities: input.capabilities,
                    adapter_type: input.adapter_type,
                    adapter_config: input.adapter_config,
                    runtime_config: input.runtime_config,
                    default_environment_id: input.default_environment_id,
                    budget_monthly_cents: input.budget_monthly_cents,
                    permissions,
                    metadata: input.metadata,
                    status: input.status,
                },
                approval,
            )
            .await
            .map_err(map_sql_error)?;
        Ok(AgentHire {
            agent: record.agent,
            approval: record.approval,
        })
    }

    pub async fn approve(&self, id: Uuid) -> Result<Option<AgentRow>> {
        let repo = AgentRepo::new(&self.db);
        let Some(existing) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        if existing.status != "pending_approval" {
            return Ok(Some(existing));
        }
        repo.approve_pending(id).await.map_err(map_sql_error)
    }

    pub async fn update_permissions(
        &self,
        id: Uuid,
        input: AgentPermissionUpdate,
    ) -> Result<Option<AgentRow>> {
        let repo = AgentRepo::new(&self.db);
        let Some(existing) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        if existing.status == "pending_approval" {
            return Err(conflict(
                "Pending approval agent permissions cannot be changed before board approval",
            ));
        }
        let mut permissions = existing
            .permissions
            .as_object()
            .cloned()
            .unwrap_or_default();
        permissions.insert(
            "canCreateAgents".into(),
            Value::Bool(input.can_create_agents),
        );
        if let Some(value) = input.can_create_skills {
            permissions.insert("canCreateSkills".into(), Value::Bool(value));
        }
        if let Some(value) = input.trust_preset {
            permissions.insert("trustPreset".into(), value);
        }
        if let Some(value) = input.authorization_policy {
            permissions.insert("authorizationPolicy".into(), value);
        }
        let normalized = crate::permissions::normalize_agent_permissions(
            Value::Object(permissions),
            &existing.role,
        );
        let permissions = normalized.to_value();
        let effective_can_assign = existing.role.eq_ignore_ascii_case("ceo")
            || input.can_create_agents
            || input.can_assign_tasks;
        repo.update_permissions_and_access(
            id,
            permissions,
            effective_can_assign,
            input.granted_by_user_id.as_deref(),
        )
        .await
        .map_err(map_sql_error)
    }

    pub async fn pause(&self, id: Uuid, reason: PauseReason) -> Result<Option<AgentRow>> {
        let repo = AgentRepo::new(&self.db);
        let Some(existing) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        if existing.status == "terminated" {
            return Err(conflict("Cannot pause terminated agent"));
        }
        let updated = repo.pause(id, reason.as_str()).await.map_err(map_sql_error)?;
        // R594: 触发 lifecycle hook
        self.dispatch_lifecycle(AgentLifecycleEvent::Paused {
            id,
            company_id: existing.company_id,
            reason: reason.as_str().to_owned(),
        })
        .await?;
        Ok(updated)
    }

    pub async fn resume(&self, id: Uuid) -> Result<Option<AgentRow>> {
        let repo = AgentRepo::new(&self.db);
        let Some(existing) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        match existing.status.as_str() {
            "terminated" => return Err(conflict("Cannot resume terminated agent")),
            "pending_approval" => {
                return Err(conflict("Pending approval agents cannot be resumed"));
            }
            _ => {}
        }
        let updated = repo.resume(id).await.map_err(map_sql_error)?;
        // R594: 触发 lifecycle hook
        self.dispatch_lifecycle(AgentLifecycleEvent::Resumed {
            id,
            company_id: existing.company_id,
        })
        .await?;
        Ok(updated)
    }

    pub async fn clear_error(&self, id: Uuid) -> Result<Option<AgentRow>> {
        let repo = AgentRepo::new(&self.db);
        let Some(existing) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        match existing.status.as_str() {
            "terminated" => return Err(conflict("Cannot clear error on terminated agent")),
            "pending_approval" => {
                return Err(conflict(
                    "Pending approval agents cannot have errors cleared",
                ));
            }
            "error" => {}
            _ => {
                return Err(conflict(
                    "Only agents in error status can have their error cleared",
                ));
            }
        }
        repo.clear_error(id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| conflict("Only agents in error status can have their error cleared"))
            .map(Some)
    }

    pub async fn terminate(&self, id: Uuid) -> Result<Option<AgentRow>> {
        let repo = AgentRepo::new(&self.db);
        // R594: 先取 agent 信息（用于 hook payload），如果不存在则 short-circuit
        let Some(existing) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        let updated = repo.terminate(id).await.map_err(map_sql_error)?;
        // R594: 触发 lifecycle hook
        self.dispatch_lifecycle(AgentLifecycleEvent::Terminated {
            id,
            company_id: existing.company_id,
            role: existing.role.clone(),
        })
        .await?;
        Ok(updated)
    }

    pub async fn create_api_key(&self, id: Uuid, input: CreateAgentKey) -> Result<AgentKeyCreated> {
        let repo = AgentRepo::new(&self.db);
        let existing = repo
            .get(id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| pc_errors::not_found("Agent"))?;
        match existing.status.as_str() {
            "pending_approval" => {
                return Err(conflict("Cannot create keys for pending approval agents"));
            }
            "terminated" => return Err(conflict("Cannot create keys for terminated agents")),
            _ => {}
        }
        let name = input.name.trim();
        if name.is_empty() {
            return Err(validation("name must not be empty"));
        }
        let scope = normalize_api_key_scope(input.scope)?;
        let token = create_token();
        let key_hash = hash_token(&token);
        let row = repo
            .create_api_key(CreateAgentApiKeyRecord {
                agent_id: id,
                company_id: existing.company_id,
                name: name.to_owned(),
                key_hash,
                responsible_user_id: input
                    .responsible_user_id
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty()),
                scope_config: (scope["kind"] != "standard").then(|| scope.clone()),
            })
            .await
            .map_err(map_sql_error)?;
        Ok(AgentKeyCreated {
            id: row.id,
            name: row.name,
            responsible_user_id: row.responsible_user_id,
            scope,
            token,
            created_at: row.created_at,
        })
    }

    pub async fn list_api_keys(&self, id: Uuid) -> Result<Vec<AgentApiKey>> {
        AgentRepo::new(&self.db)
            .list_api_keys(id)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| AgentApiKey {
                        id: row.id,
                        name: row.name,
                        responsible_user_id: row.responsible_user_id,
                        scope: row
                            .scope_config
                            .unwrap_or_else(|| json!({"kind": "standard"})),
                        created_at: row.created_at,
                        revoked_at: row.revoked_at,
                    })
                    .collect()
            })
            .map_err(map_sql_error)
    }

    pub async fn revoke_api_key(
        &self,
        agent_id: Uuid,
        key_id: Uuid,
    ) -> Result<Option<AgentApiKey>> {
        AgentRepo::new(&self.db)
            .revoke_api_key(agent_id, key_id)
            .await
            .map(|row| {
                row.map(|row| AgentApiKey {
                    id: row.id,
                    name: row.name,
                    responsible_user_id: row.responsible_user_id,
                    scope: row
                        .scope_config
                        .unwrap_or_else(|| json!({"kind": "standard"})),
                    created_at: row.created_at,
                    revoked_at: row.revoked_at,
                })
            })
            .map_err(map_sql_error)
    }

    pub async fn update(
        &self,
        id: Uuid,
        patch: AgentPatch,
        revision_context: RevisionContext,
    ) -> Result<Option<AgentRow>> {
        let repo = AgentRepo::new(&self.db);
        let Some(existing) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        let mut next = AgentConfigSnapshot::from(&existing);
        patch.apply_to(&mut next);
        if next.name.trim().is_empty() {
            return Err(validation("name must not be empty"));
        }

        let before = AgentConfigSnapshot::from(&existing).sanitized();
        let after = next.clone().sanitized();
        let changed_keys = before
            .changed_keys(&after)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let revision = (!changed_keys.is_empty()).then(|| NewAgentConfigRevision {
            company_id: existing.company_id,
            created_by_agent_id: revision_context.created_by_agent_id,
            created_by_user_id: revision_context.created_by_user_id,
            source: if revision_context.source.is_empty() {
                "patch".into()
            } else {
                revision_context.source
            },
            rolled_back_from_revision_id: revision_context.rolled_back_from_revision_id,
            changed_keys,
            before_config: serde_json::to_value(before).expect("serialize config snapshot"),
            after_config: serde_json::to_value(after).expect("serialize config snapshot"),
        });

        repo.replace_config_with_revision(id, config_record(next), revision)
            .await
            .map_err(map_sql_error)
    }

    pub async fn list_config_revisions(&self, id: Uuid) -> Result<Vec<AgentConfigRevision>> {
        AgentRepo::new(&self.db)
            .list_config_revisions(id)
            .await
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .map_err(map_sql_error)
    }

    pub async fn get_config_revision(
        &self,
        id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<AgentConfigRevision>> {
        AgentRepo::new(&self.db)
            .get_config_revision(id, revision_id)
            .await
            .map(|row| row.map(Into::into))
            .map_err(map_sql_error)
    }

    pub async fn runtime_state(&self, id: Uuid) -> Result<Option<AgentRuntimeState>> {
        let repo = AgentRepo::new(&self.db);
        let Some(agent) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        let state = repo
            .ensure_runtime_state(&agent)
            .await
            .map_err(map_sql_error)?;
        let latest = repo
            .latest_task_session(agent.company_id, agent.id)
            .await
            .map_err(map_sql_error)?;
        Ok(Some(runtime_state(state, latest.as_ref())))
    }

    pub async fn list_task_sessions(&self, id: Uuid) -> Result<Option<Vec<AgentTaskSessionRow>>> {
        let repo = AgentRepo::new(&self.db);
        let Some(agent) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        let mut sessions = repo
            .list_task_sessions(agent.company_id, id)
            .await
            .map_err(map_sql_error)?;
        for session in &mut sessions {
            session.session_params_json = session
                .session_params_json
                .as_ref()
                .map(crate::sanitize_snapshot_value);
        }
        Ok(Some(sessions))
    }

    pub async fn reset_runtime_session(
        &self,
        id: Uuid,
        input: ResetRuntimeSession,
    ) -> Result<Option<ResetRuntimeState>> {
        let repo = AgentRepo::new(&self.db);
        let Some(agent) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        let task_key = input
            .task_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let (state, cleared_task_sessions) = repo
            .reset_runtime_session(&agent, task_key)
            .await
            .map_err(map_sql_error)?;
        Ok(Some(ResetRuntimeState {
            state: runtime_state(state, None),
            cleared_task_sessions,
        }))
    }

    pub async fn rollback_config_revision(
        &self,
        id: Uuid,
        revision_id: Uuid,
        mut context: RevisionContext,
    ) -> Result<Option<AgentRow>> {
        let Some(revision) = self.get_config_revision(id, revision_id).await? else {
            return Ok(None);
        };
        if contains_redacted_marker(&revision.after_config) {
            return Err(unprocessable(
                "Cannot roll back a revision that contains redacted secret values",
            ));
        }
        let snapshot: AgentConfigSnapshot = serde_json::from_value(revision.after_config)
            .map_err(|error| internal(format!("invalid config revision snapshot: {error}")))?;
        context.source = "rollback".into();
        context.rolled_back_from_revision_id = Some(revision_id);
        self.update(id, AgentPatch::from_snapshot(snapshot), context)
            .await
    }

    // ============================================================
    // R604: orgForCompany — 列出公司下非 terminated 的 agent，
    //       递归构建 reports_to 树（如果 manager 不在公司内则视为 root）
    // ============================================================
    pub async fn org_for_company(&self, company_id: Uuid) -> Result<Vec<OrgChartNode>> {
        let repo = AgentRepo::new(&self.db);
        let rows = repo.list_by_company(company_id).await.map_err(map_sql_error)?;
        let company_id_set: HashSet<Uuid> = rows.iter().map(|r| r.id).collect();
        let active: Vec<AgentRow> = rows
            .into_iter()
            .filter(|r| r.status != "terminated")
            .collect();
        // 按 manager 分组。如果 reportsTo 指向公司外的 agent，则视为 root
        let mut by_manager: HashMap<Option<Uuid>, Vec<AgentRow>> = HashMap::new();
        for row in &active {
            let key = match row.reports_to {
                Some(mgr) if company_id_set.contains(&mgr) => Some(mgr),
                _ => None,
            };
            by_manager.entry(key).or_default().push(row.clone());
        }
        // 按 name 排序（与 Node listCompanyAgentRows 行为一致）
        for v in by_manager.values_mut() {
            v.sort_by(|a, b| a.name.cmp(&b.name));
        }
        // 递归构建
        fn build(
            manager_id: Option<Uuid>,
            by_manager: &HashMap<Option<Uuid>, Vec<AgentRow>>,
        ) -> Vec<OrgChartNode> {
            let members = match by_manager.get(&manager_id) {
                Some(m) => m,
                None => return Vec::new(),
            };
            members
                .iter()
                .map(|row| OrgChartNode {
                    id: row.id,
                    name: row.name.clone(),
                    role: row.role.clone(),
                    title: row.title.clone(),
                    status: row.status.clone(),
                    reports_to: row.reports_to,
                    reports: build(Some(row.id), by_manager),
                })
                .collect()
        }
        let tree = build(None, &by_manager);
        let count = active.len() as i64;
        self.dispatch_org_chart_computed(company_id, count).await?;
        Ok(tree)
    }

    /// R604: 触发 org chart computed 事件给所有 hook。
    async fn dispatch_org_chart_computed(
        &self,
        company_id: Uuid,
        count: i64,
    ) -> Result<()> {
        for hook in &self.hooks {
            if let Err(e) = hook.on_org_chart_computed(company_id, count).await {
                tracing::warn!(
                    company_id = %company_id,
                    error = %e,
                    "agent org chart hook failed"
                );
            }
        }
        Ok(())
    }

    // ============================================================
    // R604: getChainOfCommand — 从 agent 向上追溯 reports_to 链
    //       限制 50 跳 + visited 集合防环（与 Node 行为一致）
    // ============================================================
    pub async fn get_chain_of_command(&self, agent_id: Uuid) -> Result<Vec<ChainOfCommandNode>> {
        let repo = AgentRepo::new(&self.db);
        let Some(start) = repo.get(agent_id).await.map_err(map_sql_error)? else {
            return Ok(Vec::new());
        };
        let mut visited: HashSet<Uuid> = HashSet::new();
        visited.insert(start.id);
        let mut chain: Vec<ChainOfCommandNode> = Vec::new();
        let mut current_id = start.reports_to;
        while let Some(mgr_id) = current_id {
            if visited.contains(&mgr_id) || chain.len() >= 50 {
                break;
            }
            visited.insert(mgr_id);
            let Some(mgr) = repo.get(mgr_id).await.map_err(map_sql_error)? else {
                break;
            };
            chain.push(ChainOfCommandNode {
                id: mgr.id,
                name: mgr.name.clone(),
                role: mgr.role.clone(),
                title: mgr.title.clone(),
            });
            current_id = mgr.reports_to;
        }
        Ok(chain)
    }

    // ============================================================
    // R604: resolveByReference — 按 urlKey / id 解析 agent
    //       1) trim 后如果匹配 UUID 格式 → 按 id 查（必须属于指定 company）
    //       2) 否则按 urlKey（normalizeAgentUrlKey）查同公司非 terminated agent
    //       3) 0 匹配 → NotFound；1 匹配 → Found；>1 匹配 → Ambiguous
    // ============================================================
    pub async fn resolve_by_reference(
        &self,
        company_id: Uuid,
        reference: &str,
    ) -> Result<ResolveByRefResult> {
        let raw = reference.trim();
        if raw.is_empty() {
            return Ok(ResolveByRefResult::NotFound);
        }
        let repo = AgentRepo::new(&self.db);
        if is_uuid_like(raw) {
            if let Ok(parsed) = Uuid::parse_str(raw) {
                if let Some(agent) = repo.get(parsed).await.map_err(map_sql_error)? {
                    if agent.company_id == company_id {
                        return Ok(ResolveByRefResult::Found { agent });
                    }
                }
            }
            return Ok(ResolveByRefResult::NotFound);
        }
        let Some(target_key) = normalize_agent_url_key(raw) else {
            return Ok(ResolveByRefResult::NotFound);
        };
        let rows = repo
            .list_by_company(company_id)
            .await
            .map_err(map_sql_error)?;
        let candidates: Vec<AgentRow> = rows
            .into_iter()
            .filter(|row| {
                row.status != "terminated"
                    && normalize_agent_url_key(&row.name).as_deref() == Some(target_key.as_str())
            })
            .collect();
        match candidates.len() {
            0 => Ok(ResolveByRefResult::NotFound),
            1 => Ok(ResolveByRefResult::Found {
                agent: candidates.into_iter().next().expect("single"),
            }),
            _ => Ok(ResolveByRefResult::Ambiguous { candidates }),
        }
    }
}

fn runtime_state(
    row: AgentRuntimeStateRow,
    latest: Option<&AgentTaskSessionRow>,
) -> AgentRuntimeState {
    AgentRuntimeState {
        agent_id: row.agent_id,
        company_id: row.company_id,
        adapter_type: row.adapter_type,
        session_id: row.session_id,
        session_display_id: latest.and_then(|session| session.session_display_id.clone()),
        session_params_json: latest
            .and_then(|session| session.session_params_json.as_ref())
            .map(crate::sanitize_snapshot_value),
        state_json: row.state_json,
        last_run_id: row.last_run_id,
        last_run_status: row.last_run_status,
        total_input_tokens: row.total_input_tokens,
        total_output_tokens: row.total_output_tokens,
        total_cached_input_tokens: row.total_cached_input_tokens,
        total_cost_cents: row.total_cost_cents,
        last_error: row.last_error,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn config_record(snapshot: AgentConfigSnapshot) -> AgentConfigRecord {
    AgentConfigRecord {
        name: snapshot.name,
        role: snapshot.role,
        title: snapshot.title,
        icon: snapshot.icon,
        reports_to: snapshot.reports_to,
        capabilities: snapshot.capabilities,
        adapter_type: snapshot.adapter_type,
        adapter_config: snapshot.adapter_config,
        runtime_config: snapshot.runtime_config,
        default_environment_id: snapshot.default_environment_id,
        budget_monthly_cents: snapshot.budget_monthly_cents,
        metadata: snapshot.metadata,
    }
}

/// R604: 把字符串规范化为 agent urlKey（仅含 a-z0-9 + `-`，首尾 `-` 去除）。
pub fn normalize_agent_url_key(value: &str) -> Option<String> {
    let trimmed = value.trim().to_lowercase();
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_dash = true; // 抑制开头连续 `-`
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed_out = out.trim_matches('-').to_string();
    if trimmed_out.is_empty() {
        None
    } else {
        Some(trimmed_out)
    }
}

/// R604: 简化的 UUID 形状判定（trim 后正则匹配）。
pub fn is_uuid_like(value: &str) -> bool {
    // trim + lowercase + 长度检查
    let v = value.trim();
    if v.len() != 36 {
        return false;
    }
    Uuid::parse_str(v).is_ok()
}

fn map_sql_error(error: sqlx::Error) -> Error {
    internal(format!("agent database operation failed: {error}"))
}

// 复用 permissions 模块的标准化逻辑（对齐 Node agent-permissions.ts）

fn normalize_api_key_scope(scope: Value) -> Result<Value> {
    let Some(object) = scope.as_object() else {
        return Err(validation("scope must be an object"));
    };
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("standard");
    if !matches!(kind, "standard" | "task_bridge" | "skill_test") {
        return Err(validation("unsupported agent API key scope"));
    }
    let mut normalized = object.clone();
    normalized.insert("kind".into(), Value::String(kind.into()));
    Ok(Value::Object(normalized))
}

fn create_token() -> String {
    let random = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    format!("pcp_{}", &random[..48])
}

fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
