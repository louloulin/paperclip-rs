//! `tool_*` 域 — Tool Gateway / 应用与连接。
//!
//! 设计原则：
//! - 严格按 paperclip schema 1:1 建模（tool_applications / tool_connections /
//!   tool_connection_health / tool_connection_versions /
//!   tool_catalog_entries / tool_action_requests / tool_audit_events）
//! - 不在仓库层写 OAuth 逻辑，认证流由 `pc-tool-gateway` 处理
//! - 所有方法都强制 `company_id` 过滤

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
    #[sqlx(rename = "type")]
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
        self.metadata
            .get(metadata_keys::DESCRIPTION)
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

/// 历史保留：tool_action_requests 老 schema 是审批流模型（已不在 v3 schema 中）。
/// 重命名为 LegacyToolApprovalRow 以避免和 Round 105 新引入的真实 schema 行混淆。
const LEGACY_APPROVAL_COLS: &str = "id, company_id, application_id, connection_id, requester_type,      requester_user_id, requester_agent_id, action_name, payload, status,      submitted_at, approved_by_user_id, decided_at, executed_at,      result_summary, error_code, error_message,      created_at, updated_at";

#[allow(dead_code)]
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyToolApprovalRow {
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
    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications              WHERE company_id=$1              ORDER BY created_at DESC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 142: 只列 active 状态的 application（tool_gallery 用）。
    pub async fn list_active_applications(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications              WHERE company_id=$1 AND status='active'              ORDER BY created_at DESC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 100: 按 id 全局查（不限定 company_id）。
    /// 用于纯 id-based 端点（如 `GET /api/tool-applications/:id`），调用方可用返回的
    /// `company_id` 决定是否允许后续跨公司操作。
    pub async fn get_by_id(&self, id: Uuid) -> RepoResult<Option<ToolApplicationRow>> {
        let sql = format!("SELECT {APP_COLS} FROM tool_applications WHERE id=$1");
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<ToolApplicationRow>> {
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
    pub async fn delete_application(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM tool_applications WHERE company_id=$1 AND id=$2")
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

    /// Round 142: 仅按 company_id 列出所有 connection（不限 application）。
    pub async fn list_connections_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolConnectionRow>> {
        let sql = format!(
            "SELECT {CONN_COLS} FROM tool_connections              WHERE company_id=$1              ORDER BY created_at DESC"
        );
        Ok(sqlx::query_as::<_, ToolConnectionRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 142: 按 (id, company_id) 删除 connection。
    pub async fn delete_connection_by_company(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM tool_connections WHERE id = $1 AND company_id = $2")
            .bind(id)
            .bind(company_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Round 142: 设置 connection 为 connected（status='connected', enabled=true, healthy）。
    pub async fn mark_connection_connected(&self, connection_id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE tool_connections SET status = 'connected', enabled = true, \
                health_status = 'healthy', last_health_at = now(), updated_at = now() \
             WHERE id = $1",
        )
        .bind(connection_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 142: 删除 oauth state（RETURNING company_id, connection_id）。
    pub async fn delete_oauth_state_returning(
        &self,
        state_token: &str,
    ) -> RepoResult<Option<(Uuid, Uuid)>> {
        let row: Option<(Uuid, Uuid)> = sqlx::query_as(
            "DELETE FROM tool_oauth_states WHERE state = $1 RETURNING company_id, connection_id",
        )
        .bind(state_token)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 142: 清理过期的 oauth state。
    pub async fn prune_expired_oauth_states(&self) -> RepoResult<u64> {
        Ok(
            sqlx::query("DELETE FROM tool_oauth_states WHERE expires_at < now()")
                .execute(self.db.pool())
                .await?
                .rows_affected(),
        )
    }

    /// Round 142: 完成 oauth（复合事务）：UPDATE connection status + INSERT connection_grants + INSERT oauth state。
    pub async fn complete_oauth(
        &self,
        company_id: Uuid,
        connection_id: Uuid,
        credential_refs: &Value,
        new_state: &str,
    ) -> RepoResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE tool_connections SET status = 'connected', enabled = true, \
                health_status = 'healthy', last_health_at = now(), updated_at = now() \
             WHERE id = $1 AND company_id = $2",
        )
        .bind(connection_id)
        .bind(company_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO connection_grants (company_id, connection_id, kind, status, credential_secret_refs) \
             VALUES ($1, $2, 'oauth', 'active', $3::jsonb)",
        )
        .bind(company_id)
        .bind(connection_id)
        .bind(credential_refs)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO tool_oauth_states (state, company_id, connection_id, code_verifier, expires_at) \
             VALUES ($1, $2, $3, $4, now() + interval '10 minutes') \
             ON CONFLICT (state) DO NOTHING",
        )
        .bind(new_state)
        .bind(company_id)
        .bind(connection_id)
        .bind(format!("scopes:{}", Uuid::new_v4()))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
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
        sqlx::query("UPDATE tool_connections SET last_health_at=now() WHERE id=$1")
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
        sqlx::query("UPDATE tool_connections SET last_used_at=now(), updated_at=now() WHERE id=$1")
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

    pub async fn upsert_catalog(&self, e: &CatalogEntryRow) -> RepoResult<CatalogEntryRow> {
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
        r: &LegacyToolApprovalRow,
    ) -> RepoResult<LegacyToolApprovalRow> {
        let sql = format!(
            "INSERT INTO tool_action_requests (company_id, application_id, connection_id,                 requester_type, requester_user_id, requester_agent_id, action_name, payload,                 status, submitted_at)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)              RETURNING {LEGACY_APPROVAL_COLS}"
        );
        Ok(sqlx::query_as::<_, LegacyToolApprovalRow>(&sql)
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
    ) -> RepoResult<Option<LegacyToolApprovalRow>> {
        let status = if approve { "approved" } else { "rejected" };
        let sql = format!(
            "UPDATE tool_action_requests SET status=$2, approved_by_user_id=$3, decided_at=now(),              updated_at=now() WHERE id=$1 RETURNING {LEGACY_APPROVAL_COLS}"
        );
        Ok(sqlx::query_as::<_, LegacyToolApprovalRow>(&sql)
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
        let status = if error_code.is_some() {
            "failed"
        } else {
            "executed"
        };
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
    ) -> RepoResult<Vec<LegacyToolApprovalRow>> {
        let sql = format!(
            "SELECT {LEGACY_APPROVAL_COLS} FROM tool_action_requests              WHERE company_id=$1 AND status IN ('pending','submitted')              ORDER BY submitted_at ASC"
        );
        Ok(sqlx::query_as::<_, LegacyToolApprovalRow>(&sql)
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
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM tool_profiles WHERE company_id=$1 AND profile_key=$2")
                .bind(company_id)
                .bind(profile_key)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn create_profile(&self, p: &NewToolProfile) -> RepoResult<ToolProfileRow> {
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

    pub async fn delete_profile(&self, company_id: Uuid, profile_id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM tool_profiles WHERE company_id=$1 AND id=$2")
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
        let conditions = e
            .conditions
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
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

    /// Round 141: 通过 profile_id 查找 company_id（仅取 company_id 字段）。
    pub async fn find_profile_company_id(&self, profile_id: Uuid) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT company_id FROM tool_profiles WHERE id=$1")
                .bind(profile_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(c,)| c))
    }

    /// Round 141: 通过 profile_id 取完整 profile（不限 company）。
    pub async fn find_profile_by_id(&self, profile_id: Uuid) -> RepoResult<Option<ToolProfileRow>> {
        let sql = format!("SELECT {PROFILE_COLS} FROM tool_profiles WHERE id=$1");
        Ok(sqlx::query_as::<_, ToolProfileRow>(&sql)
            .bind(profile_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Round 141: 复制 profile（含全部 entries），复合事务。
    /// 返回 (new_profile_id, company_id, source_profile_key, source_name, description, status, metadata)。
    pub async fn clone_profile(
        &self,
        source_id: Uuid,
        new_key: &str,
        new_name: &str,
    ) -> RepoResult<Uuid> {
        let mut tx = self.db.pool().begin().await?;
        let new_id: Uuid = sqlx::query_scalar(
            "INSERT INTO tool_profiles (company_id, profile_key, name, description, status, default_action, metadata) \
             SELECT company_id, $2, $3, description, status, default_action, metadata \
             FROM tool_profiles WHERE id=$1 RETURNING id",
        )
        .bind(source_id)
        .bind(new_key)
        .bind(new_name)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO tool_profile_entries \
                (company_id, profile_id, selector_type, effect, application_id, connection_id, \
                 catalog_entry_id, tool_name, risk_level, conditions) \
             SELECT company_id, $2, selector_type, effect, application_id, connection_id, \
                    catalog_entry_id, tool_name, risk_level, conditions \
             FROM tool_profile_entries WHERE profile_id=$1",
        )
        .bind(source_id)
        .bind(new_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(new_id)
    }

    /// Round 141: 批量添加 application 类型 include 效果 entries（review_tool_profile_new_tools 用）。
    pub async fn approve_new_tools_for_profile(
        &self,
        company_id: Uuid,
        profile_id: Uuid,
        app_ids: &[Uuid],
    ) -> RepoResult<u64> {
        if app_ids.is_empty() {
            return Ok(0);
        }
        let mut total: u64 = 0;
        for app_id in app_ids {
            let n = sqlx::query(
                "INSERT INTO tool_profile_entries (company_id, profile_id, selector_type, effect, application_id) \
                 VALUES ($1, $2, 'application', 'include', $3) ON CONFLICT DO NOTHING",
            )
            .bind(company_id)
            .bind(profile_id)
            .bind(app_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
            total += n;
        }
        Ok(total)
    }

    /// Round 143: 列出 profile 尚未引用的 application（list_tool_profile_new_tools 用）。
    /// 返回 application_id, key, name, risk_level。
    pub async fn list_new_tools_for_profile(
        &self,
        company_id: Uuid,
        profile_id: Uuid,
    ) -> RepoResult<Vec<(Uuid, String, Option<String>, Option<String>)>> {
        let rows: Vec<(Uuid, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT ta.id, ta.application_key, ta.display_name, ta.risk_level \
             FROM tool_applications ta \
             WHERE ta.company_id=$1 AND ta.status='active' \
             AND NOT EXISTS ( \
                SELECT 1 FROM tool_profile_entries tpe \
                WHERE tpe.profile_id=$2 AND tpe.application_id=ta.id \
             ) \
             ORDER BY ta.display_name LIMIT 100",
        )
        .bind(company_id)
        .bind(profile_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// Round 141: 通过 entry_id 查找 company_id。
    pub async fn find_profile_entry_company_id(&self, entry_id: Uuid) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT company_id FROM tool_profile_entries WHERE id=$1")
                .bind(entry_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(c,)| c))
    }

    /// Round 141: 通过 entry_id 取完整 entry。
    pub async fn get_profile_entry_by_id(
        &self,
        entry_id: Uuid,
    ) -> RepoResult<Option<ToolProfileEntryRow>> {
        let sql = format!("SELECT {PROFILE_ENTRY_COLS} FROM tool_profile_entries WHERE id=$1");
        Ok(sqlx::query_as::<_, ToolProfileEntryRow>(&sql)
            .bind(entry_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Round 141: 增量 UPDATE profile entry（COALESCE 语义）。
    pub async fn patch_profile_entry(
        &self,
        entry_id: Uuid,
        effect: Option<&str>,
        risk_level: Option<&str>,
        conditions: Option<&Value>,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE tool_profile_entries SET \
                effect=COALESCE($2, effect), \
                risk_level=COALESCE($3, risk_level), \
                conditions=COALESCE($4, conditions), \
                updated_at=now() \
             WHERE id=$1",
        )
        .bind(entry_id)
        .bind(effect)
        .bind(risk_level)
        .bind(conditions)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 141: 按 id 删除 profile entry。
    pub async fn delete_profile_entry_by_id(&self, entry_id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM tool_profile_entries WHERE id=$1")
            .bind(entry_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Round 143: 检查 profile_key 是否存在（用于 create dedup 检查）。
    pub async fn profile_key_exists(
        &self,
        company_id: Uuid,
        profile_key: &str,
    ) -> RepoResult<bool> {
        let exists: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM tool_profiles WHERE company_id = $1 AND profile_key = $2",
        )
        .bind(company_id)
        .bind(profile_key)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(exists.is_some())
    }

    /// Round 143: 创建 profile_v2（带 entries），复合事务。
    pub async fn create_profile_v2(
        &self,
        company_id: Uuid,
        profile_key: &str,
        name: &str,
        description: Option<&str>,
        status: &str,
        default_action: &str,
        metadata: &Value,
        entries: &[ToolProfileEntryInput],
    ) -> RepoResult<Uuid> {
        let mut tx = self.db.pool().begin().await?;
        let profile_id: Uuid = sqlx::query_scalar(
            "INSERT INTO tool_profiles (company_id, profile_key, name, description, status, default_action, metadata) \
             VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, '{}'::jsonb)) RETURNING id",
        )
        .bind(company_id)
        .bind(profile_key)
        .bind(name)
        .bind(description)
        .bind(status)
        .bind(default_action)
        .bind(metadata)
        .fetch_one(&mut *tx)
        .await?;
        for e in entries {
            sqlx::query(
                "INSERT INTO tool_profile_entries (company_id, profile_id, selector_type, effect, application_id, connection_id, catalog_entry_id, tool_name, risk_level, conditions) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, COALESCE($10, '{}'::jsonb))",
            )
            .bind(company_id)
            .bind(profile_id)
            .bind(&e.selector_type)
            .bind(&e.effect)
            .bind(e.application_id)
            .bind(e.connection_id)
            .bind(e.catalog_entry_id)
            .bind(e.tool_name.as_deref())
            .bind(e.risk_level.as_deref())
            .bind(&e.conditions)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(profile_id)
    }

    /// Round 143: 检查 profile 是否属于某 company（用于 bind 前的存在性检查）。
    pub async fn profile_belongs_to_company(
        &self,
        company_id: Uuid,
        profile_id: Uuid,
    ) -> RepoResult<bool> {
        let exists: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM tool_profiles WHERE company_id = $1 AND id = $2")
                .bind(company_id)
                .bind(profile_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(exists.is_some())
    }

    /// Round 143: 创建 profile binding（关联 profile 到 agent/role 等 target）。
    pub async fn create_profile_binding(
        &self,
        company_id: Uuid,
        profile_id: Uuid,
        target_type: &str,
        target_id: &str,
        priority: Option<i32>,
        metadata: &Value,
    ) -> RepoResult<Uuid> {
        let id: (Uuid,) = sqlx::query_as(
            "INSERT INTO tool_profile_bindings (company_id, profile_id, target_type, target_id, priority, metadata) \
             VALUES ($1, $2, $3, $4, COALESCE($5, 100), COALESCE($6, '{}'::jsonb)) RETURNING id",
        )
        .bind(company_id)
        .bind(profile_id)
        .bind(target_type)
        .bind(target_id)
        .bind(priority)
        .bind(metadata)
        .fetch_one(self.db.pool())
        .await?;
        Ok(id.0)
    }

    /// Round 143: 删除 profile binding。
    pub async fn delete_profile_binding(
        &self,
        company_id: Uuid,
        profile_id: Uuid,
        target_type: &str,
        target_id: &str,
    ) -> RepoResult<u64> {
        Ok(sqlx::query(
            "DELETE FROM tool_profile_bindings \
             WHERE company_id = $1 AND profile_id = $2 AND target_type = $3 AND target_id = $4",
        )
        .bind(company_id)
        .bind(profile_id)
        .bind(target_type)
        .bind(target_id)
        .execute(self.db.pool())
        .await?
        .rows_affected())
    }
}

/// Round 143: create_profile_v2 的 entry 输入 DTO（不含 company_id/profile_id）。
#[derive(Debug, Clone)]
pub struct ToolProfileEntryInput {
    pub selector_type: String,
    pub effect: String,
    pub application_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub catalog_entry_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub risk_level: Option<String>,
    pub conditions: Value,
}

const PROFILE_COLS: &str = "id, company_id, profile_key, name, description, status, default_action, metadata, created_at, updated_at";
const PROFILE_ENTRY_COLS: &str = "id, company_id, profile_id, selector_type, effect, application_id, connection_id, catalog_entry_id, tool_name, risk_level, conditions, created_at, updated_at";

// ============================================================
// Round 102: ToolRuntimeSlot 仓储层
// ============================================================
//
// 真实表 schema (0148_tool_access_mcp_connections.sql)：
//   tool_runtime_slots(
//     id, company_id, connection_id, slot_key,
//     status, provider_ref, health_status, health_message,
//     last_started_at, last_used_at, idle_deadline_at,
//     metadata,
//     created_at, updated_at
//   )
//
// **没有任何** `slot_kind / acquired_at / last_heartbeat_at` 列。
// 之前路由层 list_tool_runtime_slots 用这三个错列；同时 tool_runtime_health 用
// `last_heartbeat_at` 也会失败。下面用真实 schema 列投影。

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRuntimeSlotRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub connection_id: Uuid,
    pub slot_key: String,
    pub status: String,
    pub provider_ref: Option<String>,
    pub health_status: String,
    pub health_message: Option<String>,
    pub last_started_at: Option<Timestamp>,
    pub last_used_at: Option<Timestamp>,
    pub idle_deadline_at: Option<Timestamp>,
    pub metadata: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Round 102: tool_runtime_slots 活跃度 + 最近心跳聚合
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRuntimeHealth {
    pub company_id: Uuid,
    pub active_slots: i64,
    /// 真实 schema 中没有 `last_heartbeat_at`；这里降级为 `last_used_at`（最近活跃）。
    pub last_used_at: Option<Timestamp>,
}

impl<'a> ToolRepo<'a> {
    /// List runtime slots for a company, ordered by `last_started_at DESC` (proxy for activity).
    pub async fn list_runtime_slots_by_company(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<ToolRuntimeSlotRow>> {
        let sql = format!(
            "SELECT {RUNTIME_SLOT_COLS} FROM tool_runtime_slots              WHERE company_id=$1              ORDER BY COALESCE(last_started_at, updated_at) DESC              LIMIT $2"
        );
        Ok(sqlx::query_as::<_, ToolRuntimeSlotRow>(&sql)
            .bind(company_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_runtime_slot(
        &self,
        company_id: Uuid,
        slot_id: Uuid,
    ) -> RepoResult<Option<ToolRuntimeSlotRow>> {
        let sql = format!(
            "SELECT {RUNTIME_SLOT_COLS} FROM tool_runtime_slots              WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, ToolRuntimeSlotRow>(&sql)
            .bind(company_id)
            .bind(slot_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn runtime_health(&self, company_id: Uuid) -> RepoResult<ToolRuntimeHealth> {
        let row: (i64, Option<Timestamp>) = sqlx::query_as(
            "SELECT COUNT(*)::bigint, MAX(last_used_at)               FROM tool_runtime_slots              WHERE company_id=$1 AND status='active'",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(ToolRuntimeHealth {
            company_id,
            active_slots: row.0,
            last_used_at: row.1,
        })
    }
}

const RUNTIME_SLOT_COLS: &str = "id, company_id, connection_id, slot_key, status, provider_ref, health_status, health_message, last_started_at, last_used_at, idle_deadline_at, metadata, created_at, updated_at";

// ============================================================
// Round 103: ToolStdioTemplate 仓储层
// ============================================================
//
// 真实表 schema (0153_tool_stdio_command_templates.sql)：
//   tool_stdio_command_templates(
//     id, company_id, template_key, name, description, status, command,
//     args, env_keys, tools,            -- 三个 jsonb DEFAULT '[]'
//     created_by_agent_id, created_by_user_id,
//     disabled_at,
//     created_at, updated_at
//   )
//
// **不存在**的列：`template_id`（实为 `template_key`）/ `env_schema`（实为 args/env_keys/tools 三个 jsonb）/ `disabled_reason`
// 之前路由层的 list/create/disable 三个端点都用了错列。

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStdioTemplateRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub template_key: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub command: String,
    pub args: Value,     // jsonb '[]'
    pub env_keys: Value, // jsonb '[]'
    pub tools: Value,    // jsonb '[]'
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub disabled_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToolStdioTemplate {
    pub company_id: Uuid,
    pub template_key: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub command: String,
    #[serde(default = "default_stdio_args")]
    pub args: Value,
    #[serde(default = "default_stdio_env_keys")]
    pub env_keys: Value,
    #[serde(default = "default_stdio_tools")]
    pub tools: Value,
    #[serde(default)]
    pub created_by_agent_id: Option<Uuid>,
    #[serde(default)]
    pub created_by_user_id: Option<String>,
}

fn default_stdio_args() -> Value {
    serde_json::json!([])
}
fn default_stdio_env_keys() -> Value {
    serde_json::json!([])
}
fn default_stdio_tools() -> Value {
    serde_json::json!([])
}

impl<'a> ToolRepo<'a> {
    pub async fn list_stdio_templates_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolStdioTemplateRow>> {
        let sql = format!(
            "SELECT {STDIO_TEMPLATE_COLS} FROM tool_stdio_command_templates              WHERE company_id=$1              ORDER BY name ASC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolStdioTemplateRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// 复合冲突检测：名字重复。
    pub async fn find_stdio_template_id_by_name(
        &self,
        company_id: Uuid,
        name: &str,
    ) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM tool_stdio_command_templates WHERE company_id=$1 AND name=$2",
        )
        .bind(company_id)
        .bind(name)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn create_stdio_template(
        &self,
        t: &NewToolStdioTemplate,
    ) -> RepoResult<ToolStdioTemplateRow> {
        if t.name.trim().is_empty() {
            return Err(RepoError::Invalid("name must not be empty".into()));
        }
        if t.command.trim().is_empty() {
            return Err(RepoError::Invalid("command must not be empty".into()));
        }
        if t.template_key.trim().is_empty() {
            return Err(RepoError::Invalid("template_key must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO tool_stdio_command_templates                 (company_id, template_key, name, description, status, command, args, env_keys, tools,                  created_by_agent_id, created_by_user_id)              VALUES ($1, $2, $3, $4, 'active', $5, $6, $7, $8, $9, $10)              RETURNING {STDIO_TEMPLATE_COLS}",
        );
        Ok(sqlx::query_as::<_, ToolStdioTemplateRow>(&sql)
            .bind(t.company_id)
            .bind(&t.template_key)
            .bind(&t.name)
            .bind(t.description.as_deref())
            .bind(&t.command)
            .bind(&t.args)
            .bind(&t.env_keys)
            .bind(&t.tools)
            .bind(t.created_by_agent_id)
            .bind(t.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    /// Round 103: 禁用 stdio template. 默认按 UUID 找；template_key 兜底。
    /// `disabled_reason` 不存在，所以不写该字段。
    pub async fn disable_stdio_template(
        &self,
        company_id: Uuid,
        id_or_key: &str,
    ) -> RepoResult<bool> {
        // 先按 UUID 试
        if let Ok(uuid) = Uuid::parse_str(id_or_key) {
            let n = sqlx::query(
                "UPDATE tool_stdio_command_templates SET                      disabled_at = now(), updated_at = now()                      WHERE company_id=$1 AND id=$2 AND disabled_at IS NULL",
            )
            .bind(company_id)
            .bind(uuid)
            .execute(self.db.pool())
            .await?
            .rows_affected();
            if n > 0 {
                return Ok(true);
            }
        }
        // 再按 template_key 试
        let n = sqlx::query(
            "UPDATE tool_stdio_command_templates SET                  disabled_at = now(), updated_at = now()                  WHERE company_id=$1 AND template_key=$2 AND disabled_at IS NULL",
        )
        .bind(company_id)
        .bind(id_or_key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }
    // ---- Round 164: tool_access route 仓储化新增方法（针对 0148 tool_connections schema）----

    /// Round 164: 写入 connection_token_issuance，返回新行 id。
    pub async fn create_connection_token_issuance(
        &self,
        connection_id: Uuid,
        path: &str,
        status: &str,
        requested_at: pc_core::Timestamp,
    ) -> RepoResult<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO connection_token_issuances \
                (connection_id, path, status, requested_at) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(connection_id)
        .bind(path)
        .bind(status)
        .bind(requested_at)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }

    /// Round 164: 列出 company 的 active tool applications（0148 schema）。
    pub async fn list_active_applications_v1(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolApplicationRow>> {
        let sql = format!(
            "SELECT {APP_COLS} FROM tool_applications \
             WHERE company_id=$1 AND status='active' \
             ORDER BY created_at DESC",
        );
        Ok(sqlx::query_as::<_, ToolApplicationRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 164: 写入/upsert 一个 tool application（0148 schema，name 唯一）。
    pub async fn upsert_application(
        &self,
        company_id: Uuid,
        name: &str,
        kind: &str,
        metadata: &Value,
    ) -> RepoResult<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO tool_applications (company_id, name, type, metadata) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (company_id, name) DO UPDATE SET updated_at=now() \
             RETURNING id",
        )
        .bind(company_id)
        .bind(name)
        .bind(kind)
        .bind(metadata)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }

    /// Round 164: 创建一个 tool connection（0148 schema），返回完整列。
    pub async fn create_connection_v1(
        &self,
        company_id: Uuid,
        application_id: Uuid,
        name: &str,
        transport: &str,
        config: &Value,
        uid: &str,
    ) -> RepoResult<(
        Uuid,
        Uuid,
        Uuid,
        String,
        String,
        String,
        bool,
        Value,
        Value,
        String,
        Option<String>,
        Option<pc_core::Timestamp>,
        Option<pc_core::Timestamp>,
        pc_core::Timestamp,
        pc_core::Timestamp,
    )> {
        let row: (
            Uuid,
            Uuid,
            Uuid,
            String,
            String,
            String,
            bool,
            Value,
            Value,
            String,
            Option<String>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            pc_core::Timestamp,
            pc_core::Timestamp,
        ) = sqlx::query_as(
            "INSERT INTO tool_connections \
                (company_id, application_id, name, transport, config, uid) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id, company_id, application_id, name, transport, status, enabled, config, \
                       credential_refs, health_status, health_message, last_health_at, \
                       last_catalog_refresh_at, created_at, updated_at",
        )
        .bind(company_id)
        .bind(application_id)
        .bind(name)
        .bind(transport)
        .bind(config)
        .bind(uid)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 164: 列 connection（0148 schema，按 company_id，无 application 过滤）。
    pub async fn list_connections_v1(
        &self,
        company_id: Uuid,
    ) -> RepoResult<
        Vec<(
            Uuid,
            Uuid,
            Uuid,
            String,
            String,
            String,
            bool,
            Value,
            Value,
            String,
            Option<String>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            pc_core::Timestamp,
            pc_core::Timestamp,
        )>,
    > {
        let rows: Vec<(
            Uuid,
            Uuid,
            Uuid,
            String,
            String,
            String,
            bool,
            Value,
            Value,
            String,
            Option<String>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            pc_core::Timestamp,
            pc_core::Timestamp,
        )> = sqlx::query_as(
            "SELECT id, company_id, application_id, name, transport, status, enabled, config, \
                    credential_refs, health_status, health_message, last_health_at, \
                    last_catalog_refresh_at, created_at, updated_at \
             FROM tool_connections WHERE company_id=$1 ORDER BY created_at DESC",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// Round 164: 按 (id, company_id) 取 connection（0148 schema）。
    pub async fn get_connection_v1(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<
        Option<(
            Uuid,
            Uuid,
            Uuid,
            String,
            String,
            String,
            bool,
            Value,
            Value,
            String,
            Option<String>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            pc_core::Timestamp,
            pc_core::Timestamp,
        )>,
    > {
        let row: Option<(
            Uuid,
            Uuid,
            Uuid,
            String,
            String,
            String,
            bool,
            Value,
            Value,
            String,
            Option<String>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            pc_core::Timestamp,
            pc_core::Timestamp,
        )> = sqlx::query_as(
            "SELECT id, company_id, application_id, name, transport, status, enabled, config, \
                    credential_refs, health_status, health_message, last_health_at, \
                    last_catalog_refresh_at, created_at, updated_at \
             FROM tool_connections WHERE id=$1 AND company_id=$2",
        )
        .bind(id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }
}

const STDIO_TEMPLATE_COLS: &str = "id, company_id, template_key, name, description, status, command, args, env_keys, tools, created_by_agent_id, created_by_user_id, disabled_at, created_at, updated_at";

// ============================================================
// Round 104: ToolPolicy 仓储层
// ============================================================
//
// 真实表 schema (0149_agent_access_phase2_contracts.sql)：
//   tool_policies(
//     id, company_id, name, description, policy_type,
//     priority, enabled,
//     selectors, conditions, config,
//     created_by_agent_id, created_by_user_id,
//     created_at, updated_at
//   )
//
// **不存在**的列：`decision / scope`（list_tool_policies 之前用这俩）。

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicyRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub policy_type: String,
    pub priority: i32,
    pub enabled: bool,
    pub selectors: Value,
    pub conditions: Option<Value>,
    pub config: Option<Value>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToolPolicy {
    pub company_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub policy_type: String,
    #[serde(default = "default_policy_priority")]
    pub priority: i32,
    #[serde(default = "default_policy_enabled")]
    pub enabled: bool,
    #[serde(default = "default_policy_selectors")]
    pub selectors: Value,
    #[serde(default = "default_metadata")]
    pub conditions: Value,
    #[serde(default = "default_metadata")]
    pub config: Value,
    #[serde(default)]
    pub created_by_agent_id: Option<Uuid>,
    #[serde(default)]
    pub created_by_user_id: Option<String>,
}

fn default_policy_priority() -> i32 {
    100
}
fn default_policy_enabled() -> bool {
    true
}
fn default_policy_selectors() -> Value {
    serde_json::json!({})
}

impl<'a> ToolRepo<'a> {
    pub async fn list_policies_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolPolicyRow>> {
        let sql = format!(
            "SELECT {POLICY_COLS} FROM tool_policies              WHERE company_id=$1              ORDER BY name ASC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolPolicyRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_enabled_policies_by_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<ToolPolicyRow>> {
        let sql = format!(
            "SELECT {POLICY_COLS} FROM tool_policies              WHERE company_id=$1 AND enabled=true              ORDER BY priority ASC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolPolicyRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_policy(
        &self,
        company_id: Uuid,
        policy_id: Uuid,
    ) -> RepoResult<Option<ToolPolicyRow>> {
        let sql = format!(
            "SELECT {POLICY_COLS} FROM tool_policies              WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, ToolPolicyRow>(&sql)
            .bind(company_id)
            .bind(policy_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn find_policy_id_by_name(
        &self,
        company_id: Uuid,
        name: &str,
    ) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM tool_policies WHERE company_id=$1 AND name=$2")
                .bind(company_id)
                .bind(name)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn create_policy(&self, p: &NewToolPolicy) -> RepoResult<ToolPolicyRow> {
        if p.name.trim().is_empty() {
            return Err(RepoError::Invalid("policy name must not be empty".into()));
        }
        if p.policy_type.trim().is_empty() {
            return Err(RepoError::Invalid("policy_type must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO tool_policies                 (company_id, name, description, policy_type, priority, enabled,                  selectors, conditions, config, created_by_agent_id, created_by_user_id)              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)              RETURNING {POLICY_COLS}",
        );
        Ok(sqlx::query_as::<_, ToolPolicyRow>(&sql)
            .bind(p.company_id)
            .bind(&p.name)
            .bind(p.description.as_deref())
            .bind(&p.policy_type)
            .bind(p.priority)
            .bind(p.enabled)
            .bind(&p.selectors)
            .bind(&p.conditions)
            .bind(&p.config)
            .bind(p.created_by_agent_id)
            .bind(p.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn delete_policy(&self, company_id: Uuid, policy_id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM tool_policies WHERE company_id=$1 AND id=$2")
            .bind(company_id)
            .bind(policy_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Round 104: 重排策略优先级，按 policy_ids 顺序分配 priority = i * step。
    /// 全部写入必须在同一事务中。
    pub async fn reorder_policies(
        &self,
        company_id: Uuid,
        policy_ids: &[Uuid],
        step: i32,
    ) -> RepoResult<u64> {
        if policy_ids.is_empty() {
            return Ok(0);
        }
        let mut tx = self.db.pool().begin().await?;
        let mut total: u64 = 0;
        for (i, pid) in policy_ids.iter().enumerate() {
            let new_priority = (i as i32) * step;
            let n = sqlx::query(
                "UPDATE tool_policies SET priority=$1, updated_at=now()                  WHERE company_id=$2 AND id=$3",
            )
            .bind(new_priority)
            .bind(company_id)
            .bind(pid)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            total += n;
        }
        tx.commit().await?;
        Ok(total)
    }

    /// Round 141: 模糊查询 name 排除自身（patch 时检查 name 冲突用）。
    pub async fn find_policy_id_by_name_excluding(
        &self,
        company_id: Uuid,
        name: &str,
        exclude_id: Uuid,
    ) -> RepoResult<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM tool_policies WHERE company_id=$1 AND name=$2 AND id <> $3",
        )
        .bind(company_id)
        .bind(name)
        .bind(exclude_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    /// Round 141: 增量 UPDATE（COALESCE 语义：None = 不动）；返回受影响行数。
    pub async fn patch_policy(
        &self,
        company_id: Uuid,
        policy_id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        priority: Option<i32>,
        enabled: Option<bool>,
        selectors: Option<&Value>,
        conditions: Option<&Value>,
        config: Option<&Value>,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE tool_policies SET \
                name = COALESCE($1, name), \
                description = COALESCE($2, description), \
                priority = COALESCE($3, priority), \
                enabled = COALESCE($4, enabled), \
                selectors = COALESCE($5, selectors), \
                conditions = COALESCE($6, conditions), \
                config = COALESCE($7, config), \
                updated_at = now() \
             WHERE company_id = $8 AND id = $9",
        )
        .bind(name)
        .bind(description)
        .bind(priority)
        .bind(enabled)
        .bind(selectors)
        .bind(conditions)
        .bind(config)
        .bind(company_id)
        .bind(policy_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 141: 列出 trust 类型规则（policy_type='trust' OR 包含 trustRuleKey 选择器）。
    pub async fn list_trust_rules(&self, company_id: Uuid) -> RepoResult<Vec<ToolPolicyRow>> {
        let sql = format!(
            "SELECT {POLICY_COLS} FROM tool_policies \
             WHERE company_id=$1 \
             AND (policy_type = 'trust' OR policy_type = 'tool_trust_rule' OR selectors ? 'trustRuleKey') \
             ORDER BY updated_at DESC LIMIT 200"
        );
        Ok(sqlx::query_as::<_, ToolPolicyRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 141: 检查 policy 是否属于 trust 规则类型。
    pub async fn is_trust_rule(&self, company_id: Uuid, policy_id: Uuid) -> RepoResult<bool> {
        let exists: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM tool_policies \
             WHERE company_id = $1 AND id = $2 \
             AND (policy_type = 'trust' OR policy_type = 'tool_trust_rule')",
        )
        .bind(company_id)
        .bind(policy_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(exists.is_some())
    }

    /// Round 141: 撤销 trust rule（设置 enabled=false + 在 config 记录 revokedAt/revokeReason）。
    pub async fn revoke_trust_rule(
        &self,
        company_id: Uuid,
        policy_id: Uuid,
        reason: Option<&str>,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE tool_policies SET enabled = false, \
                config = COALESCE(config, '{{}}'::jsonb) || jsonb_build_object('revokedAt', to_jsonb(now()), 'revokeReason', to_jsonb($1::text)), \
                updated_at = now() \
             WHERE company_id = $2 AND id = $3",
        )
        .bind(reason.unwrap_or(""))
        .bind(company_id)
        .bind(policy_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 141: 读取 action_request 派生 trust rule 选择器所需的字段。
    pub async fn find_action_request_for_trust_rule(
        &self,
        company_id: Uuid,
        action_request_id: Uuid,
    ) -> RepoResult<Option<ActionRequestTrustFields>> {
        let row: Option<(Value, Option<Uuid>, Option<Uuid>, Option<String>)> = sqlx::query_as(
            "SELECT canonical_arguments_summary, application_id, connection_id, tool_name \
             FROM tool_action_requests WHERE company_id = $1 AND id = $2",
        )
        .bind(company_id)
        .bind(action_request_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(
            row.map(|(summary, application_id, connection_id, tool_name)| {
                ActionRequestTrustFields {
                    summary,
                    application_id,
                    connection_id,
                    tool_name,
                }
            }),
        )
    }
}

// Round 141: trust rule 派生选择器需要的 action_request 字段。
#[derive(Debug, Clone)]
pub struct ActionRequestTrustFields {
    pub summary: Value,
    pub application_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub tool_name: Option<String>,
}

const POLICY_COLS: &str = "id, company_id, name, description, policy_type, priority, enabled, selectors, conditions, config, created_by_agent_id, created_by_user_id, created_at, updated_at";

// ============================================================
// Round 105: ToolActionRequest 仓储层
// ============================================================
//
// 真实表 schema (0149_agent_access_phase2_contracts.sql)：
//   tool_action_requests(
//     id, company_id, invocation_id, issue_id, interaction_id, approval_id,
//     status, canonical_arguments_hash, canonical_arguments_summary,
//     signed_arguments, preview_markdown,
//     requested_by_agent_id, requested_by_user_id,
//     resolved_by_agent_id, resolved_by_user_id,
//     decided_by_agent_id, decided_by_user_id,
//     decided_at, expires_at, resolved_at,
//     created_at, updated_at
//   )
//
// **不存在**的列：`action_kind / requested_by / payload`
// 之前 list_tool_action_requests 用这 3 个错列。

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolActionRequestRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub invocation_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub interaction_id: Option<Uuid>,
    pub approval_id: Option<Uuid>,
    pub status: String,
    pub canonical_arguments_hash: String,
    pub canonical_arguments_summary: Value,
    pub signed_arguments: Option<String>,
    pub preview_markdown: Option<String>,
    pub requested_by_agent_id: Option<Uuid>,
    pub requested_by_user_id: Option<String>,
    pub resolved_by_agent_id: Option<Uuid>,
    pub resolved_by_user_id: Option<String>,
    pub decided_by_agent_id: Option<Uuid>,
    pub decided_by_user_id: Option<String>,
    pub decided_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
    pub resolved_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Round 146: install_example 复合事务的返回结构。
#[derive(Debug, Clone)]
pub struct InstallExampleResult {
    pub application_id: Uuid,
    pub connection_id: Uuid,
    pub profile_id: Uuid,
    pub profile_entries: usize,
}

/// Round 145: tool_invocations 1:1 schema 投影（供 list_invocations / create_invocation 返回）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvocationSummaryRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub agent_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub catalog_entry_id: Option<Uuid>,
    pub tool_name: String,
    pub status: String,
    pub result_summary: Option<Value>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

impl<'a> ToolRepo<'a> {
    /// Round 144: 列出某 company 的 active tool categories（distinct risk_level）。
    pub async fn list_tool_categories(&self, company_id: Uuid) -> RepoResult<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT risk_level FROM tool_catalog_entries \
             WHERE company_id = $1 AND status = 'active' ORDER BY risk_level",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows.into_iter().map(|(r,)| r).collect())
    }

    /// Round 144: 设置 catalog entry 状态为 quarantined（delete_tool 用）。
    pub async fn quarantine_catalog_entry(&self, entry_id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("UPDATE tool_catalog_entries SET status = 'quarantined' WHERE id = $1")
            .bind(entry_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Round 144: 创建 oauth state（upsert_oauth_state 用）。
    pub async fn upsert_oauth_state(
        &self,
        company_id: Uuid,
        connection_id: Uuid,
        state_token: &str,
        code_verifier: &str,
    ) -> RepoResult<()> {
        // Best-effort delete of expired rows.
        sqlx::query("DELETE FROM tool_oauth_states WHERE expires_at < now()")
            .execute(self.db.pool())
            .await?;
        sqlx::query(
            "INSERT INTO tool_oauth_states (state, company_id, connection_id, code_verifier, expires_at) \
             VALUES ($1, $2, $3, $4, now() + interval '10 minutes') \
             ON CONFLICT (state) DO NOTHING",
        )
        .bind(state_token)
        .bind(company_id)
        .bind(connection_id)
        .bind(code_verifier)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 145: tool_catalog_entries 动态查询（tool_lookup 用）。
    /// has_q / has_risk / has_cid 决定 WHERE 子句与绑定参数。
    pub async fn lookup_catalog_entries(
        &self,
        company_id: Uuid,
        q: Option<&str>,
        risk_level: Option<&str>,
        connection_id: Option<Uuid>,
    ) -> RepoResult<
        Vec<(
            Uuid,
            Uuid,
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Value,
            String,
            String,
            Timestamp,
        )>,
    > {
        let has_q = q.map(|s| !s.is_empty()).unwrap_or(false);
        let has_risk = risk_level.is_some();
        let has_cid = connection_id.is_some();
        let mut sql = String::from(
            "SELECT id, company_id, connection_id, name, title, description, \
             input_schema, risk_level, status, created_at \
             FROM tool_catalog_entries WHERE company_id = $1 AND status = 'active'",
        );
        let like_pat = q
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{s}%"))
            .unwrap_or_default();
        let risk_val = risk_level.unwrap_or("");
        let cid = connection_id.unwrap_or_else(Uuid::nil);
        if has_q {
            sql.push_str(" AND (name ILIKE $2 OR title ILIKE $2 OR description ILIKE $2)");
        }
        if has_risk {
            sql.push_str(&format!(" AND risk_level = ${}", 2 + i32::from(has_q)));
        }
        if has_cid {
            sql.push_str(&format!(
                " AND connection_id = ${}",
                2 + i32::from(has_q) + i32::from(has_risk)
            ));
        }
        sql.push_str(" ORDER BY name LIMIT 100");
        let rows: Vec<(
            Uuid,
            Uuid,
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Value,
            String,
            String,
            Timestamp,
        )> = sqlx::query_as(&sql)
            .bind(company_id)
            .bind(&like_pat)
            .bind(risk_val)
            .bind(cid)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// Round 145: 按 id 查 active catalog entry（get_tool 用）。
    pub async fn get_active_catalog_entry(
        &self,
        entry_id: Uuid,
    ) -> RepoResult<
        Option<(
            Uuid,
            Uuid,
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Value,
            String,
            String,
            Timestamp,
        )>,
    > {
        let row: Option<(
            Uuid,
            Uuid,
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Value,
            String,
            String,
            Timestamp,
        )> = sqlx::query_as(
            "SELECT id, company_id, connection_id, name, title, description, \
             input_schema, risk_level, status, created_at \
             FROM tool_catalog_entries WHERE id = $1 AND status = 'active'",
        )
        .bind(entry_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 145: invoke_tool 取 catalog entry（按 company + id + active）。
    pub async fn find_active_catalog_entry_by_company(
        &self,
        company_id: Uuid,
        entry_id: Uuid,
    ) -> RepoResult<
        Option<(
            Uuid,
            Uuid,
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Value,
            String,
            String,
            Timestamp,
        )>,
    > {
        let row: Option<(
            Uuid,
            Uuid,
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Value,
            String,
            String,
            Timestamp,
        )> = sqlx::query_as(
            "SELECT id, company_id, connection_id, name, title, description, \
             input_schema, risk_level, status, created_at \
             FROM tool_catalog_entries \
             WHERE id = $1 AND company_id = $2 AND status = 'active'",
        )
        .bind(entry_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 145: 创建 tool_invocation（invoke_tool 用）。
    pub async fn create_invocation(
        &self,
        company_id: Uuid,
        connection_id: Uuid,
        catalog_entry_id: Uuid,
        tool_name: &str,
        arguments_summary: Option<&Value>,
    ) -> RepoResult<InvocationSummaryRow> {
        let row: InvocationSummaryRow = sqlx::query_as(
            "INSERT INTO tool_invocations \
             (company_id, actor_type, connection_id, catalog_entry_id, tool_name, \
              arguments_summary, status, started_at) \
             VALUES ($1, 'user', $2, $3, $4, $5, 'pending', now()) \
             RETURNING id, company_id, actor_type, actor_id, agent_id, issue_id, run_id, \
                       connection_id, catalog_entry_id, tool_name, status, result_summary, \
                       error_code, error_message, started_at, completed_at, created_at",
        )
        .bind(company_id)
        .bind(connection_id)
        .bind(catalog_entry_id)
        .bind(tool_name)
        .bind(arguments_summary)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 145: list_invocations（按 company + optional connection filter）。
    pub async fn list_invocations(
        &self,
        company_id: Uuid,
        connection_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> RepoResult<Vec<InvocationSummaryRow>> {
        let rows: Vec<InvocationSummaryRow> = if let Some(cid) = connection_id {
            sqlx::query_as(
                "SELECT id, company_id, actor_type, actor_id, agent_id, issue_id, run_id, \
                 connection_id, catalog_entry_id, tool_name, status, result_summary, \
                 error_code, error_message, started_at, completed_at, created_at \
                 FROM tool_invocations \
                 WHERE company_id = $1 AND connection_id = $2 \
                 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
            )
            .bind(company_id)
            .bind(cid)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.db.pool())
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, company_id, actor_type, actor_id, agent_id, issue_id, run_id, \
                 connection_id, catalog_entry_id, tool_name, status, result_summary, \
                 error_code, error_message, started_at, completed_at, created_at \
                 FROM tool_invocations \
                 WHERE company_id = $1 \
                 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(company_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.db.pool())
            .await?
        };
        Ok(rows)
    }

    /// Round 146: 列出某 company 的 enabled tool_policies（policy_test_route 用）。
    /// 返回 (id, name, priority, enabled, selectors)
    pub async fn list_enabled_policies_for_test(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<(Uuid, String, i32, bool, Value)>> {
        let rows: Vec<(Uuid, String, i32, bool, Value)> = sqlx::query_as(
            "SELECT id, name, priority, enabled, selectors \
             FROM tool_policies WHERE company_id = $1 AND enabled = true \
             ORDER BY priority ASC LIMIT 50",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// Round 146: 插入一条 policy_decision audit event（policy_test_route 用）。
    pub async fn insert_policy_decision_event(
        &self,
        company_id: Uuid,
        actor_type: Option<&str>,
        actor_id: Option<&str>,
        agent_id: Option<Uuid>,
        application_id: Option<Uuid>,
        connection_id: Option<Uuid>,
        catalog_entry_id: Option<Uuid>,
        tool_name: Option<&str>,
        decision: &str,
        matched_policy_ids: &Value,
        reason_code: &str,
        arguments_summary: &Value,
    ) -> RepoResult<Option<Uuid>> {
        let event_id: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO tool_call_events \
             (company_id, event_type, actor_type, actor_id, agent_id, application_id, connection_id, catalog_entry_id, tool_name, decision, matched_policy_ids, reason_code, outcome, arguments_summary) \
             VALUES ($1, 'policy_decision', COALESCE($2, 'system'), $3, $4, $5, $6, $7, $8, $9, $10::jsonb, $11, 'pending', $12) RETURNING id",
        )
        .bind(company_id)
        .bind(actor_type)
        .bind(actor_id)
        .bind(agent_id)
        .bind(application_id)
        .bind(connection_id)
        .bind(catalog_entry_id)
        .bind(tool_name)
        .bind(decision)
        .bind(matched_policy_ids)
        .bind(reason_code)
        .bind(arguments_summary)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(event_id)
    }

    /// Round 147: 列出 agent 的有效 profile bindings（含 company-level 默认）。
    /// 返回 (binding_id, target_type, profile_id, profile_key, name, priority)
    pub async fn list_effective_profile_bindings_for_agent(
        &self,
        company_id: Uuid,
        agent_id: &str,
    ) -> RepoResult<Vec<(Uuid, String, Uuid, String, String, i32)>> {
        let rows: Vec<(Uuid, String, Uuid, String, String, i32)> = sqlx::query_as(
            "SELECT b.id, b.target_type, b.profile_id, p.profile_key, p.name, b.priority \
             FROM tool_profile_bindings b JOIN tool_profiles p ON p.id = b.profile_id \
             WHERE b.company_id = $1 AND (b.target_type = 'agent' AND b.target_id = $2 \
                OR b.target_type = 'company' AND b.target_id = $1::text)",
        )
        .bind(company_id)
        .bind(agent_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// Round 146: 列出需要关注的 tool_connections（disabled 或 unhealthy）。
    pub async fn list_apps_attention(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<(Uuid, String, String, bool, String, Option<String>)>> {
        let rows: Vec<(Uuid, String, String, bool, String, Option<String>)> = sqlx::query_as(
            "SELECT id, name, transport, enabled, health_status, health_message \
             FROM tool_connections WHERE company_id = $1 \
                AND (enabled = false OR health_status IN ('unhealthy','stale','unknown')) \
             ORDER BY updated_at DESC LIMIT 100",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// Round 146: 列出某 connection 的 connection_grants（list_connection_grants 用）。
    pub async fn list_connection_grants(
        &self,
        connection_id: Uuid,
    ) -> RepoResult<Vec<(Uuid, Uuid, String, Option<String>, Option<String>, String)>> {
        let rows: Vec<(Uuid, Uuid, String, Option<String>, Option<String>, String)> =
            sqlx::query_as(
                "SELECT id, connection_id, kind, subject_user_id, status, created_at::text \
             FROM connection_grants WHERE connection_id = $1 ORDER BY created_at DESC LIMIT 50",
            )
            .bind(connection_id)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// Round 146: 安装 example（复合事务：INSERT application + INSERT connection + INSERT profile + INSERT entries）。
    pub async fn install_example(
        &self,
        company_id: Uuid,
        example_id: &str,
        name: &str,
        kind: &str,
        description: Option<&str>,
        example_config: &Value,
        tools: &[String],
    ) -> RepoResult<InstallExampleResult> {
        let mut tx = self.db.pool().begin().await?;
        let application_id: Uuid = sqlx::query_scalar(
            "INSERT INTO tool_applications (company_id, name, kind, description, config) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (company_id, name) DO UPDATE SET updated_at = now() \
             RETURNING id",
        )
        .bind(company_id)
        .bind(name)
        .bind(kind)
        .bind(description)
        .bind(example_config)
        .fetch_one(&mut *tx)
        .await?;
        let uid = format!("tc_{}", Uuid::new_v4().simple());
        let connection_id: (Uuid,) = sqlx::query_as(
            "INSERT INTO tool_connections (company_id, application_id, name, transport, status, enabled, uid) \
             VALUES ($1, $2, $3, 'stdio', 'pending', false, $4) RETURNING id",
        )
        .bind(company_id)
        .bind(application_id)
        .bind(name)
        .bind(&uid)
        .fetch_one(&mut *tx)
        .await?;
        let profile_key = format!("prof-from-example-{example_id}");
        let profile_id: (Uuid,) = sqlx::query_as(
            "INSERT INTO tool_profiles (company_id, profile_key, name, description, status, default_action, metadata) \
             VALUES ($1, $2, $3, $4, 'active', 'ask', $5) RETURNING id",
        )
        .bind(company_id)
        .bind(&profile_key)
        .bind(format!("Profile for {name}"))
        .bind(description)
        .bind(json!({ "sourceExampleId": example_id }))
        .fetch_one(&mut *tx)
        .await?;
        let mut entries_count: usize = 0;
        for tool_name in tools {
            if tool_name.is_empty() {
                continue;
            }
            sqlx::query(
                "INSERT INTO tool_profile_entries (company_id, profile_id, selector_type, effect, application_id, connection_id, tool_name) \
                 VALUES ($1, $2, 'tool', 'include', $3, $4, $5)",
            )
            .bind(company_id)
            .bind(profile_id.0)
            .bind(application_id)
            .bind(connection_id.0)
            .bind(tool_name)
            .execute(&mut *tx)
            .await?;
            entries_count += 1;
        }
        tx.commit().await?;
        Ok(InstallExampleResult {
            application_id,
            connection_id: connection_id.0,
            profile_id: profile_id.0,
            profile_entries: entries_count,
        })
    }

    /// Round 143: 列出某 run 的 tool_call_events（get_run_decisions_route 用）。
    /// 返回 (id, event_type, tool_name, decision, reason_code, arguments_summary, matched_policy_ids, created_at)
    pub async fn list_tool_call_events_for_run(
        &self,
        company_id: Uuid,
        run_id: Uuid,
    ) -> RepoResult<
        Vec<(
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<Value>,
            Value,
            Option<Timestamp>,
        )>,
    > {
        let rows: Vec<(
            Uuid, String, Option<String>, Option<String>, Option<String>,
            Option<Value>, Value, Option<Timestamp>,
        )> = sqlx::query_as(
            "SELECT id, event_type, tool_name, decision, reason_code, arguments_summary, matched_policy_ids, created_at \
             FROM tool_call_events WHERE company_id = $1 AND run_id = $2 \
             ORDER BY created_at DESC LIMIT 200",
        )
        .bind(company_id)
        .bind(run_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    pub async fn list_action_requests_by_company(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<ToolActionRequestRow>> {
        let sql = format!(
            "SELECT {ACTION_REQ_COLS_V3} FROM tool_action_requests              WHERE company_id=$1              ORDER BY created_at DESC LIMIT $2"
        );
        Ok(sqlx::query_as::<_, ToolActionRequestRow>(&sql)
            .bind(company_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_action_request(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<ToolActionRequestRow>> {
        let sql = format!(
            "SELECT {ACTION_REQ_COLS_V3} FROM tool_action_requests              WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, ToolActionRequestRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn list_action_requests_by_invocation(
        &self,
        invocation_id: Uuid,
    ) -> RepoResult<Vec<ToolActionRequestRow>> {
        let sql = format!(
            "SELECT {ACTION_REQ_COLS_V3} FROM tool_action_requests              WHERE invocation_id=$1              ORDER BY created_at DESC LIMIT 100"
        );
        Ok(sqlx::query_as::<_, ToolActionRequestRow>(&sql)
            .bind(invocation_id)
            .fetch_all(self.db.pool())
            .await?)
    }
}

const ACTION_REQ_COLS_V3: &str = "id, company_id, invocation_id, issue_id, interaction_id, approval_id, status, canonical_arguments_hash, canonical_arguments_summary, signed_arguments, preview_markdown, requested_by_agent_id, requested_by_user_id, resolved_by_agent_id, resolved_by_user_id, decided_by_agent_id, decided_by_user_id, decided_at, expires_at, resolved_at, created_at, updated_at";

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

    // ---- Round 102: ToolRuntimeHealth + 列投影基本字段 ----

    #[test]
    fn runtime_health_payload_fields() {
        // 验证结构能被序列化（camelCase 路径）
        let h = ToolRuntimeHealth {
            company_id: Uuid::new_v4(),
            active_slots: 5,
            last_used_at: None,
        };
        let json = serde_json::to_value(&h).unwrap();
        assert_eq!(json["activeSlots"], 5);
        assert!(json["lastUsedAt"].is_null());
        assert!(json["companyId"].is_string());
    }

    #[test]
    fn runtime_slot_col_includes_real_columns_only() {
        // 真实 schema 没有 slot_kind/acquired_at/last_heartbeat_at
        assert!(!RUNTIME_SLOT_COLS.contains("slot_kind"));
        assert!(!RUNTIME_SLOT_COLS.contains("acquired_at"));
        assert!(!RUNTIME_SLOT_COLS.contains("last_heartbeat_at"));
        // 必须包含真实列
        assert!(RUNTIME_SLOT_COLS.contains("slot_key"));
        assert!(RUNTIME_SLOT_COLS.contains("last_started_at"));
        assert!(RUNTIME_SLOT_COLS.contains("last_used_at"));
        assert!(RUNTIME_SLOT_COLS.contains("health_status"));
    }

    // ---- Round 103: ToolStdioTemplate ----

    #[test]
    fn new_stdio_template_defaults_have_empty_json_arrays() {
        let t = NewToolStdioTemplate {
            company_id: Uuid::new_v4(),
            template_key: "k".into(),
            name: "n".into(),
            description: None,
            command: "echo".into(),
            args: default_stdio_args(),
            env_keys: default_stdio_env_keys(),
            tools: default_stdio_tools(),
            created_by_agent_id: None,
            created_by_user_id: None,
        };
        assert_eq!(t.args, serde_json::json!([]));
        assert_eq!(t.env_keys, serde_json::json!([]));
        assert_eq!(t.tools, serde_json::json!([]));
    }

    #[test]
    fn stdio_template_col_excludes_wrong_columns() {
        // 真实 schema 没有 template_id (应是 template_key)、env_schema、disabled_reason
        assert!(!STDIO_TEMPLATE_COLS.contains("template_id "));
        assert!(!STDIO_TEMPLATE_COLS.contains("env_schema"));
        assert!(!STDIO_TEMPLATE_COLS.contains("disabled_reason"));
        // 必须包含真实列
        assert!(STDIO_TEMPLATE_COLS.contains("template_key"));
        assert!(STDIO_TEMPLATE_COLS.contains("args"));
        assert!(STDIO_TEMPLATE_COLS.contains("env_keys"));
        assert!(STDIO_TEMPLATE_COLS.contains("tools"));
        assert!(STDIO_TEMPLATE_COLS.contains("status"));
    }

    // ---- Round 104: ToolPolicy ----

    #[test]
    fn new_tool_policy_defaults() {
        let p = NewToolPolicy {
            company_id: Uuid::new_v4(),
            name: "n".into(),
            description: None,
            policy_type: "scoped".into(),
            priority: default_policy_priority(),
            enabled: default_policy_enabled(),
            selectors: default_policy_selectors(),
            conditions: default_metadata(),
            config: default_metadata(),
            created_by_agent_id: None,
            created_by_user_id: None,
        };
        assert_eq!(p.priority, 100);
        assert!(p.enabled);
        assert_eq!(p.selectors, serde_json::json!({}));
    }

    #[test]
    fn policy_col_excludes_decision_scope() {
        // 真实 schema 没有 decision / scope
        assert!(!POLICY_COLS.contains("decision"));
        assert!(!POLICY_COLS.contains("scope"));
        // 必须包含真实列
        assert!(POLICY_COLS.contains("policy_type"));
        assert!(POLICY_COLS.contains("priority"));
        assert!(POLICY_COLS.contains("enabled"));
        assert!(POLICY_COLS.contains("selectors"));
        assert!(POLICY_COLS.contains("conditions"));
        assert!(POLICY_COLS.contains("config"));
    }

    // ---- Round 105: ToolActionRequest ----

    #[test]
    fn action_request_col_excludes_wrong_columns() {
        // 检查每个以逗号/空格分隔的 token 是否属于错列集合。
        let cols: Vec<&str> = ACTION_REQ_COLS_V3
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .collect();
        let wrong = [
            "action_kind",
            "requested_by",
            "payload",
            "application_id",
            "connection_id",
            "action_name",
        ];
        for c in &cols {
            assert!(!wrong.contains(c), "schema leak: forbidden col {c}");
        }
        // 必须包含真实列
        for must in [
            "invocation_id",
            "canonical_arguments_hash",
            "canonical_arguments_summary",
            "requested_by_agent_id",
            "requested_by_user_id",
            "decided_at",
        ] {
            assert!(cols.contains(&must), "missing col: {must}");
        }
    }

    #[test]
    fn action_request_row_has_minimal_required_fields() {
        let r = ToolActionRequestRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            invocation_id: Uuid::new_v4(),
            issue_id: None,
            interaction_id: None,
            approval_id: None,
            status: "pending".into(),
            canonical_arguments_hash: "abc123".into(),
            canonical_arguments_summary: serde_json::json!({"first": 1}),
            signed_arguments: None,
            preview_markdown: None,
            requested_by_agent_id: None,
            requested_by_user_id: None,
            resolved_by_agent_id: None,
            resolved_by_user_id: None,
            decided_by_agent_id: None,
            decided_by_user_id: None,
            decided_at: None,
            expires_at: None,
            resolved_at: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        assert_eq!(r.status, "pending");
        assert_eq!(r.canonical_arguments_hash, "abc123");
    }

    // ---- Round 757: ToolApplicationRow kind->type column mapping & DB row projection ----

    /// 验证 ToolApplicationRow.kind 字段必须有 #[sqlx(rename = "type")]，
    /// 否则 sqlx FromRow 在投影 type 列时会 ColumnNotFound("kind")。
    #[test]
    fn r757_tool_application_row_kind_uses_db_type_column() {
        // Source review: struct field 'kind' 必须带 #[sqlx(rename = "type")],
        // 因为 DB 列是 type（SQL 关键字），serde rename 不影响 sqlx FromRow。
        let src = include_str!("tool.rs");
        assert!(
            src.contains("#[sqlx(rename = \"type\")]"),
            "ToolApplicationRow.kind must carry #[sqlx(rename = \"type\")] so sqlx FromRow can project from DB column type"
        );
    }

    /// 验证 ToolApplicationRow.description() 从 metadata.description 取值。
    #[test]
    fn r757_tool_application_row_description_from_metadata() {
        let row = ToolApplicationRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            name: "R757 desc-test".into(),
            kind: "mcp".into(),
            status: "active".into(),
            metadata: serde_json::json!({"description": "R757 description helper"}),
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        assert_eq!(row.description(), Some("R757 description helper"));
    }

    /// 验证 ToolApplicationRow.config() 从 metadata.config 取值。
    #[test]
    fn r757_tool_application_row_config_from_metadata() {
        let row = ToolApplicationRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            name: "R757 config-test".into(),
            kind: "mcp".into(),
            status: "active".into(),
            metadata: serde_json::json!({"config": {"endpoint": "https://x", "timeout": 30}}),
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        let cfg = row.config();
        assert_eq!(cfg["endpoint"], "https://x");
        assert_eq!(cfg["timeout"], 30);
    }

    /// 验证 metadata 缺失 description/config 时的默认行为。
    #[test]
    fn r757_tool_application_row_missing_metadata_keys() {
        let row = ToolApplicationRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            name: "R757 empty-meta".into(),
            kind: "mcp".into(),
            status: "active".into(),
            metadata: serde_json::json!({}),
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        assert_eq!(row.description(), None);
        assert_eq!(row.config(), serde_json::json!({}));
    }

    /// 验证 PatchToolApplication.metadata_patch() 合并顺序：description 覆盖 -> config 替换 -> metadata_merge 增量。
    #[test]
    fn r757_patch_tool_application_metadata_patch_order() {
        let p = PatchToolApplication {
            name: None,
            description: Some("desc-first".into()),
            config: Some(serde_json::json!({"k": "v"})),
            status: None,
            metadata_merge: serde_json::Map::from_iter([
                ("extra".to_string(), serde_json::json!("merge-value")),
            ]),
        };
        let m = p.metadata_patch();
        assert_eq!(m["description"], "desc-first");
        assert_eq!(m["config"]["k"], "v");
        assert_eq!(m["extra"], "merge-value");
    }
}
