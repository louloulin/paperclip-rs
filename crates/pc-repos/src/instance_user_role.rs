//! `instance_user_roles` 域 — instance-level 角色（目前仅 `instance_admin`）。
//!
//! 设计：
//! - 每行 (user_id, role) 唯一
//! - promote 用 INSERT ... ON CONFLICT DO UPDATE 实现幂等
//! - demote 是硬删除（删除后用户失去 admin 权限）
//!
//! Round 151: 从 `routes/access.rs` 抽出 SQL，提供仓储方法。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

/// 单条 instance_user_roles 行的核心字段投影。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct InstanceUserRoleRow {
    pub id: Uuid,
    pub user_id: String,
    pub role: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

/// Round 151: 列出给定用户集合中拥有任意 instance role 的 user_id。
/// 用于 `list_admin_users` 路由批量标记 isInstanceAdmin。
pub async fn list_user_ids_with_any_role(db: &Db, user_ids: &[String]) -> RepoResult<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT user_id FROM instance_user_roles WHERE user_id = ANY($1::text[])")
            .bind(user_ids)
            .fetch_all(db.pool())
            .await
            .unwrap_or_default();
    Ok(rows.into_iter().map(|(u,)| u).collect())
}

/// Round 151: 授予 instance_admin 角色（幂等）。返回 role_assignment 的 id。
pub async fn promote(db: &Db, user_id: &str) -> RepoResult<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO instance_user_roles (user_id, role) VALUES ($1, 'instance_admin') \
         ON CONFLICT (user_id) DO UPDATE SET updated_at = now() \
         RETURNING id",
    )
    .bind(user_id)
    .fetch_one(db.pool())
    .await?;
    Ok(row.0)
}

/// Round 151: 撤销 instance_admin 角色（硬删除）。返回受影响行数。
pub async fn demote(db: &Db, user_id: &str) -> RepoResult<u64> {
    let r = sqlx::query(
        "DELETE FROM instance_user_roles WHERE user_id = $1 AND role = 'instance_admin'",
    )
    .bind(user_id)
    .execute(db.pool())
    .await?;
    Ok(r.rows_affected())
}

/// Repository handle — wrap `Db` so callers can use the OOP-style API.
#[derive(Clone)]
pub struct InstanceUserRoleRepo<'a> {
    db: &'a Db,
}

impl<'a> InstanceUserRoleRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_user_ids_with_any_role(
        &self,
        user_ids: &[String],
    ) -> RepoResult<Vec<String>> {
        list_user_ids_with_any_role(self.db, user_ids).await
    }

    pub async fn promote(&self, user_id: &str) -> RepoResult<Uuid> {
        promote(self.db, user_id).await
    }

    pub async fn demote(&self, user_id: &str) -> RepoResult<u64> {
        demote(self.db, user_id).await
    }
    /// 检查某 user 是否为 instance_admin。
    pub async fn is_admin(&self, user_id: &str) -> RepoResult<bool> {
        let row: (bool,) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM instance_user_roles \
             WHERE user_id = $1 AND role = 'instance_admin')",
        )
        .bind(user_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }
}
