//! `principal_permission_grant` 域 — 给定公司在 (principal_type, principal_id) 上
//! 授予/撤销 `permission_key` 的能力。
//!
//! Schema (paperclip `packages/db/src/schema/principal_permission_grants.ts`)：
//! - `principal_permission_grants(id, company_id, principal_type, principal_id,
//!   permission_key, scope jsonb, granted_by_user_id, created_at, updated_at)`
//! - 唯一索引 `principal_permission_grants_unique_idx(company_id, principal_type, principal_id, permission_key)`
//! - 普通索引 `principal_permission_grants_company_permission_idx(company_id, permission_key)`
//!
//! 与 Node 1:1 对齐：
//! - 一行 = (company, principal, key) 三元组唯一
//! - `scope jsonb` 可空；空则全公司范围
//! - `granted_by_user_id` 文本留痕

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionGrantRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub principal_type: String,
    pub principal_id: String,
    pub permission_key: String,
    #[serde(default)]
    pub scope: Option<JsonValue>,
    #[serde(default)]
    pub granted_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 写入 grant 时使用的 DTO。
#[derive(Debug, Clone)]
pub struct PermissionGrantInput {
    pub permission_key: String,
    pub scope: Option<JsonValue>,
    pub granted_by_user_id: Option<String>,
}

pub struct PrincipalPermissionGrantRepo<'a> {
    pub db: &'a Db,
}

impl<'a> PrincipalPermissionGrantRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 列出给定 principal 在公司的所有 grant。
    pub async fn list_for_principal(
        &self,
        company_id: Uuid,
        principal_type: &str,
        principal_id: &str,
    ) -> RepoResult<Vec<PermissionGrantRow>> {
        let rows: Vec<PermissionGrantRow> = sqlx::query_as::<_, PermissionGrantRow>(
            "SELECT id, company_id, principal_type, principal_id, permission_key, scope, granted_by_user_id, created_at, updated_at              FROM principal_permission_grants              WHERE company_id = $1 AND principal_type = $2 AND principal_id = $3              ORDER BY permission_key",
        )
        .bind(company_id)
        .bind(principal_type)
        .bind(principal_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// upsert 一行 grant（permission_key 唯一）。
    pub async fn upsert_one(
        &self,
        company_id: Uuid,
        principal_type: &str,
        principal_id: &str,
        input: PermissionGrantInput,
    ) -> RepoResult<PermissionGrantRow> {
        let row: PermissionGrantRow = sqlx::query_as::<_, PermissionGrantRow>(
            "INSERT INTO principal_permission_grants (id, company_id, principal_type, principal_id, permission_key, scope, granted_by_user_id, created_at, updated_at)              VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, now(), now())              ON CONFLICT (company_id, principal_type, principal_id, permission_key) DO UPDATE              SET scope = EXCLUDED.scope, granted_by_user_id = EXCLUDED.granted_by_user_id, updated_at = now()              RETURNING id, company_id, principal_type, principal_id, permission_key, scope, granted_by_user_id, created_at, updated_at",
        )
        .bind(company_id)
        .bind(principal_type)
        .bind(principal_id)
        .bind(&input.permission_key)
        .bind(&input.scope)
        .bind(&input.granted_by_user_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 删除一行（按 key）。
    pub async fn revoke_one(
        &self,
        company_id: Uuid,
        principal_type: &str,
        principal_id: &str,
        permission_key: &str,
    ) -> RepoResult<bool> {
        let r = sqlx::query(
            "DELETE FROM principal_permission_grants WHERE company_id = $1 AND principal_type = $2 AND principal_id = $3 AND permission_key = $4",
        )
        .bind(company_id)
        .bind(principal_type)
        .bind(principal_id)
        .bind(permission_key)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// 事务：在同一 principal 下，先删除所有旧 grant，再插入新 grant 列表。
    pub async fn replace_all_for_principal<'g, I>(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        company_id: Uuid,
        principal_type: &str,
        principal_id: &str,
        grants: I,
    ) -> RepoResult<Vec<PermissionGrantRow>>
    where
        I: IntoIterator<Item = &'g PermissionGrantInput>,
    {
        sqlx::query(
            "DELETE FROM principal_permission_grants WHERE company_id = $1 AND principal_type = $2 AND principal_id = $3",
        )
        .bind(company_id)
        .bind(principal_type)
        .bind(principal_id)
        .execute(&mut **tx)
        .await?;
        let mut written = Vec::new();
        for grant in grants {
            let row: PermissionGrantRow = sqlx::query_as::<_, PermissionGrantRow>(
                "INSERT INTO principal_permission_grants (id, company_id, principal_type, principal_id, permission_key, scope, granted_by_user_id, created_at, updated_at)                  VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, now(), now())                  RETURNING id, company_id, principal_type, principal_id, permission_key, scope, granted_by_user_id, created_at, updated_at",
            )
            .bind(company_id)
            .bind(principal_type)
            .bind(principal_id)
            .bind(&grant.permission_key)
            .bind(&grant.scope)
            .bind(&grant.granted_by_user_id)
            .fetch_one(&mut **tx)
            .await?;
            written.push(row);
        }
        written.sort_by(|a, b| a.permission_key.cmp(&b.permission_key));
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_grant_input_default_scope_is_none() {
        let input = PermissionGrantInput {
            permission_key: "tasks:assign".to_string(),
            scope: None,
            granted_by_user_id: None,
        };
        assert_eq!(input.permission_key, "tasks:assign");
        assert!(input.scope.is_none());
        assert!(input.granted_by_user_id.is_none());
    }
}
