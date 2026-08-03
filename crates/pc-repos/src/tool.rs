//! `tool_*` 域 — Tool Gateway / 应用与连接。
//!
//! 设计原则：
//! - 严格按 paperclip schema 1:1 建模（tool_applications / tool_connections /
//!   tool_connection_health / tool_connection_versions /
//!   tool_catalog_entries / tool_action_requests / tool_audit_events）
//! - 不在仓库层写 OAuth 逻辑，认证流由 `pc-tool-gateway` 处理
//! - 所有方法都强制 `company_id` 过滤

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApplicationType {
    Mcp,
    Api,
    Cli,
    Webhook,
}
impl ToolApplicationType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Api => "api",
            Self::Cli => "cli",
            Self::Webhook => "webhook",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApplicationStatus {
    Active,
    Disabled,
    Draft,
}
impl ToolApplicationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Draft => "draft",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Stdio,
    Http,
    WebSocket,
    Internal,
}
impl ConnectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::WebSocket => "websocket",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionAuthKind {
    None,
    ApiKey,
    OAuth2,
    Basic,
    Bearer,
    McpToken,
}
impl ConnectionAuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ApiKey => "api_key",
            Self::OAuth2 => "oauth2",
            Self::Basic => "basic",
            Self::Bearer => "bearer",
            Self::McpToken => "mcp_token",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionHealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}
impl ConnectionHealthStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
        }
    }
}

const APP_COLS: &str = "id, company_id, name, slug, description, status, application_type,      version, manifest, icon_url, categories, tags,      requires_approval, max_call_rate_per_min, default_timeout_ms,      created_by_agent_id, created_by_user_id,      updated_by_agent_id, updated_by_user_id,      archived_at, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApplicationRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    pub application_type: String,
    pub version: String,
    pub manifest: Value,
    pub icon_url: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub requires_approval: bool,
    pub max_call_rate_per_min: i32,
    pub default_timeout_ms: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const CONN_COLS: &str = "id, company_id, application_id, name, kind, ownership, auth_kind,      auth_config, target_host, target_port, target_path, install_target_type, install_target_id,      status, last_used_at, last_health_at, expires_at, revision,      credential_owner_user_id, metadata,      deleted_at, created_by_agent_id, created_by_user_id,      updated_by_agent_id, updated_by_user_id,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConnectionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub application_id: Uuid,
    pub name: String,
    pub kind: String,
    pub ownership: String,
    pub auth_kind: String,
    pub auth_config: Value,
    pub target_host: Option<String>,
    pub target_port: Option<i32>,
    pub target_path: Option<String>,
    pub install_target_type: Option<String>,
    pub install_target_id: Option<String>,
    pub status: String,
    pub last_used_at: Option<Timestamp>,
    pub last_health_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
    pub revision: i32,
    pub credential_owner_user_id: Option<String>,
    pub metadata: Option<Value>,
    pub deleted_at: Option<Timestamp>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const HEALTH_COLS: &str = "id, connection_id, status, latency_ms, checked_at, message, details";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionHealthRow {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub status: String,
    pub latency_ms: Option<i32>,
    pub checked_at: Timestamp,
    pub message: Option<String>,
    pub details: Option<Value>,
}

const CATALOG_COLS: &str = "id, company_id, slug, name, description, kind, status,      application_id, distribution_visibility, metadata,      installed_connection_id, installed_at, install_target_type, install_target_id,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntryRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: String,
    pub status: String,
    pub application_id: Option<Uuid>,
    pub distribution_visibility: String,
    pub metadata: Option<Value>,
    pub installed_connection_id: Option<Uuid>,
    pub installed_at: Option<Timestamp>,
    pub install_target_type: Option<String>,
    pub install_target_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const ACTION_REQ_COLS: &str = "id, company_id, application_id, connection_id, requester_type,      requester_user_id, requester_agent_id, action_name, payload, status,      submitted_at, approved_by_user_id, decided_at, executed_at,      result_summary, error_code, error_message,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolActionRequestRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub application_id: Uuid,
    pub connection_id: Uuid,
    pub requester_type: String,
    pub requester_user_id: Option<String>,
    pub requester_agent_id: Option<Uuid>,
    pub action_name: String,
    pub payload: Value,
    pub status: String,
    pub submitted_at: Timestamp,
    pub approved_by_user_id: Option<String>,
    pub decided_at: Option<Timestamp>,
    pub executed_at: Option<Timestamp>,
    pub result_summary: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToolApplication {
    pub company_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub application_type: ToolApplicationType,
    pub version: String,
    pub manifest: Value,
    pub icon_url: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub requires_approval: bool,
    pub max_call_rate_per_min: i32,
    pub default_timeout_ms: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

pub struct ToolRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ToolRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- 1) tool_applications ----

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications              WHERE company_id=$1 AND archived_at IS NULL              ORDER BY name"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications              WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get_by_slug(
        &self,
        company_id: Uuid,
        slug: &str,
    ) -> RepoResult<Option<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications              WHERE company_id=$1 AND slug=$2"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .bind(slug)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create_application(
        &self,
        a: &NewToolApplication,
    ) -> RepoResult<ToolApplicationRow> {
        if a.name.trim().is_empty() || a.slug.trim().is_empty() {
            return Err(RepoError::Invalid("name/slug must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO tool_applications (company_id, name, slug, description, application_type,                 version, manifest, icon_url, categories, tags, requires_approval,                 max_call_rate_per_min, default_timeout_ms, created_by_agent_id, created_by_user_id)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)              RETURNING {APP_COLS}",
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(a.company_id)
            .bind(&a.name)
            .bind(&a.slug)
            .bind(a.description.as_deref())
            .bind(a.application_type.as_str())
            .bind(&a.version)
            .bind(&a.manifest)
            .bind(a.icon_url.as_deref())
            .bind(&a.categories)
            .bind(&a.tags)
            .bind(a.requires_approval)
            .bind(a.max_call_rate_per_min)
            .bind(a.default_timeout_ms)
            .bind(a.created_by_agent_id)
            .bind(a.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn set_application_status(
        &self,
        company_id: Uuid,
        id: Uuid,
        status: ToolApplicationStatus,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE tool_applications SET status=$3, updated_at=now()              WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(id)
        .bind(status.as_str())
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn archive_application(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE tool_applications SET archived_at=now(), updated_at=now()              WHERE company_id=$1 AND id=$2 AND archived_at IS NULL",
        )
        .bind(company_id)
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    // ---- 2) tool_connections ----

    pub async fn list_connections(
        &self,
        company_id: Uuid,
        application_id: Uuid,
    ) -> RepoResult<Vec<ToolConnectionRow>> {
        let sql = format!(
            "SELECT {CONN_COLS} FROM tool_connections              WHERE company_id=$1 AND application_id=$2 AND deleted_at IS NULL              ORDER BY name"
        );
        Ok(sqlx::query_as::<_, ToolConnectionRow>(&sql)
            .bind(company_id)
            .bind(application_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_connection(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<ToolConnectionRow>> {
        let sql = format!(
            "SELECT {CONN_COLS} FROM tool_connections              WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL"
        );
        Ok(sqlx::query_as::<_, ToolConnectionRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create_connection(&self, c: &ToolConnectionRow) -> RepoResult<ToolConnectionRow> {
        let sql = format!(
            "INSERT INTO tool_connections (company_id, application_id, name, kind, ownership,                 auth_kind, auth_config, target_host, target_port, target_path,                 install_target_type, install_target_id, status, revision,                 credential_owner_user_id, metadata, created_by_agent_id, created_by_user_id)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)              RETURNING {CONN_COLS}"
        );
        Ok(sqlx::query_as::<_, ToolConnectionRow>(&sql)
            .bind(c.company_id)
            .bind(c.application_id)
            .bind(&c.name)
            .bind(&c.kind)
            .bind(&c.ownership)
            .bind(&c.auth_kind)
            .bind(&c.auth_config)
            .bind(c.target_host.as_deref())
            .bind(c.target_port)
            .bind(c.target_path.as_deref())
            .bind(c.install_target_type.as_deref())
            .bind(c.install_target_id.as_deref())
            .bind(&c.status)
            .bind(c.revision)
            .bind(c.credential_owner_user_id.as_deref())
            .bind(c.metadata.clone())
            .bind(c.created_by_agent_id)
            .bind(c.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn record_connection_health(
        &self,
        connection_id: Uuid,
        status: ConnectionHealthStatus,
        latency_ms: Option<i32>,
        message: Option<&str>,
        details: Option<Value>,
    ) -> RepoResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "INSERT INTO tool_connection_health (connection_id, status, latency_ms, message, details)              VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(connection_id)
        .bind(status.as_str())
        .bind(latency_ms)
        .bind(message)
        .bind(details)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE tool_connections SET last_health_at=now() WHERE id=$1",
        )
        .bind(connection_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn latest_health(
        &self,
        connection_id: Uuid,
    ) -> RepoResult<Option<ConnectionHealthRow>> {
        let sql = format!(
            "SELECT {HEALTH_COLS} FROM tool_connection_health              WHERE connection_id=$1 ORDER BY checked_at DESC LIMIT 1"
        );
        Ok(sqlx::query_as::<_, ConnectionHealthRow>(&sql)
            .bind(connection_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn touch_used(&self, connection_id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE tool_connections SET last_used_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(connection_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    // ---- 3) tool_catalog_entries ----

    pub async fn list_catalog_for_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<CatalogEntryRow>> {
        let sql = format!(
            "SELECT {CATALOG_COLS} FROM tool_catalog_entries              WHERE company_id=$1 ORDER BY name"
        );
        Ok(sqlx::query_as::<_, CatalogEntryRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn upsert_catalog(
        &self,
        e: &CatalogEntryRow,
    ) -> RepoResult<CatalogEntryRow> {
        let sql = format!(
            "INSERT INTO tool_catalog_entries (company_id, slug, name, description, kind, status,                 application_id, distribution_visibility, metadata)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)              ON CONFLICT (company_id, slug) DO UPDATE SET                 name=EXCLUDED.name, description=EXCLUDED.description, status=EXCLUDED.status,                 application_id=EXCLUDED.application_id, metadata=EXCLUDED.metadata,                 updated_at=now()              RETURNING {CATALOG_COLS}"
        );
        Ok(sqlx::query_as::<_, CatalogEntryRow>(&sql)
            .bind(e.company_id)
            .bind(&e.slug)
            .bind(&e.name)
            .bind(e.description.as_deref())
            .bind(&e.kind)
            .bind(&e.status)
            .bind(e.application_id)
            .bind(&e.distribution_visibility)
            .bind(e.metadata.clone())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn mark_catalog_installed(
        &self,
        id: Uuid,
        connection_id: Uuid,
        target_type: &str,
        target_id: &str,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE tool_catalog_entries SET installed_connection_id=$2, installed_at=now(),              install_target_type=$3, install_target_id=$4, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(connection_id)
        .bind(target_type)
        .bind(target_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    // ---- 4) tool_action_requests ----

    pub async fn create_action_request(
        &self,
        r: &ToolActionRequestRow,
    ) -> RepoResult<ToolActionRequestRow> {
        let sql = format!(
            "INSERT INTO tool_action_requests (company_id, application_id, connection_id,                 requester_type, requester_user_id, requester_agent_id, action_name, payload,                 status, submitted_at)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)              RETURNING {ACTION_REQ_COLS}"
        );
        Ok(sqlx::query_as::<_, ToolActionRequestRow>(&sql)
            .bind(r.company_id)
            .bind(r.application_id)
            .bind(r.connection_id)
            .bind(&r.requester_type)
            .bind(r.requester_user_id.as_deref())
            .bind(r.requester_agent_id)
            .bind(&r.action_name)
            .bind(&r.payload)
            .bind(&r.status)
            .bind(r.submitted_at)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn decide_action(
        &self,
        id: Uuid,
        approve: bool,
        approver_user_id: &str,
    ) -> RepoResult<Option<ToolActionRequestRow>> {
        let status = if approve { "approved" } else { "rejected" };
        let sql = format!(
            "UPDATE tool_action_requests SET status=$2, approved_by_user_id=$3, decided_at=now(),              updated_at=now() WHERE id=$1 RETURNING {ACTION_REQ_COLS}"
        );
        Ok(sqlx::query_as::<_, ToolActionRequestRow>(&sql)
            .bind(id)
            .bind(status)
            .bind(approver_user_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn mark_action_executed(
        &self,
        id: Uuid,
        result_summary: &str,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> RepoResult<()> {
        let status = if error_code.is_some() { "failed" } else { "executed" };
        sqlx::query(
            "UPDATE tool_action_requests SET status=$2, executed_at=now(), result_summary=$3,              error_code=$4, error_message=$5, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(status)
        .bind(result_summary)
        .bind(error_code)
        .bind(error_message)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn pending_action_requests(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolActionRequestRow>> {
        let sql = format!(
            "SELECT {ACTION_REQ_COLS} FROM tool_action_requests              WHERE company_id=$1 AND status IN ('pending','submitted')              ORDER BY submitted_at ASC"
        );
        Ok(sqlx::query_as::<_, ToolActionRequestRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_type_strings() {
        assert_eq!(ToolApplicationType::Mcp.as_str(), "mcp");
        assert_eq!(ToolApplicationType::Api.as_str(), "api");
        assert_eq!(ToolApplicationType::Webhook.as_str(), "webhook");
    }
    #[test]
    fn connection_kind_strings() {
        assert_eq!(ConnectionKind::Stdio.as_str(), "stdio");
        assert_eq!(ConnectionKind::Http.as_str(), "http");
        assert_eq!(ConnectionKind::Internal.as_str(), "internal");
    }
    #[test]
    fn auth_kind_strings() {
        assert_eq!(ConnectionAuthKind::ApiKey.as_str(), "api_key");
        assert_eq!(ConnectionAuthKind::OAuth2.as_str(), "oauth2");
        assert_eq!(ConnectionAuthKind::McpToken.as_str(), "mcp_token");
    }
    #[test]
    fn health_status_strings() {
        assert_eq!(ConnectionHealthStatus::Healthy.as_str(), "healthy");
        assert_eq!(ConnectionHealthStatus::Unhealthy.as_str(), "unhealthy");
    }
    #[test]
    fn new_tool_application_minimum() {
        let a = NewToolApplication {
            company_id: Uuid::new_v4(),
            name: "Stripe".into(),
            slug: "stripe".into(),
            description: None,
            application_type: ToolApplicationType::Api,
            version: "1.0.0".into(),
            manifest: serde_json::json!({}),
            icon_url: None,
            categories: vec!["payments".into()],
            tags: vec![],
            requires_approval: true,
            max_call_rate_per_min: 60,
            default_timeout_ms: 30_000,
            created_by_agent_id: None,
            created_by_user_id: Some("u1".into()),
        };
        assert!(!a.name.trim().is_empty());
        assert_eq!(a.slug, "stripe");
    }
}
