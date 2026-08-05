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

/// 历史保留：原 schema 假设的 type 枚举。当前 schema 的 `type` 列无 CHECK 约束，
/// 仅作 str helper 使用。**保留该 enum 以让旧调用者**仍然获得 `mcp/api/cli/webhook`
/// 这些字面量；仓储本身不再强约束。
#[allow(dead_code)]
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

/// 历史保留：tool_applications.status 实际 DEFAULT 'active'，无 CHECK 约束；
/// 这里保留 enum 仅用作 caller 端的字符串 helper。
#[allow(dead_code)]
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

// Round 100: 1:1 对齐真实 tool_applications 表 schema（0148 migration）
// 真实列：id, company_id, name, type, status, metadata, created_at, updated_at
const APP_COLS: &str = "id, company_id, name, type, status, metadata, created_at, updated_at";

/// 1:1 投影真实 `tool_applications` 表 schema（Round 100）。
/// 详见 `pc_repos::tool::APP_COLS`。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApplicationRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    /// 投影列名：DB 是 `type`（关键字），响应里保留 `kind` 以兼容现有 API。
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    /// JSONB 列：内部含 description + config 等元数据。
    pub metadata: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 在 tool_applications.metadata jsonb 内的常用字段键。
pub mod metadata_keys {
    pub const DESCRIPTION: &str = "description";
    pub const CONFIG: &str = "config";
}

impl ToolApplicationRow {
    pub fn description(&self) -> Option<&str> {
        self.metadata.get(metadata_keys::DESCRIPTION)
            .and_then(Value::as_str)
    }
    pub fn config(&self) -> Value {
        self.metadata
            .get(metadata_keys::CONFIG)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}))
    }
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

/// 创建 `tool_applications` 的最小写入 payload（Round 100）。
/// 注意：description + config 在这里被合并成 jsonb `metadata`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToolApplication {
    pub company_id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

fn default_metadata() -> Value {
    serde_json::json!({})
}

impl NewToolApplication {
    /// 把 description 嵌入 metadata，构造最终入库的 jsonb。
    pub fn effective_metadata(&self) -> Value {
        let mut m = self.metadata.clone();
        if let serde_json::Value::Object(ref mut map) = m {
            if let Some(d) = &self.description {
                map.insert(metadata_keys::DESCRIPTION.into(), serde_json::json!(d));
            }
        }
        m
    }
}

/// `tool_applications` 部分更新 payload（Round 100）。
/// 任意字段为 None 表示保持原值；description/config 走 metadata jsonb 合并。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchToolApplication {
    pub name: Option<String>,
    pub description: Option<String>,
    pub config: Option<Value>,
    pub status: Option<String>,
    #[serde(default)]
    pub metadata_merge: serde_json::Map<String, Value>,
}

impl PatchToolApplication {
    /// 构造给 `metadata = metadata || $patch::jsonb` 的 jsonb 合并 patch。
    /// 包含：顶层 description（新值则覆盖）、config（整个替换）、metadata_merge 自定义键。
    pub fn metadata_patch(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(d) = &self.description {
            m.insert(metadata_keys::DESCRIPTION.into(), serde_json::json!(d));
        }
        if let Some(c) = &self.config {
            m.insert(metadata_keys::CONFIG.into(), c.clone());
        }
        for (k, v) in &self.metadata_merge {
            m.insert(k.clone(), v.clone());
        }
        serde_json::Value::Object(m)
    }
    pub fn is_noop(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.config.is_none()
            && self.status.is_none()
            && self.metadata_merge.is_empty()
    }
}

pub struct ToolRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ToolRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- 1) tool_applications ----

    /// Round 100: list_by_company 对齐真实 schema，去掉 archived_at IS NULL 过滤。
    /// 返回的 row 是已 1:1 投影 DB 行的 ToolApplicationRow。
    pub async fn list_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications              WHERE company_id=$1              ORDER BY created_at DESC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

/// Round 100: 按 id 全局查（不限定 company_id）。
    /// 用于纯 id-based 端点（如 `GET /api/tool-applications/:id`），调用方可用返回的
    /// `company_id` 决定是否允许后续跨公司操作。
    pub async fn get_by_id(
        &self,
        id: Uuid,
    ) -> RepoResult<Option<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications WHERE id=$1"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
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

    /// 兼容旧 API：`name` 当作伪 slug 查（slug 列已不存在）。
    pub async fn get_by_name(
        &self,
        company_id: Uuid,
        name: &str,
    ) -> RepoResult<Option<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications              WHERE company_id=$1 AND name=$2"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .bind(name)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Round 100: INSERT 只写真实列 `(company_id, name, type, metadata)`；
    /// status 用 DEFAULT 'active'，created_at/updated_at 由 DEFAULT now() 维护。
    /// description 被嵌入 metadata jsonb。
    pub async fn create_application(
        &self,
        a: &NewToolApplication,
    ) -> RepoResult<ToolApplicationRow> {
        if a.name.trim().is_empty() {
            return Err(RepoError::Invalid("name must not be empty".into()));
        }
        if a.kind.trim().is_empty() {
            return Err(RepoError::Invalid("kind must not be empty".into()));
        }
        let metadata = a.effective_metadata();
        let sql = format!(
            "INSERT INTO tool_applications (company_id, name, type, metadata)              VALUES ($1, $2, $3, $4)              RETURNING {APP_COLS}",
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(a.company_id)
            .bind(&a.name)
            .bind(&a.kind)
            .bind(&metadata)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn set_application_status(
        &self,
        company_id: Uuid,
        id: Uuid,
        status: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE tool_applications SET status=$1, updated_at=now()              WHERE company_id=$2 AND id=$3",
        )
        .bind(status)
        .bind(company_id)
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 100: 已删除 archived_at 列。patch_application 才是真正的"删除"语义。
    pub async fn delete_application(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "DELETE FROM tool_applications WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 100: 真正的部分更新入口。
    /// - `name`/`status`: 直接覆盖
    /// - `description`/`config`/`metadata_merge`: 通过 `metadata = metadata || $patch::jsonb` 合并
    pub async fn patch_application(
        &self,
        company_id: Uuid,
        id: Uuid,
        p: &PatchToolApplication,
    ) -> RepoResult<bool> {
        if p.is_noop() {
            // 即使 PATCH 是空 payload，仍然更新 updated_at 保证单调递增，避免业务方反复死循环。
            let n = sqlx::query(
                "UPDATE tool_applications SET updated_at=now() WHERE company_id=$1 AND id=$2",
            )
            .bind(company_id)
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
            return Ok(n > 0);
        }
        let meta_patch = p.metadata_patch();
        let n = sqlx::query(
            "UPDATE tool_applications SET                 name = COALESCE($1, name),                 status = COALESCE($2, status),                 metadata = metadata || $3::jsonb,                 updated_at = now()               WHERE company_id=$4 AND id=$5",
        )
        .bind(p.name.as_deref())
        .bind(p.status.as_deref())
        .bind(&meta_patch)
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

// ============================================================
// Round 101: ToolProfile 仓储层
// ============================================================
//
// 真实表 schema (0149_agent_access_phase2_contracts.sql):
//   tool_profiles(
//     id, company_id, profile_key, name, description,
//     status, default_action, metadata,
//     created_at, updated_at
//   )
//   tool_profile_entries(
//     id, company_id, profile_id, selector_type, effect,
//     application_id, connection_id, catalog_entry_id,
//     tool_name, risk_level, conditions,
//     created_at, updated_at
//   )
//
// 之前路由层 list_tool_profiles 用 `kind / scope / updated_at` 是错的列名；list 改用
// 真实列，并平级映射回 Node API 期望的 JSON 形状（保留 kind/scope 兼容老客户端）。

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProfileRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub profile_key: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub default_action: String,
    pub metadata: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProfileEntryRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub profile_id: Uuid,
    pub selector_type: String,
    pub effect: String,
    pub application_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub catalog_entry_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub risk_level: Option<String>,
    pub conditions: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `tool_profiles` 写入 payload。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToolProfile {
    pub company_id: Uuid,
    pub profile_key: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_profile_status")]
    pub status: String,
    #[serde(default = "default_profile_action")]
    pub default_action: String,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

fn default_profile_status() -> String {
    "active".to_string()
}
fn default_profile_action() -> String {
    "deny".to_string()
}

/// `tool_profile_entries` 写入 payload。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToolProfileEntry {
    pub company_id: Uuid,
    pub profile_id: Uuid,
    pub selector_type: String,
    #[serde(default = "default_entry_effect")]
    pub effect: String,
    pub application_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub catalog_entry_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub risk_level: Option<String>,
    #[serde(default)]
    pub conditions: Option<Value>,
}

fn default_entry_effect() -> String {
    "include".to_string()
}

impl<'a> ToolRepo<'a> {
    // ---- 3a) tool_profiles ----

    pub async fn list_profiles_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolProfileRow>> {
        let sql = format!(
            "SELECT {PROFILE_COLS} FROM tool_profiles              WHERE company_id=$1              ORDER BY updated_at DESC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolProfileRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_profile(
        &self,
        company_id: Uuid,
        profile_id: Uuid,
    ) -> RepoResult<Option<ToolProfileRow>> {
        let sql = format!(
            "SELECT {PROFILE_COLS} FROM tool_profiles              WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, ToolProfileRow>(&sql)
            .bind(company_id)
            .bind(profile_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// 在 (company_id, profile_key) 上做幂等检查；存在则返回 Some(id)。
    pub async fn find_profile_id_by_key(
        &self,
        company_id: Uuid,
        profile_key: &str,
    ) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM tool_profiles WHERE company_id=$1 AND profile_key=$2",
        )
        .bind(company_id)
        .bind(profile_key)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn create_profile(
        &self,
        p: &NewToolProfile,
    ) -> RepoResult<ToolProfileRow> {
        if p.name.trim().is_empty() {
            return Err(RepoError::Invalid("profile name must not be empty".into()));
        }
        if p.profile_key.trim().is_empty() {
            return Err(RepoError::Invalid("profile_key must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO tool_profiles                 (company_id, profile_key, name, description, status, default_action, metadata)              VALUES ($1, $2, $3, $4, $5, $6, $7)              RETURNING {PROFILE_COLS}",
        );
        Ok(sqlx::query_as::<_, ToolProfileRow>(&sql)
            .bind(p.company_id)
            .bind(&p.profile_key)
            .bind(&p.name)
            .bind(p.description.as_deref())
            .bind(&p.status)
            .bind(&p.default_action)
            .bind(&p.metadata)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn delete_profile(
        &self,
        company_id: Uuid,
        profile_id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "DELETE FROM tool_profiles WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(profile_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    // ---- 3b) tool_profile_entries ----

    pub async fn list_profile_entries(
        &self,
        profile_id: Uuid,
    ) -> RepoResult<Vec<ToolProfileEntryRow>> {
        let sql = format!(
            "SELECT {PROFILE_ENTRY_COLS} FROM tool_profile_entries              WHERE profile_id=$1              ORDER BY created_at ASC LIMIT 1000"
        );
        Ok(sqlx::query_as::<_, ToolProfileEntryRow>(&sql)
            .bind(profile_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn create_profile_entry(
        &self,
        e: &NewToolProfileEntry,
    ) -> RepoResult<ToolProfileEntryRow> {
        if e.selector_type.trim().is_empty() {
            return Err(RepoError::Invalid("selector_type must not be empty".into()));
        }
        let conditions = e.conditions.clone().unwrap_or_else(|| serde_json::json!({}));
        let sql = format!(
            "INSERT INTO tool_profile_entries                 (company_id, profile_id, selector_type, effect, application_id, connection_id,                  catalog_entry_id, tool_name, risk_level, conditions)              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)              RETURNING {PROFILE_ENTRY_COLS}",
        );
        Ok(sqlx::query_as::<_, ToolProfileEntryRow>(&sql)
            .bind(e.company_id)
            .bind(e.profile_id)
            .bind(&e.selector_type)
            .bind(&e.effect)
            .bind(e.application_id)
            .bind(e.connection_id)
            .bind(e.catalog_entry_id)
            .bind(e.tool_name.as_deref())
            .bind(e.risk_level.as_deref())
            .bind(&conditions)
            .fetch_one(self.db.pool())
            .await?)
    }
}

const PROFILE_COLS: &str = "id, company_id, profile_key, name, description, status, default_action, metadata, created_at, updated_at";
const PROFILE_ENTRY_COLS: &str = "id, company_id, profile_id, selector_type, effect, application_id, connection_id, catalog_entry_id, tool_name, risk_level, conditions, created_at, updated_at";


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
            kind: "api".into(),
            description: Some("Stripe payment integration".into()),
            metadata: serde_json::json!({}),
        };
        assert!(!a.name.trim().is_empty());
        assert_eq!(a.kind, "api");
        // description 被合并进 metadata jsonb
        let m = a.effective_metadata();
        assert_eq!(m["description"], "Stripe payment integration");
    }

    #[test]
    fn patch_tool_application_patch_key_construction() {
        let p = PatchToolApplication {
            name: Some("NewName".into()),
            description: Some("new desc".into()),
            config: Some(serde_json::json!({"endpoint": "https://x"})),
            status: Some("disabled".into()),
            metadata_merge: serde_json::Map::new(),
        };
        let m = p.metadata_patch();
        assert_eq!(m["description"], "new desc");
        assert_eq!(m["config"]["endpoint"], "https://x");

        // no-op patch
        let empty = PatchToolApplication::default();
        assert!(empty.is_noop());
        let p2 = PatchToolApplication {
            description: Some("x".into()),
            ..PatchToolApplication::default()
        };
        assert!(!p2.is_noop());
    }

    #[test]
    fn tool_application_row_metadata_helpers() {
        let row = ToolApplicationRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            name: "x".into(),
            kind: "mcp".into(),
            status: "active".into(),
            metadata: serde_json::json!({"description": "desc", "config": {"k": 1}}),
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        assert_eq!(row.description(), Some("desc"));
        assert_eq!(row.config()["k"], 1);
    }

    // ---- Round 101: ToolProfile + ToolProfileEntry ----

    #[test]
    fn new_tool_profile_defaults() {
        let p = NewToolProfile {
            company_id: Uuid::new_v4(),
            profile_key: "k".into(),
            name: "n".into(),
            description: None,
            status: default_profile_status(),
            default_action: default_profile_action(),
            metadata: serde_json::json!({}),
        };
        assert_eq!(p.status, "active");
        assert_eq!(p.default_action, "deny");
    }

    #[test]
    fn new_tool_profile_entry_defaults() {
        let e = NewToolProfileEntry {
            company_id: Uuid::new_v4(),
            profile_id: Uuid::new_v4(),
            selector_type: "tool_name".into(),
            effect: default_entry_effect(),
            application_id: None,
            connection_id: None,
            catalog_entry_id: None,
            tool_name: Some("x".into()),
            risk_level: None,
            conditions: None,
        };
        assert_eq!(e.effect, "include");
    }
}
