//! `execution_workspaces` + `execution_lease` 域 — Agent 执行工作区。
//!
//! 设计：
//! - 工作区是 agent 实际执行命令的逻辑沙盒（含 cwd、branch、provider、生命周期状态）
//! - lease 表记录 agent 对 workspace 的独占占用，用于同时多 agent 场景的协调
//! - 所有写入走事务 + 检查 status 转换合法性

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Active,
    Cleaning,
    Closed,
    Failed,
}
impl WorkspaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Cleaning => "cleaning",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    LocalCheckout,
    DockerContainer,
    K8sPod,
    Ephemeral,
}
impl WorkspaceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalCheckout => "local_checkout",
            Self::DockerContainer => "docker_container",
            Self::K8sPod => "k8s_pod",
            Self::Ephemeral => "ephemeral",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    LocalFs,
    Docker,
    Kubernetes,
    HostedRemote,
}
impl ProviderType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalFs => "local_fs",
            Self::Docker => "docker",
            Self::Kubernetes => "kubernetes",
            Self::HostedRemote => "hosted_remote",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Holding,
    Released,
    Expired,
    Revoked,
}
impl LeaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Holding => "holding",
            Self::Released => "released",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }
}

const WS_COLS: &str = "id, company_id, project_id, project_workspace_id, source_issue_id,      mode, strategy_type, name, status, cwd, repo_url, base_ref, branch_name,      provider_type, provider_ref, derived_from_execution_workspace_id,      last_used_at, opened_at, closed_at, cleanup_eligible_at, cleanup_reason, metadata,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Uuid,
    pub project_workspace_id: Option<Uuid>,
    pub source_issue_id: Option<Uuid>,
    pub mode: String,
    pub strategy_type: String,
    pub name: String,
    pub status: String,
    pub cwd: Option<String>,
    pub repo_url: Option<String>,
    pub base_ref: Option<String>,
    pub branch_name: Option<String>,
    pub provider_type: String,
    pub provider_ref: Option<String>,
    pub derived_from_execution_workspace_id: Option<Uuid>,
    pub last_used_at: Timestamp,
    pub opened_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub cleanup_eligible_at: Option<Timestamp>,
    pub cleanup_reason: Option<String>,
    pub metadata: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const LEASE_COLS: &str = "id, company_id, workspace_id, agent_id, run_id, heartbeat_run_id,      state, token, acquired_at, expires_at, last_renewed_at, released_at, revocation_reason";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub run_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub state: String,
    pub token: String,
    pub acquired_at: Timestamp,
    pub expires_at: Timestamp,
    pub last_renewed_at: Timestamp,
    pub released_at: Option<Timestamp>,
    pub revocation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWorkspace {
    pub company_id: Uuid,
    pub project_id: Uuid,
    pub project_workspace_id: Option<Uuid>,
    pub source_issue_id: Option<Uuid>,
    pub mode: WorkspaceMode,
    pub strategy_type: String,
    pub name: String,
    pub cwd: Option<String>,
    pub repo_url: Option<String>,
    pub base_ref: Option<String>,
    pub branch_name: Option<String>,
    pub provider_type: ProviderType,
    pub provider_ref: Option<String>,
    pub derived_from: Option<Uuid>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLease {
    pub company_id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub run_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub ttl_secs: i64,
}

pub struct ExecutionRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ExecutionRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- workspaces ----

    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<WorkspaceRow>> {
        let sql = format!(
            "SELECT {WS_COLS} FROM execution_workspaces              WHERE company_id=$1 ORDER BY last_used_at DESC",
        );
        Ok(sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_for_project(
        &self,
        company_id: Uuid,
        project_id: Uuid,
    ) -> RepoResult<Vec<WorkspaceRow>> {
        let sql = format!(
            "SELECT {WS_COLS} FROM execution_workspaces              WHERE company_id=$1 AND project_id=$2              ORDER BY last_used_at DESC",
        );
        Ok(sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(company_id)
            .bind(project_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<WorkspaceRow>> {
        let sql = format!(
            "SELECT {WS_COLS} FROM execution_workspaces              WHERE company_id=$1 AND id=$2",
        );
        Ok(sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// R634: 查 execution workspace 的 company_id（用于在 HTTP 层做 authz 检查）。
    pub async fn company_id_for_workspace(&self, workspace_id: Uuid) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT company_id FROM execution_workspaces WHERE id = $1")
                .bind(workspace_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(c,)| c))
    }

    /// Round 108: 查单个 operation 元数据（company_id + heartbeat_run_id + 截断的 stdout/stderr + log_ref）
    /// 用于 `read_workspace_operation_log` 端点的开头查询。
    pub async fn find_operation_log_meta(
        &self,
        operation_id: Uuid,
    ) -> sqlx::Result<Option<WorkspaceOperationMetaRow>> {
        sqlx::query_as::<_, WorkspaceOperationMetaRow>(
            "SELECT company_id, heartbeat_run_id, stdout_excerpt, stderr_excerpt, log_ref              FROM workspace_operations WHERE id = $1",
        )
        .bind(operation_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create(&self, w: &NewWorkspace) -> RepoResult<WorkspaceRow> {
        if w.name.trim().is_empty() {
            return Err(RepoError::Invalid(
                "workspace name must not be empty".into(),
            ));
        }
        let sql = format!(
            "INSERT INTO execution_workspaces (company_id, project_id, project_workspace_id,                 source_issue_id, mode, strategy_type, name, cwd, repo_url, base_ref, branch_name,                 provider_type, provider_ref, derived_from_execution_workspace_id, metadata)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)              RETURNING {WS_COLS}",
        );
        Ok(sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(w.company_id)
            .bind(w.project_id)
            .bind(w.project_workspace_id)
            .bind(w.source_issue_id)
            .bind(w.mode.as_str())
            .bind(&w.strategy_type)
            .bind(&w.name)
            .bind(w.cwd.as_deref())
            .bind(w.repo_url.as_deref())
            .bind(w.base_ref.as_deref())
            .bind(w.branch_name.as_deref())
            .bind(w.provider_type.as_str())
            .bind(w.provider_ref.as_deref())
            .bind(w.derived_from)
            .bind(w.metadata.clone())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn touch_last_used(&self, id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE execution_workspaces SET last_used_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn transition_status(
        &self,
        company_id: Uuid,
        id: Uuid,
        to: WorkspaceStatus,
        cleanup_reason: Option<&str>,
    ) -> RepoResult<Option<WorkspaceRow>> {
        let mut tx = self.db.pool().begin().await?;
        let cur: WorkspaceRow = sqlx::query_as::<_, WorkspaceRow>(&format!(
            "SELECT {WS_COLS} FROM execution_workspaces              WHERE company_id=$1 AND id=$2 FOR UPDATE",
        ))
        .bind(company_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| RepoError::NotFound {
            entity: "execution_workspace",
            id: id.to_string(),
        })?;
        // 拒绝：已 terminal 的不能再转移
        let cur_status = WorkspaceStatus::parse(&cur.status);
        if let Some(prev) = cur_status {
            if prev.is_terminal() && to != prev {
                return Err(RepoError::Invalid(format!(
                    "cannot transition workspace from {prev:?} to {to:?}",
                )));
            }
        }
        let closed_at = if matches!(to, WorkspaceStatus::Closed | WorkspaceStatus::Failed) {
            Some("now()")
        } else {
            None
        };
        let sql = if let Some(closed) = closed_at {
            format!(
                "UPDATE execution_workspaces SET status=$2, closed_at={closed},                  cleanup_reason=$3, updated_at=now()                  WHERE company_id=$1 AND id=$4 RETURNING {WS_COLS}"
            )
        } else {
            format!(
                "UPDATE execution_workspaces SET status=$2, cleanup_reason=$3, updated_at=now()                  WHERE company_id=$1 AND id=$4 RETURNING {WS_COLS}"
            )
        };
        let row = sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(company_id)
            .bind(to.as_str())
            .bind(cleanup_reason)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn list_cleanup_eligible(&self, before: Timestamp) -> RepoResult<Vec<WorkspaceRow>> {
        let sql = format!(
            "SELECT {WS_COLS} FROM execution_workspaces              WHERE status='closed' AND cleanup_eligible_at IS NOT NULL                AND cleanup_eligible_at < $1 ORDER BY cleanup_eligible_at ASC LIMIT 200",
        );
        Ok(sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(before)
            .fetch_all(self.db.pool())
            .await?)
    }

    // ---- leases ----

    /// 试图原子获取一个 workspace 的 lease。
    /// 规则：
    /// * workspace 当前 status=active
    /// * 任意 active lease 未过期 → 拒绝
    pub async fn acquire_lease(&self, n: &NewLease) -> RepoResult<Option<LeaseRow>> {
        let mut tx = self.db.pool().begin().await?;
        // 1. 取 workspace FOR UPDATE
        let ws_status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM execution_workspaces              WHERE company_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(n.company_id)
        .bind(n.workspace_id)
        .fetch_optional(&mut *tx)
        .await?;
        match ws_status.as_deref() {
            Some("active") => {}
            Some(other) => {
                return Err(RepoError::Invalid(format!(
                    "workspace is not active (status={other})"
                )));
            }
            None => {
                return Err(RepoError::NotFound {
                    entity: "execution_workspace",
                    id: n.workspace_id.to_string(),
                });
            }
        }
        // 2. 检查冲突 lease
        let conflicts: Option<i64> = sqlx::query_scalar(
            "SELECT COUNT(*) FROM execution_lease              WHERE workspace_id=$1 AND state='holding' AND expires_at > now()",
        )
        .bind(n.workspace_id)
        .fetch_one(&mut *tx)
        .await?;
        if conflicts.unwrap_or(0) > 0 {
            return Ok(None); // 已被占用
        }
        // 3. 创建 lease
        let token = Uuid::new_v4().to_string();
        let sql = format!(
            "INSERT INTO execution_lease (company_id, workspace_id, agent_id, run_id,                 heartbeat_run_id, state, token, expires_at)              VALUES ($1,$2,$3,$4,$5,'holding',$6,now() + make_interval(secs => $7))              RETURNING {LEASE_COLS}",
        );
        let lease = sqlx::query_as::<_, LeaseRow>(&sql)
            .bind(n.company_id)
            .bind(n.workspace_id)
            .bind(n.agent_id)
            .bind(n.run_id)
            .bind(n.heartbeat_run_id)
            .bind(&token)
            .bind(n.ttl_secs)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(lease))
    }

    pub async fn renew_lease(
        &self,
        lease_id: Uuid,
        token: &str,
        new_ttl_secs: i64,
    ) -> RepoResult<Option<LeaseRow>> {
        let sql = format!(
            "UPDATE execution_lease SET expires_at = now() + make_interval(secs => $3),              last_renewed_at = now()              WHERE id=$1 AND token=$2 AND state='holding' AND expires_at > now()              RETURNING {LEASE_COLS}"
        );
        Ok(sqlx::query_as::<_, LeaseRow>(&sql)
            .bind(lease_id)
            .bind(token)
            .bind(new_ttl_secs)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// R803: 释放 lease (returns LeaseRow; RepoError::NotFound on miss / state mismatch).
    pub async fn release_lease(&self, lease_id: Uuid, token: &str) -> RepoResult<LeaseRow> {
        sqlx::query_as::<_, LeaseRow>(
            "UPDATE execution_lease SET state='released', released_at=now() \
             WHERE id=$1 AND token=$2 AND state='holding' \
             RETURNING id, company_id, workspace_id, agent_id, run_id, heartbeat_run_id, state, \
                token, acquired_at, expires_at, last_renewed_at, released_at, revocation_reason",
        )
        .bind(lease_id)
        .bind(token)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or_else(|| RepoError::NotFound { entity: "lease", id: lease_id.to_string() })
    }

    pub async fn revoke_lease(&self, lease_id: Uuid, reason: &str) -> RepoResult<()> {
        sqlx::query(
            "UPDATE execution_lease SET state='revoked', released_at=now(), revocation_reason=$2              WHERE id=$1",
        )
        .bind(lease_id)
        .bind(reason)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn expire_overdue(&self) -> RepoResult<u64> {
        let n = sqlx::query(
            "UPDATE execution_lease SET state='expired'              WHERE state='holding' AND expires_at <= now()",
        )
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    pub async fn active_lease_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> RepoResult<Option<LeaseRow>> {
        let sql = format!(
            "SELECT {LEASE_COLS} FROM execution_lease              WHERE workspace_id=$1 AND state='holding' AND expires_at > now()              ORDER BY acquired_at DESC LIMIT 1"
        );
        Ok(sqlx::query_as::<_, LeaseRow>(&sql)
            .bind(workspace_id)
            .fetch_optional(self.db.pool())
            .await?)
    }
}

// ---- workspace action log ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Service,
    Command,
    Reconcile,
}
impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Command => "command",
            Self::Reconcile => "reconcile",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "service" => Some(Self::Service),
            "command" => Some(Self::Command),
            "reconcile" => Some(Self::Reconcile),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
impl ActionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

const ACTION_COLS: &str = "id, workspace_id, kind, action, payload, status, error,     requested_by_user_id, requested_by_agent_id, started_at, completed_at, created_at, updated_at";

/// Round 108: workspace_operations 单行元数据，用于 `read_workspace_operation_log` 端点。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOperationMetaRow {
    pub company_id: Uuid,
    pub heartbeat_run_id: Option<Uuid>,
    pub stdout_excerpt: Option<String>,
    pub stderr_excerpt: Option<String>,
    pub log_ref: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionLogRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub action: String,
    pub payload: Value,
    pub status: String,
    pub error: Option<String>,
    pub requested_by_user_id: Option<Uuid>,
    pub requested_by_agent_id: Option<Uuid>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewActionLog {
    pub workspace_id: Uuid,
    pub kind: ActionKind,
    pub action: String,
    pub payload: Option<Value>,
    pub requested_by_user_id: Option<Uuid>,
    pub requested_by_agent_id: Option<Uuid>,
}

impl<'a> ExecutionRepo<'a> {
    /// Enqueue a workspace action. Returns the queued row.
    pub async fn enqueue_action(&self, n: &NewActionLog) -> RepoResult<ActionLogRow> {
        let sql = format!(
            "INSERT INTO workspace_action_log (workspace_id, kind, action, payload, status,                 requested_by_user_id, requested_by_agent_id)              VALUES ($1,$2,$3,$4,'queued',$5,$6) RETURNING {ACTION_COLS}",
        );
        Ok(sqlx::query_as::<_, ActionLogRow>(&sql)
            .bind(n.workspace_id)
            .bind(n.kind.as_str())
            .bind(&n.action)
            .bind(n.payload.clone().unwrap_or_else(|| serde_json::json!({})))
            .bind(n.requested_by_user_id)
            .bind(n.requested_by_agent_id)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn list_actions_for_workspace(
        &self,
        workspace_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<ActionLogRow>> {
        let sql = format!(
            "SELECT {ACTION_COLS} FROM workspace_action_log              WHERE workspace_id=$1 ORDER BY created_at DESC LIMIT $2"
        );
        Ok(sqlx::query_as::<_, ActionLogRow>(&sql)
            .bind(workspace_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn claim_next_queued_action(&self) -> RepoResult<Option<ActionLogRow>> {
        let mut tx = self.db.pool().begin().await?;
        let claimed: Option<ActionLogRow> = sqlx::query_as::<_, ActionLogRow>(&format!(
            "SELECT {ACTION_COLS} FROM workspace_action_log              WHERE status='queued' ORDER BY created_at ASC              FOR UPDATE SKIP LOCKED LIMIT 1"
        ))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = claimed else {
            return Ok(None);
        };
        let updated: ActionLogRow = sqlx::query_as::<_, ActionLogRow>(&format!(
            "UPDATE workspace_action_log SET status='running', started_at=now(), updated_at=now()              WHERE id=$1 RETURNING {ACTION_COLS}"
        ))
        .bind(row.id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(updated))
    }

    pub async fn complete_action(
        &self,
        action_id: Uuid,
        to: ActionStatus,
        error: Option<&str>,
    ) -> RepoResult<Option<ActionLogRow>> {
        let sql = format!(
            "UPDATE workspace_action_log SET status=$2, error=$3, completed_at=now(),                 updated_at=now()              WHERE id=$1 AND (status='queued' OR status='running') RETURNING {ACTION_COLS}"
        );
        Ok(sqlx::query_as::<_, ActionLogRow>(&sql)
            .bind(action_id)
            .bind(to.as_str())
            .bind(error)
            .fetch_optional(self.db.pool())
            .await?)
    }
}

// ---- runtime services ----

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeServiceRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub scope_type: String,
    pub scope_id: Option<String>,
    pub service_name: String,
    pub status: String,
    pub lifecycle: String,
    pub reuse_key: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub port: Option<i32>,
    pub url: Option<String>,
    pub provider: String,
    pub provider_ref: Option<String>,
    pub owner_agent_id: Option<Uuid>,
    pub started_by_run_id: Option<Uuid>,
    pub last_used_at: Timestamp,
    pub started_at: Timestamp,
    pub stopped_at: Option<Timestamp>,
    pub stop_policy: Option<Value>,
    pub health_status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLifecycle {
    Fresh,
    Started,
    Restarting,
    Stopped,
}
impl RuntimeLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Started => "started",
            Self::Restarting => "restarting",
            Self::Stopped => "stopped",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fresh" => Some(Self::Fresh),
            "started" => Some(Self::Started),
            "restarting" => Some(Self::Restarting),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

const RS_COLS: &str = "id, company_id, project_id, project_workspace_id, issue_id, scope_type,     scope_id, service_name, status, lifecycle, reuse_key, command, cwd, port, url, provider,     provider_ref, owner_agent_id, started_by_run_id, last_used_at, started_at, stopped_at,     stop_policy, health_status, created_at, updated_at";

impl<'a> ExecutionRepo<'a> {
    pub async fn list_runtime_services_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> RepoResult<Vec<RuntimeServiceRow>> {
        let sql = format!(
            "SELECT {RS_COLS} FROM workspace_runtime_services              WHERE scope_type='execution_workspace' AND scope_id=$1              ORDER BY last_used_at DESC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, RuntimeServiceRow>(&sql)
            .bind(workspace_id.to_string())
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_runtime_service(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<RuntimeServiceRow>> {
        let sql = format!(
            "SELECT {RS_COLS} FROM workspace_runtime_services              WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, RuntimeServiceRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn set_runtime_service_lifecycle(
        &self,
        id: Uuid,
        lifecycle: RuntimeLifecycle,
    ) -> RepoResult<Option<RuntimeServiceRow>> {
        let sql = format!(
            "UPDATE workspace_runtime_services SET lifecycle=$2, updated_at=now()              WHERE id=$1 RETURNING {RS_COLS}"
        );
        Ok(sqlx::query_as::<_, RuntimeServiceRow>(&sql)
            .bind(id)
            .bind(lifecycle.as_str())
            .fetch_optional(self.db.pool())
            .await?)
    }

    // =========================================================================
    // Round 159: execution_workspaces route 仓储化新增方法
    // =========================================================================

    /// Round 159: workspace_overview — (active_workspaces, recent_runs_24h, failed_runs_24h)。
    pub async fn overview_stats(&self, company_id: Uuid) -> sqlx::Result<(i64, i64, i64)> {
        let row: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
                (SELECT COUNT(*)::bigint FROM execution_workspaces WHERE company_id = $1 AND status = 'active'), \
                (SELECT COUNT(*)::bigint FROM heartbeat_runs WHERE company_id = $1 AND created_at > now() - interval '24 hours'), \
                (SELECT COUNT(*)::bigint FROM heartbeat_runs WHERE company_id = $1 AND status = 'failed' AND created_at > now() - interval '24 hours')",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 159: 按 id 查（不需要 company_id），用于 get_workspace 等无 tenant 上下文的端点。
    pub async fn get_by_id(&self, id: Uuid) -> RepoResult<Option<WorkspaceRow>> {
        let sql = format!("SELECT {WS_COLS} FROM execution_workspaces WHERE id = $1");
        Ok(sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Round 159: 按 id 取 company_id（acquire_lease_route 用）。
    pub async fn company_id_for_id(&self, id: Uuid) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT company_id FROM execution_workspaces WHERE id = $1")
                .bind(id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(c,)| c))
    }

    /// Round 159: UPDATE name (COALESCE) + 触 updated_at，返回 rows_affected > 0。
    pub async fn update_name(&self, id: Uuid, name: Option<&str>) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE execution_workspaces SET name = COALESCE($2, name), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 159: UPDATE status='reconciling'（runtime_service_action 用）。
    pub async fn set_status_to_reconciling(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE execution_workspaces SET status = 'reconciling', updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 159: set branch_name + provider_ref + touch last_used_at。
    pub async fn set_branch_provider_ref(
        &self,
        id: Uuid,
        branch: &str,
        provider_ref: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE execution_workspaces \
             SET branch_name = $1, provider_ref = $2, last_used_at = now(), updated_at = now() \
             WHERE id = $3",
        )
        .bind(branch)
        .bind(provider_ref)
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 159: clear provider_ref + set cleanup_reason。
    pub async fn clear_provider_ref(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE execution_workspaces \
             SET provider_ref = NULL, cleanup_reason = COALESCE(cleanup_reason, 'worktree_removed'), \
                 last_used_at = now(), updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 159: 取 workspace 最新一次 heartbeat_run (status, finished_at)。
    /// close_readiness 用。
    pub async fn latest_heartbeat_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> RepoResult<Option<(String, Option<Timestamp>)>> {
        let row: Option<(String, Option<Timestamp>)> = sqlx::query_as(
            "SELECT status, finished_at FROM heartbeat_runs \
             WHERE context_snapshot->>'executionWorkspaceId' = $1 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(workspace_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }
    /// Round 186: workspace_command_authz -- get workspace (id::text, kind) for authz summary.
    pub async fn get_id_kind(
        &self,
        workspace_id: Uuid,
    ) -> sqlx::Result<Option<(String, Option<String>)>> {
        sqlx::query_as("SELECT id::text, kind FROM execution_workspaces WHERE id = $1")
            .bind(workspace_id)
            .fetch_optional(self.db.pool())
            .await
    }
}

impl WorkspaceStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "cleaning" => Some(Self::Cleaning),
            "closed" => Some(Self::Closed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

// 让 rustc 安静（Postgres / Transaction 留作将来 actor 化）
#[allow(dead_code)]
fn _tx_marker(_: Transaction<'_, Postgres>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_status_strings() {
        assert_eq!(WorkspaceStatus::Active.as_str(), "active");
        assert_eq!(WorkspaceStatus::Closed.as_str(), "closed");
        assert_eq!(WorkspaceStatus::Cleaning.as_str(), "cleaning");
        assert_eq!(WorkspaceStatus::Failed.as_str(), "failed");
        assert!(WorkspaceStatus::Closed.is_terminal());
        assert!(WorkspaceStatus::Failed.is_terminal());
        assert!(!WorkspaceStatus::Active.is_terminal());
    }

    #[test]
    fn mode_strings() {
        assert_eq!(WorkspaceMode::LocalCheckout.as_str(), "local_checkout");
        assert_eq!(WorkspaceMode::DockerContainer.as_str(), "docker_container");
        assert_eq!(WorkspaceMode::K8sPod.as_str(), "k8s_pod");
    }

    #[test]
    fn lease_state_strings() {
        assert_eq!(LeaseState::Holding.as_str(), "holding");
        assert_eq!(LeaseState::Released.as_str(), "released");
        assert_eq!(LeaseState::Revoked.as_str(), "revoked");
    }

    #[test]
    fn new_workspace_minimum() {
        let w = NewWorkspace {
            company_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            project_workspace_id: None,
            source_issue_id: None,
            mode: WorkspaceMode::LocalCheckout,
            strategy_type: "default".into(),
            name: "ws-1".into(),
            cwd: Some("/tmp".into()),
            repo_url: None,
            base_ref: None,
            branch_name: None,
            provider_type: ProviderType::LocalFs,
            provider_ref: None,
            derived_from: None,
            metadata: None,
        };
        assert!(!w.name.trim().is_empty());
    }
}
