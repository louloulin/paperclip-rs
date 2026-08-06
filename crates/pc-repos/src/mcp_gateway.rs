//! `tool_mcp_gateways` 域 — MCP server gateway 路由表。
//!
//! 设计：
//! - 每条 gateway 绑定一个 profile_id（工具授权）+ agent_id（执行者）
//! - status 字段由运行时更新（connecting / connected / disconnected）
//! - metadata 是 jsonb 自由扩展
//!
//! Round 155: 从 `routes/tool_gateway.rs` 抽出 SQL，提供仓储方法。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{Db, RepoError, RepoResult};

/// 单条 MCP gateway 的完整 DB 行投影（1:1 映射 `tool_mcp_gateways`）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct McpGatewayRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    pub profile_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub metadata: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub async fn list_by_company(db: &Db, company_id: Uuid) -> RepoResult<Vec<McpGatewayRow>> {
    let rows: Vec<McpGatewayRow> = sqlx::query_as(
        "SELECT id, company_id, name, slug, description, status, profile_id, \
         agent_id, project_id, issue_id, metadata, created_at, updated_at \
         FROM tool_mcp_gateways WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    db: &Db,
    company_id: Uuid,
    name: &str,
    slug: &str,
    description: Option<&str>,
    profile_id: Uuid,
    agent_id: Option<Uuid>,
    project_id: Option<Uuid>,
    issue_id: Option<Uuid>,
) -> RepoResult<McpGatewayRow> {
    let row: McpGatewayRow = sqlx::query_as(
        "INSERT INTO tool_mcp_gateways \
         (company_id, name, slug, description, profile_id, agent_id, project_id, issue_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, company_id, name, slug, description, status, profile_id, \
                   agent_id, project_id, issue_id, metadata, created_at, updated_at",
    )
    .bind(company_id)
    .bind(name)
    .bind(slug)
    .bind(description)
    .bind(profile_id)
    .bind(agent_id)
    .bind(project_id)
    .bind(issue_id)
    .fetch_one(db.pool())
    .await?;
    Ok(row)
}

pub async fn find_by_id(db: &Db, gateway_id: Uuid) -> RepoResult<Option<McpGatewayRow>> {
    let row: Option<McpGatewayRow> = sqlx::query_as(
        "SELECT id, company_id, name, slug, description, status, profile_id, \
         agent_id, project_id, issue_id, metadata, created_at, updated_at \
         FROM tool_mcp_gateways WHERE id = $1",
    )
    .bind(gateway_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

/// 按 public slug 或 id 字符串解析 gateway（public 路由用）。
/// 返回 (id, name)。
pub async fn find_id_and_name_by_public_id(
    db: &Db,
    public_id: &str,
) -> RepoResult<Option<(Uuid, String)>> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM tool_mcp_gateways \
         WHERE slug = $1 OR id::text = $1 LIMIT 1",
    )
    .bind(public_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

pub async fn update_partial(
    db: &Db,
    id: Uuid,
    name: Option<&str>,
    description: Option<&str>,
    status: Option<&str>,
    metadata: Option<&serde_json::Value>,
) -> RepoResult<u64> {
    let r = sqlx::query(
        "UPDATE tool_mcp_gateways SET \
            name = COALESCE($1, name), \
            description = COALESCE($2, description), \
            status = COALESCE($3, status), \
            metadata = COALESCE($4, metadata), \
            updated_at = now() \
         WHERE id = $5",
    )
    .bind(name)
    .bind(description)
    .bind(status)
    .bind(metadata)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(r.rows_affected())
}

#[derive(Clone)]
pub struct McpGatewayRepo<'a> {
    db: &'a Db,
}

impl<'a> McpGatewayRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<McpGatewayRow>> {
        list_by_company(self.db, company_id).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        company_id: Uuid,
        name: &str,
        slug: &str,
        description: Option<&str>,
        profile_id: Uuid,
        agent_id: Option<Uuid>,
        project_id: Option<Uuid>,
        issue_id: Option<Uuid>,
    ) -> RepoResult<McpGatewayRow> {
        create(
            self.db,
            company_id,
            name,
            slug,
            description,
            profile_id,
            agent_id,
            project_id,
            issue_id,
        )
        .await
    }
    pub async fn find_by_id(&self, id: Uuid) -> RepoResult<Option<McpGatewayRow>> {
        find_by_id(self.db, id).await
    }
    pub async fn find_id_and_name_by_public_id(
        &self,
        public_id: &str,
    ) -> RepoResult<Option<(Uuid, String)>> {
        find_id_and_name_by_public_id(self.db, public_id).await
    }
    pub async fn update_partial(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> RepoResult<u64> {
        update_partial(self.db, id, name, description, status, metadata).await
    }

    /// Round 155: 检查 gateway 的某个 token 是否有效（active + 未过期 + 未撤销）。
    pub async fn find_active_token(
        &self,
        gateway_id: Uuid,
        token_hash: &str,
    ) -> sqlx::Result<bool> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT t.gateway_id FROM tool_mcp_gateway_tokens t \
             INNER JOIN tool_mcp_gateways g ON g.id = t.gateway_id \
             WHERE g.id = $1 AND g.status = 'active' \
               AND t.token_hash = $2 \
               AND (t.expires_at IS NULL OR t.expires_at > now()) \
               AND t.revoked_at IS NULL \
             LIMIT 1",
        )
        .bind(gateway_id)
        .bind(token_hash)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    /// Round 155: 列出最近 N 个 session（按 created_at DESC）。
    pub async fn list_sessions(
        &self,
        limit: i64,
    ) -> sqlx::Result<Vec<(Uuid, String, String, Option<chrono::DateTime<chrono::Utc>>)>> {
        let rows: Vec<(Uuid, String, String, Option<chrono::DateTime<chrono::Utc>>)> =
            sqlx::query_as(
                "SELECT id, gateway_id::text, status::text, created_at FROM tool_gateway_sessions \
             ORDER BY created_at DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
            .unwrap_or_default();
        Ok(rows)
    }

    /// Round 155: 签发 gateway token。返回 token id。
    pub async fn issue_token(&self, gateway_id: Uuid, token_hash: &str) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO tool_mcp_gateway_tokens (gateway_id, token_hash, created_at) \
             VALUES ($1, $2, now()) RETURNING id",
        )
        .bind(gateway_id)
        .bind(token_hash)
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Round 155: 撤销 gateway token（写 revoked_at）。
    pub async fn revoke_token(&self, token_id: Uuid) -> sqlx::Result<u64> {
        let r = sqlx::query("UPDATE tool_mcp_gateway_tokens SET revoked_at = now() WHERE id = $1")
            .bind(token_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected())
    }

    /// Round 155: 列出最近 N 个 audit events（tool_access_audit_events）。
    pub async fn list_audit_events(
        &self,
        limit: i64,
    ) -> sqlx::Result<
        Vec<(
            Uuid,
            String,
            Option<serde_json::Value>,
            Option<chrono::DateTime<chrono::Utc>>,
        )>,
    > {
        let rows: Vec<(
            Uuid,
            String,
            Option<serde_json::Value>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, kind, payload, created_at FROM tool_access_audit_events \
             ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .unwrap_or_default();
        Ok(rows)
    }

    /// Round 155: 批准 action request（UPDATE tool_action_requests SET status='approved'）。
    pub async fn approve_action_request(&self, request_id: Uuid) -> sqlx::Result<u64> {
        let r = sqlx::query("UPDATE tool_action_requests SET status = 'approved' WHERE id = $1")
            .bind(request_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected())
    }

    /// Round 155: 拒绝 action request。
    pub async fn decline_action_request(&self, request_id: Uuid) -> sqlx::Result<u64> {
        let r = sqlx::query("UPDATE tool_action_requests SET status = 'declined' WHERE id = $1")
            .bind(request_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected())
    }
}
