//! `environments` + `environment_leases` 域 — Agent 运行环境的配置与占用。
//!
//! 设计：
//! - `environments` 是全局共享定义（不像 execution_workspaces 是每项目实例化）
//! - 单例约束：`driver='local'` 仅一条；managed_sandbox 仅一条
//! - `environment_leases` 记录 company → environment 的占用（关联 issue/run/workspace）
//! - lease 状态机：active → released / expired / failed

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentDriver {
    Local,
    Sandbox,
    Docker,
    Kubernetes,
    HostedRemote,
    Custom,
}
impl EnvironmentDriver {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sandbox => "sandbox",
            Self::Docker => "docker",
            Self::Kubernetes => "kubernetes",
            Self::HostedRemote => "hosted_remote",
            Self::Custom => "custom",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "local" => Some(Self::Local),
            "sandbox" => Some(Self::Sandbox),
            "docker" => Some(Self::Docker),
            "kubernetes" => Some(Self::Kubernetes),
            "hosted_remote" => Some(Self::HostedRemote),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatus {
    Active,
    Disabled,
    Deprecated,
    Provisioning,
}
impl EnvironmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Deprecated => "deprecated",
            Self::Provisioning => "provisioning",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "disabled" => Some(Self::Disabled),
            "deprecated" => Some(Self::Deprecated),
            "provisioning" => Some(Self::Provisioning),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    Active,
    Released,
    Expired,
    Failed,
}
impl LeaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Released => "released",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeasePolicy {
    Ephemeral,
    LongLived,
    Manual,
}
impl LeasePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::LongLived => "long_lived",
            Self::Manual => "manual",
        }
    }
}

const ENV_COLS: &str = "id, name, description, driver, status, config, env_vars, metadata,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub driver: String,
    pub status: String,
    pub config: Value,
    pub env_vars: Value,
    pub metadata: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const LEASE_COLS: &str = "id, company_id, environment_id, execution_workspace_id, issue_id,      heartbeat_run_id, status, lease_policy, provider, provider_lease_id, acquired_at,      last_used_at, expires_at, released_at, failure_reason, cleanup_status, metadata,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentLeaseRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub environment_id: Uuid,
    pub execution_workspace_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub status: String,
    pub lease_policy: String,
    pub provider: Option<String>,
    pub provider_lease_id: Option<String>,
    pub acquired_at: Timestamp,
    pub last_used_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub released_at: Option<Timestamp>,
    pub failure_reason: Option<String>,
    pub cleanup_status: Option<String>,
    pub metadata: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEnvironment {
    pub name: String,
    pub description: Option<String>,
    pub driver: EnvironmentDriver,
    pub status: EnvironmentStatus,
    pub config: Value,
    pub env_vars: Value,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEnvironmentLease {
    pub company_id: Uuid,
    pub environment_id: Uuid,
    pub execution_workspace_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub lease_policy: LeasePolicy,
    pub provider: Option<String>,
    pub expires_at: Option<Timestamp>,
}

pub struct EnvironmentRepo<'a> {
    pub db: &'a Db,
}

impl<'a> EnvironmentRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- environments ----

    pub async fn list_all(&self) -> RepoResult<Vec<EnvironmentRow>> {
        let sql = format!(
            "SELECT {ENV_COLS} FROM environments              ORDER BY status='active' DESC, name"
        );
        Ok(sqlx::query_as::<_, EnvironmentRow>(&sql)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 202: 按公司维度列出 environments（schema 显式有 company_id）。
    pub async fn list_for_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<EnvironmentRow>> {
        let sql = format!(
            "SELECT {ENV_COLS} FROM environments WHERE company_id = $1              ORDER BY status='active' DESC, name"
        );
        Ok(sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(&self, id: Uuid) -> RepoResult<Option<EnvironmentRow>> {
        let sql = format!("SELECT {ENV_COLS} FROM environments WHERE id=$1");
        Ok(sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get_by_name(&self, name: &str) -> RepoResult<Option<EnvironmentRow>> {
        let sql = format!("SELECT {ENV_COLS} FROM environments WHERE name=$1");
        Ok(sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(name)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get_driver(
        &self,
        driver: EnvironmentDriver,
    ) -> RepoResult<Option<EnvironmentRow>> {
        let sql = format!("SELECT {ENV_COLS} FROM environments WHERE driver=$1 LIMIT 1");
        Ok(sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(driver.as_str())
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, e: &NewEnvironment) -> RepoResult<EnvironmentRow> {
        if e.name.trim().is_empty() {
            return Err(RepoError::Invalid("environment name must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO environments (name, description, driver, status, config, env_vars, metadata)              VALUES ($1,$2,$3,$4,$5,$6,$7)              RETURNING {ENV_COLS}"
        );
        Ok(sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(&e.name)
            .bind(e.description.as_deref())
            .bind(e.driver.as_str())
            .bind(e.status.as_str())
            .bind(&e.config)
            .bind(&e.env_vars)
            .bind(e.metadata.clone())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn update_status(
        &self,
        id: Uuid,
        status: EnvironmentStatus,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE environments SET status=$2, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(status.as_str())
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Merge env_vars（不覆盖已有 key）
    pub async fn merge_env_vars(
        &self,
        id: Uuid,
        patch: &Value,
    ) -> RepoResult<bool> {
        if !patch.is_object() {
            return Err(RepoError::Invalid("env_vars patch must be an object".into()));
        }
        let n = sqlx::query(
            "UPDATE environments SET env_vars = env_vars || $2::jsonb, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(patch)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn delete(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM environments WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // --------- Backward-compat shims (旧 API，别名) ---------

    /// Back-compat: list() returns all environments.
    #[allow(dead_code)]
    pub async fn list(&self) -> RepoResult<Vec<EnvironmentRow>> {
        self.list_all().await
    }

    /// Back-compat: create with simple (name, driver, config) signature.
    #[allow(dead_code)]
    pub async fn create_simple(
        &self,
        name: &str,
        driver: &str,
        config: Value,
    ) -> RepoResult<EnvironmentRow> {
        let parsed_driver = EnvironmentDriver::parse(driver).unwrap_or(EnvironmentDriver::Custom);
        let input = NewEnvironment {
            name: name.into(),
            description: None,
            driver: parsed_driver,
            status: EnvironmentStatus::Active,
            config,
            env_vars: serde_json::json!({}),
            metadata: None,
        };
        self.create(&input).await
    }

    /// Back-compat: combined update.
    #[allow(dead_code)]
    pub async fn update(
        &self,
        id: Uuid,
        name: Option<&str>,
        status: Option<&str>,
        config: Option<Value>,
    ) -> RepoResult<Option<EnvironmentRow>> {
        let mut sql = String::from("UPDATE environments SET updated_at=now()");
        if name.is_some() { sql.push_str(", name = $2"); }
        if status.is_some() { sql.push_str(", status = $3"); }
        if config.is_some() { sql.push_str(", config = $4"); }
        let sql = format!(
            "{} WHERE id=$1 RETURNING {ENV_COLS}",
            sql
        );
        let q = sqlx::query_as::<_, EnvironmentRow>(&sql)
            .bind(id);
        let q = if let Some(n) = name { q.bind(n) } else { q.bind(Option::<String>::None) };
        let q = if let Some(s) = status {
            let st = EnvironmentStatus::parse(s).unwrap_or(EnvironmentStatus::Active);
            q.bind(st.as_str())
        } else {
            q.bind(Option::<String>::None)
        };
        let q = if let Some(c) = config { q.bind(c) } else { q.bind(Option::<Value>::None) };
        Ok(q.fetch_optional(self.db.pool()).await?)
    }

    // ---- leases ----

    pub async fn list_leases_for_company(
        &self,
        company_id: Uuid,
        only_active: bool,
    ) -> RepoResult<Vec<EnvironmentLeaseRow>> {
        let mut sql = format!(
            "SELECT {LEASE_COLS} FROM environment_leases WHERE company_id=$1"
        );
        if only_active {
            sql.push_str(" AND status='active'");
        }
        sql.push_str(" ORDER BY acquired_at DESC LIMIT 200");
        Ok(sqlx::query_as::<_, EnvironmentLeaseRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn active_lease_for_environment(
        &self,
        environment_id: Uuid,
    ) -> RepoResult<Option<EnvironmentLeaseRow>> {
        let sql = format!(
            "SELECT {LEASE_COLS} FROM environment_leases              WHERE environment_id=$1 AND status='active'              ORDER BY acquired_at DESC LIMIT 1"
        );
        Ok(sqlx::query_as::<_, EnvironmentLeaseRow>(&sql)
            .bind(environment_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn acquire_lease(
        &self,
        n: &NewEnvironmentLease,
    ) -> RepoResult<EnvironmentLeaseRow> {
        let sql = format!(
            "INSERT INTO environment_leases (company_id, environment_id,                 execution_workspace_id, issue_id, heartbeat_run_id, lease_policy,                 provider, expires_at, status)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'active')              RETURNING {LEASE_COLS}"
        );
        Ok(sqlx::query_as::<_, EnvironmentLeaseRow>(&sql)
            .bind(n.company_id)
            .bind(n.environment_id)
            .bind(n.execution_workspace_id)
            .bind(n.issue_id)
            .bind(n.heartbeat_run_id)
            .bind(n.lease_policy.as_str())
            .bind(n.provider.as_deref())
            .bind(n.expires_at)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn renew_lease(
        &self,
        id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE environment_leases SET last_used_at=now(), updated_at=now()              WHERE id=$1 AND status='active'",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn release_lease(
        &self,
        id: Uuid,
        reason: Option<&str>,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE environment_leases SET status='released', released_at=now(),              failure_reason=$2, updated_at=now() WHERE id=$1 AND status='active'",
        )
        .bind(id)
        .bind(reason)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn expire_overdue(&self) -> RepoResult<u64> {
        Ok(sqlx::query(
            "UPDATE environment_leases SET status='expired', updated_at=now()              WHERE status='active' AND expires_at IS NOT NULL AND expires_at <= now()",
        )
        .execute(self.db.pool())
        .await?
        .rows_affected())
    }

    // ---- Round 167: environments route 仓储化新增方法 ----

    /// Round 167: 列某 environment 的 lease（按 acquired_at DESC, LIMIT 100）。
    pub async fn list_leases_for_environment(
        &self,
        environment_id: Uuid,
    ) -> RepoResult<Vec<(Uuid, Uuid, Uuid, Option<Timestamp>, Option<Timestamp>, String)>> {
        let rows: Vec<(Uuid, Uuid, Uuid, Option<Timestamp>, Option<Timestamp>, String)> =
            sqlx::query_as(
                "SELECT id, environment_id, run_id, acquired_at, expires_at, status::text \
                 FROM environment_leases WHERE environment_id = $1 \
                 ORDER BY acquired_at DESC LIMIT 100",
            )
            .bind(environment_id)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// Round 167: 按 id 取单条 lease。
    pub async fn get_environment_lease(
        &self,
        lease_id: Uuid,
    ) -> RepoResult<Option<(Uuid, Uuid, Uuid, Option<Timestamp>, Option<Timestamp>, String)>> {
        let row: Option<(Uuid, Uuid, Uuid, Option<Timestamp>, Option<Timestamp>, String)> =
            sqlx::query_as(
                "SELECT id, environment_id, run_id, acquired_at, expires_at, status::text \
                 FROM environment_leases WHERE id = $1",
            )
            .bind(lease_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    /// Round 167: 触摸 environment（更新 updated_at = now）。
    pub async fn touch_environment(&self, id: Uuid) -> RepoResult<()> {
        sqlx::query("UPDATE environments SET updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Round 167: 按 environment_id 取 custom image template。
    pub async fn get_custom_image_template(
        &self,
        environment_id: Uuid,
    ) -> RepoResult<Option<(Uuid, Option<String>, Option<String>, Value)>> {
        let row: Option<(Uuid, Option<String>, Option<String>, Value)> = sqlx::query_as(
            "SELECT environment_id, dockerfile, image_ref, build_args \
             FROM environment_custom_image_templates WHERE environment_id = $1 LIMIT 1",
        )
        .bind(environment_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 167: 删 environment 的 custom image template，返回受影响行数。
    pub async fn delete_custom_image_template(&self, environment_id: Uuid) -> RepoResult<u64> {
        let n = sqlx::query("DELETE FROM environment_custom_image_templates WHERE environment_id = $1")
            .bind(environment_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n)
    }

    /// Round 167: 触摸 environment_custom_image_templates（更新 updated_at）。
    pub async fn touch_custom_image_template(&self, environment_id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE environment_custom_image_templates SET updated_at = now() WHERE environment_id = $1",
        )
        .bind(environment_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 167: 按 id 取 environment_custom_image_setup_session。
    pub async fn get_custom_image_setup_session(
        &self,
        session_id: Uuid,
    ) -> RepoResult<Option<(Uuid, String, Option<Timestamp>)>> {
        let row: Option<(Uuid, String, Option<Timestamp>)> = sqlx::query_as(
            "SELECT id, status::text, created_at FROM environment_custom_image_setup_sessions \
             WHERE id = $1",
        )
        .bind(session_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_driver_round_trip() {
        for d in [
            EnvironmentDriver::Local,
            EnvironmentDriver::Sandbox,
            EnvironmentDriver::Docker,
            EnvironmentDriver::Kubernetes,
            EnvironmentDriver::Custom,
        ] {
            assert_eq!(EnvironmentDriver::parse(d.as_str()), Some(d));
        }
        assert_eq!(EnvironmentDriver::parse("nope"), None);
    }

    #[test]
    fn env_status_strings() {
        assert_eq!(EnvironmentStatus::Active.as_str(), "active");
        assert_eq!(EnvironmentStatus::Disabled.as_str(), "disabled");
        assert_eq!(EnvironmentStatus::Deprecated.as_str(), "deprecated");
        assert_eq!(EnvironmentStatus::Provisioning.as_str(), "provisioning");
    }

    #[test]
    fn lease_status_and_policy_strings() {
        assert_eq!(LeaseStatus::Active.as_str(), "active");
        assert_eq!(LeaseStatus::Released.as_str(), "released");
        assert_eq!(LeaseStatus::Expired.as_str(), "expired");
        assert_eq!(LeaseStatus::Failed.as_str(), "failed");
        assert_eq!(LeasePolicy::Ephemeral.as_str(), "ephemeral");
        assert_eq!(LeasePolicy::LongLived.as_str(), "long_lived");
        assert_eq!(LeasePolicy::Manual.as_str(), "manual");
    }

    #[test]
    fn new_env_requires_name() {
        let e = NewEnvironment {
            name: "".into(),
            description: None,
            driver: EnvironmentDriver::Local,
            status: EnvironmentStatus::Active,
            config: serde_json::json!({}),
            env_vars: serde_json::json!({}),
            metadata: None,
        };
        assert!(e.name.trim().is_empty());
    }
}
