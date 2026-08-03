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

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<WorkspaceRow>> {
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

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<WorkspaceRow>> {
        let sql = format!(
            "SELECT {WS_COLS} FROM execution_workspaces              WHERE company_id=$1 AND id=$2",
        );
        Ok(sqlx::query_as::<_, WorkspaceRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, w: &NewWorkspace) -> RepoResult<WorkspaceRow> {
        if w.name.trim().is_empty() {
            return Err(RepoError::Invalid("workspace name must not be empty".into()));
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

    pub async fn list_cleanup_eligible(
        &self,
        before: Timestamp,
    ) -> RepoResult<Vec<WorkspaceRow>> {
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

    pub async fn release_lease(
        &self,
        lease_id: Uuid,
        token: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE execution_lease SET state='released', released_at=now()              WHERE id=$1 AND token=$2 AND state='holding'",
        )
        .bind(lease_id)
        .bind(token)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn revoke_lease(
        &self,
        lease_id: Uuid,
        reason: &str,
    ) -> RepoResult<()> {
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
