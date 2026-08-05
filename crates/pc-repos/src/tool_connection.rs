//! `tool_connections` + `tool_catalog_entries` + `tool_connection_installs` +
//! `tool_connection_grants` + `tool_connection_test_calls` 仓储。
//!
//! 设计：
//! - tool_connections 是公司下 MCP server 连接的核心表（transport + enabled + config + health）
//! - catalog / installs / grants / test_calls 都是围绕 connection_id 的子表
//! - 所有方法都按 `connection_id` 范围隔离（不跨公司）
//!
//! Round 154: 从 `routes/tool_connections.rs` 抽出 SQL，提供仓储方法。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

/// 单条 tool connection 的完整 DB 行投影（1:1 映射 `tool_connections`）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ToolConnectionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub application_id: Uuid,
    pub name: String,
    pub transport: String,
    pub status: String,
    pub enabled: bool,
    pub config: serde_json::Value,
    pub credential_refs: serde_json::Value,
    pub health_status: String,
    pub health_message: Option<String>,
    pub last_health_at: Option<Timestamp>,
    pub last_catalog_refresh_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 单条 catalog entry 的核心字段投影（用于 list）。
#[derive(Debug, Clone)]
pub struct CatalogEntrySummary {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    pub annotations: serde_json::Value,
    pub risk_level: String,
}

/// 单条 install 的核心字段投影。
#[derive(Debug, Clone)]
pub struct InstallSummary {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: String,
    pub catalog_entry_id: Option<String>,
}

/// 单条 grant 的核心字段投影。
#[derive(Debug, Clone)]
pub struct GrantSummary {
    pub id: Uuid,
    pub company_id: Uuid,
    pub profile_id: String,
    pub scopes: serde_json::Value,
}

// ============================================================================
// connection CRUD
// ============================================================================

pub async fn find_by_id(db: &Db, connection_id: Uuid) -> RepoResult<Option<ToolConnectionRow>> {
    let row: Option<ToolConnectionRow> = sqlx::query_as(
        "SELECT id, company_id, application_id, name, transport, status, enabled, config,
         credential_refs, health_status, health_message, last_health_at, last_catalog_refresh_at,
         created_at, updated_at
         FROM tool_connections WHERE id = $1",
    )
    .bind(connection_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

pub async fn delete_by_id(db: &Db, connection_id: Uuid) -> RepoResult<u64> {
    let r = sqlx::query("DELETE FROM tool_connections WHERE id = $1")
        .bind(connection_id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}

/// 单字段 update helper。返回受影响行数（应该 == 1）。
pub async fn update_name(db: &Db, id: Uuid, name: &str) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET name = $1, updated_at = now() WHERE id = $2")
        .bind(name)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}
pub async fn update_enabled(db: &Db, id: Uuid, enabled: bool) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET enabled = $1, updated_at = now() WHERE id = $2")
        .bind(enabled)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}
pub async fn update_status(db: &Db, id: Uuid, status: &str) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET status = $1, updated_at = now() WHERE id = $2")
        .bind(status)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}
pub async fn update_config(db: &Db, id: Uuid, config: &serde_json::Value) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET config = $1, updated_at = now() WHERE id = $2")
        .bind(config)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}
pub async fn update_credential_refs(db: &Db, id: Uuid, refs: &serde_json::Value) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET credential_refs = $1, updated_at = now() WHERE id = $2")
        .bind(refs)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}
pub async fn update_application_id(db: &Db, id: Uuid, application_id: Uuid) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET application_id = $1, updated_at = now() WHERE id = $2")
        .bind(application_id)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}
pub async fn update_health_check(db: &Db, id: Uuid, status: &str, message: Option<&str>) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET health_status = $1, health_message = $2, last_health_at = now(), updated_at = now() WHERE id = $3")
        .bind(status)
        .bind(message)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}
pub async fn update_status_to_reconnecting(db: &Db, id: Uuid) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET status = 'reconnecting', updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}

// ============================================================================
// catalog
// ============================================================================

pub async fn list_catalog(
    db: &Db,
    connection_id: Uuid,
) -> RepoResult<Vec<(Uuid, Uuid, String, Option<String>, Option<String>, serde_json::Value, serde_json::Value, String)>> {
    let rows: Vec<(Uuid, Uuid, String, Option<String>, Option<String>, serde_json::Value, serde_json::Value, String)> = sqlx::query_as(
        "SELECT id, company_id, name, title, description, input_schema, annotations, risk_level
         FROM tool_catalog_entries WHERE connection_id = $1 ORDER BY name",
    )
    .bind(connection_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

pub async fn touch_catalog_refresh(db: &Db, connection_id: Uuid) -> RepoResult<u64> {
    let r = sqlx::query("UPDATE tool_connections SET last_catalog_refresh_at = now(), updated_at = now() WHERE id = $1")
        .bind(connection_id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}

// ============================================================================
// installs
// ============================================================================

pub async fn list_installs(
    db: &Db,
    connection_id: Uuid,
) -> RepoResult<Vec<(Uuid, Uuid, String, String)>> {
    // Schema 实际列：id, company_id, target_type, target_id（agent_id / catalog_entry_id 不存在）
    let rows: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT id, company_id, target_type, target_id FROM tool_connection_installs
         WHERE connection_id = $1 ORDER BY created_at DESC",
    )
    .bind(connection_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

pub async fn upsert_install(
    db: &Db,
    connection_id: Uuid,
    company_id: Uuid,
    target_type: &str,
    target_id: &str,
) -> RepoResult<()> {
    // Schema 实际列：target_type, target_id；冲突策略依赖 (connection_id, target_id) 复合唯一
    sqlx::query(
        "INSERT INTO tool_connection_installs (connection_id, company_id, target_type, target_id)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(connection_id)
    .bind(company_id)
    .bind(target_type)
    .bind(target_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

// ============================================================================
// grants
// ============================================================================

pub async fn grants_table_exists(db: &Db, company_id: Uuid) -> RepoResult<bool> {
    let v: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(
            SELECT 1 FROM information_schema.tables
            WHERE table_schema = 'public' AND table_name = 'tool_connection_grants'
        )",
    )
    .bind(company_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(v.map(|(b,)| b).unwrap_or(false))
}

pub async fn list_grants(
    db: &Db,
    connection_id: Uuid,
) -> RepoResult<Vec<(Uuid, Uuid, String, serde_json::Value)>> {
    let rows: Vec<(Uuid, Uuid, String, serde_json::Value)> = sqlx::query_as(
        "SELECT id, company_id, profile_id, scopes FROM tool_connection_grants WHERE connection_id = $1",
    )
    .bind(connection_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

pub async fn delete_grant(db: &Db, grant_id: Uuid) -> RepoResult<u64> {
    let r = sqlx::query("DELETE FROM tool_connection_grants WHERE id = $1")
        .bind(grant_id)
        .execute(db.pool())
        .await?;
    Ok(r.rows_affected())
}

// ============================================================================
// Round 227: v3 `connection_grants` 仓储方法（OAuth 安装 + revoke 完整实现）
// ============================================================================

/// Round 227: v3 `connection_grants` 行投影。
///
/// 对应 Node `connectionGrants.$inferSelect`：
/// id, company_id, connection_id, kind, subject_user_id,
/// provider_tenant, credential_secret_refs, status, is_default,
/// created_by_*, revoked_*, last_used_at, created_at, updated_at
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionGrantRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub connection_id: Uuid,
    pub kind: String,
    pub subject_user_id: Option<String>,
    pub provider_tenant: Option<serde_json::Value>,
    pub credential_secret_refs: serde_json::Value,
    pub status: String,
    pub is_default: bool,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_by_agent_id: Option<Uuid>,
    pub revoked_by_user_id: Option<String>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Round 227: 列出指定 connection 的所有 v3 connection_grants。
///
/// 排序：is_default DESC, updated_at DESC（与 Node `listConnectionGrants` 对齐）
pub async fn list_connection_grants(
    db: &Db,
    connection_id: Uuid,
    company_id: Uuid,
) -> RepoResult<Vec<ConnectionGrantRow>> {
    let rows: Vec<ConnectionGrantRow> = sqlx::query_as(
        "SELECT id, company_id, connection_id, kind, subject_user_id, \
                provider_tenant, credential_secret_refs, status, is_default, \
                created_by_agent_id, created_by_user_id, \
                revoked_at, revoked_by_agent_id, revoked_by_user_id, \
                last_used_at, created_at, updated_at \
         FROM connection_grants \
         WHERE company_id = $1 AND connection_id = $2 \
         ORDER BY is_default DESC, updated_at DESC",
    )
    .bind(company_id)
    .bind(connection_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

/// Round 227: 清除指定 connection 的所有 workspace default grants。
///
/// 当创建新 default grant 时，需先清除旧的（unique index `connection_grants_default_uq`）。
pub async fn clear_workspace_defaults(
    db: &Db,
    connection_id: Uuid,
    company_id: Uuid,
) -> RepoResult<u64> {
    let r = sqlx::query(
        "UPDATE connection_grants SET is_default = false, updated_at = now() \
         WHERE company_id = $1 AND connection_id = $2 AND kind = 'workspace' AND is_default = true",
    )
    .bind(company_id)
    .bind(connection_id)
    .execute(db.pool())
    .await?;
    Ok(r.rows_affected())
}

/// Round 227: 创建一个 workspace kind connection grant（OAuth installation）。
///
/// 与 Node `addConnectionInstallation` 对齐：
/// - `kind = 'workspace'`
/// - `status = 'active'`
/// - `subject_user_id = None`（workspace 级别的 grant）
/// - 默认 is_default = false
pub async fn create_workspace_grant(
    db: &Db,
    company_id: Uuid,
    connection_id: Uuid,
    provider_tenant: Option<&serde_json::Value>,
    credential_secret_refs: &serde_json::Value,
    is_default: bool,
    created_by_agent_id: Option<Uuid>,
    created_by_user_id: Option<&str>,
) -> RepoResult<ConnectionGrantRow> {
    // 如果 is_default=true，先清除现有 defaults（避免 unique index 冲突）
    if is_default {
        clear_workspace_defaults(db, connection_id, company_id).await?;
    }
    let row: ConnectionGrantRow = sqlx::query_as(
        "INSERT INTO connection_grants \
            (company_id, connection_id, kind, provider_tenant, \
             credential_secret_refs, status, is_default, \
             created_by_agent_id, created_by_user_id) \
         VALUES ($1, $2, 'workspace', $3, $4, 'active', $5, $6, $7) \
         RETURNING id, company_id, connection_id, kind, subject_user_id, \
                provider_tenant, credential_secret_refs, status, is_default, \
                created_by_agent_id, created_by_user_id, \
                revoked_at, revoked_by_agent_id, revoked_by_user_id, \
                last_used_at, created_at, updated_at",
    )
    .bind(company_id)
    .bind(connection_id)
    .bind(provider_tenant)
    .bind(credential_secret_refs)
    .bind(is_default)
    .bind(created_by_agent_id)
    .bind(created_by_user_id)
    .fetch_one(db.pool())
    .await?;
    Ok(row)
}

/// Round 227: 撤销一个 connection grant（设置 status='revoked'）。
///
/// 与 Node `revokeConnectionGrant` 对齐：
/// - status = 'revoked'
/// - is_default = false
/// - revoked_at = now()
/// - revoked_by_* 由 actor 提供
pub async fn revoke_connection_grant(
    db: &Db,
    company_id: Uuid,
    connection_id: Uuid,
    grant_id: Uuid,
    revoked_by_agent_id: Option<Uuid>,
    revoked_by_user_id: Option<&str>,
) -> RepoResult<Option<ConnectionGrantRow>> {
    let row: Option<ConnectionGrantRow> = sqlx::query_as(
        "UPDATE connection_grants SET \
            status = 'revoked', is_default = false, revoked_at = now(), \
            revoked_by_agent_id = $4, revoked_by_user_id = $5, updated_at = now() \
         WHERE id = $1 AND company_id = $2 AND connection_id = $3 \
         RETURNING id, company_id, connection_id, kind, subject_user_id, \
                provider_tenant, credential_secret_refs, status, is_default, \
                created_by_agent_id, created_by_user_id, \
                revoked_at, revoked_by_agent_id, revoked_by_user_id, \
                last_used_at, created_at, updated_at",
    )
    .bind(grant_id)
    .bind(company_id)
    .bind(connection_id)
    .bind(revoked_by_agent_id)
    .bind(revoked_by_user_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

// ============================================================================
// test-agents / test-calls
// ============================================================================

pub async fn list_test_agents(
    db: &Db,
    connection_id: Uuid,
) -> RepoResult<Vec<(Uuid, String, String)>> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, name, adapter_type FROM agents WHERE company_id = $1 AND adapter_type = 'mcp' LIMIT 20",
    )
    .bind(connection_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

// ============================================================================
// activity / usage
// ============================================================================

pub async fn activity_table_exists(db: &Db) -> RepoResult<bool> {
    // 实际表名：tool_invocations（不是 tool_connection_activity）
    let v: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(
            SELECT 1 FROM information_schema.tables
            WHERE table_schema = 'public' AND table_name = 'tool_invocations'
        )",
    )
    .fetch_optional(db.pool())
    .await?;
    Ok(v.map(|(b,)| b).unwrap_or(false))
}

pub async fn list_activity(
    db: &Db,
    connection_id: Uuid,
    limit: i64,
) -> RepoResult<Vec<(Uuid, Uuid, String, serde_json::Value, chrono::DateTime<chrono::Utc>)>> {
    let rows: Vec<(Uuid, Uuid, String, serde_json::Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, connection_id, tool_name, request, created_at
         FROM tool_invocations WHERE connection_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(connection_id)
    .bind(limit)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

pub async fn usage_install_count(db: &Db, connection_id: Uuid) -> RepoResult<Option<i64>> {
    let row: (Option<i64>,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tool_connection_installs WHERE connection_id = $1",
    )
    .bind(connection_id)
    .fetch_one(db.pool())
    .await?;
    Ok(row.0)
}

// ============================================================================
// Repository handle
// ============================================================================

#[derive(Clone)]
pub struct ToolConnectionRepo<'a> {
    db: &'a Db,
}

impl<'a> ToolConnectionRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn find_by_id(&self, id: Uuid) -> RepoResult<Option<ToolConnectionRow>> {
        find_by_id(self.db, id).await
    }
    pub async fn delete_by_id(&self, id: Uuid) -> RepoResult<u64> {
        delete_by_id(self.db, id).await
    }
    pub async fn update_name(&self, id: Uuid, name: &str) -> RepoResult<u64> {
        update_name(self.db, id, name).await
    }
    pub async fn update_enabled(&self, id: Uuid, enabled: bool) -> RepoResult<u64> {
        update_enabled(self.db, id, enabled).await
    }
    pub async fn update_status(&self, id: Uuid, status: &str) -> RepoResult<u64> {
        update_status(self.db, id, status).await
    }
    pub async fn update_config(&self, id: Uuid, config: &serde_json::Value) -> RepoResult<u64> {
        update_config(self.db, id, config).await
    }
    pub async fn update_credential_refs(&self, id: Uuid, refs: &serde_json::Value) -> RepoResult<u64> {
        update_credential_refs(self.db, id, refs).await
    }
    pub async fn update_application_id(&self, id: Uuid, application_id: Uuid) -> RepoResult<u64> {
        update_application_id(self.db, id, application_id).await
    }
    pub async fn update_health_check(&self, id: Uuid, status: &str, message: Option<&str>) -> RepoResult<u64> {
        update_health_check(self.db, id, status, message).await
    }
    pub async fn update_status_to_reconnecting(&self, id: Uuid) -> RepoResult<u64> {
        update_status_to_reconnecting(self.db, id).await
    }
    pub async fn list_catalog(
        &self,
        connection_id: Uuid,
    ) -> RepoResult<Vec<(Uuid, Uuid, String, Option<String>, Option<String>, serde_json::Value, serde_json::Value, String)>> {
        list_catalog(self.db, connection_id).await
    }
    pub async fn touch_catalog_refresh(&self, connection_id: Uuid) -> RepoResult<u64> {
        touch_catalog_refresh(self.db, connection_id).await
    }
    pub async fn list_installs(&self, connection_id: Uuid) -> RepoResult<Vec<(Uuid, Uuid, String, String)>> {
        list_installs(self.db, connection_id).await
    }
    pub async fn upsert_install(
        &self,
        connection_id: Uuid,
        company_id: Uuid,
        target_type: &str,
        target_id: &str,
    ) -> RepoResult<()> {
        upsert_install(self.db, connection_id, company_id, target_type, target_id).await
    }
    pub async fn grants_table_exists(&self, company_id: Uuid) -> RepoResult<bool> {
        grants_table_exists(self.db, company_id).await
    }
    pub async fn list_grants(&self, connection_id: Uuid) -> RepoResult<Vec<(Uuid, Uuid, String, serde_json::Value)>> {
        list_grants(self.db, connection_id).await
    }
    pub async fn delete_grant(&self, grant_id: Uuid) -> RepoResult<u64> {
        delete_grant(self.db, grant_id).await
    }
    pub async fn list_test_agents(&self, connection_id: Uuid) -> RepoResult<Vec<(Uuid, String, String)>> {
        list_test_agents(self.db, connection_id).await
    }
    pub async fn activity_table_exists(&self) -> RepoResult<bool> {
        activity_table_exists(self.db).await
    }
    pub async fn list_activity(
        &self,
        connection_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<(Uuid, Uuid, String, serde_json::Value, chrono::DateTime<chrono::Utc>)>> {
        list_activity(self.db, connection_id, limit).await
    }
    pub async fn usage_install_count(&self, connection_id: Uuid) -> RepoResult<Option<i64>> {
        usage_install_count(self.db, connection_id).await
    }
}
