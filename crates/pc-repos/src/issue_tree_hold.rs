//! `issue_tree_hold` 域 — issue 树暂停/限流控制。
//!
//! Schema (drizzle 0066_issue_tree_holds.sql)：
//! - `issue_tree_holds(id, company_id, root_issue_id, mode, status, reason, release_policy,
//!   created_by_actor_type, created_by_user_id, released_at, created_at, updated_at)`
//!
//! Mode 取值：`pause` / `stop` / `throttle` / `isolate`（路由层校验）。
//! Status 默认 `'active'`，释放后改 `'released'`。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

/// 列表 / 获取轻量投影（不包含 release 元数据列）。
pub const LIST_COLS: &str = "id, root_issue_id, mode, status, reason, release_policy, created_at";

/// 完整字段投影（含 release 元数据）。
pub const FULL_COLS: &str = "id, root_issue_id, mode, status, reason, release_policy, \
    released_at, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeHoldRow {
    pub id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub status: String,
    pub reason: Option<String>,
    pub release_policy: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeHoldDetailRow {
    pub id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub status: String,
    pub reason: Option<String>,
    pub release_policy: serde_json::Value,
    pub released_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// 创建入参。
#[derive(Debug, Clone)]
pub struct NewIssueTreeHold<'a> {
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: &'a str,
    pub reason: Option<&'a str>,
    pub release_policy: serde_json::Value,
    pub created_by_user_id: &'a str,
}

pub struct IssueTreeHoldRepo<'a> {
    pub db: &'a Db,
}

impl<'a> IssueTreeHoldRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 按 root_issue_id + status 列出 holds（按 created_at DESC + LIMIT 100）。
    pub async fn list_by_root(
        &self,
        root_issue_id: Uuid,
        status: &str,
        limit: i64,
    ) -> sqlx::Result<Vec<IssueTreeHoldRow>> {
        let sql = format!(
            "SELECT {LIST_COLS} FROM issue_tree_holds \
             WHERE root_issue_id = $1 AND status = $2 \
             ORDER BY created_at DESC LIMIT $3"
        );
        sqlx::query_as::<_, IssueTreeHoldRow>(&sql)
            .bind(root_issue_id)
            .bind(status)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    /// 按 id + root_issue_id 取完整 hold（含 released_at）。
    pub async fn get_by_id(
        &self,
        id: Uuid,
        root_issue_id: Uuid,
    ) -> sqlx::Result<Option<IssueTreeHoldDetailRow>> {
        let sql = format!(
            "SELECT {FULL_COLS} FROM issue_tree_holds \
             WHERE id = $1 AND root_issue_id = $2"
        );
        sqlx::query_as::<_, IssueTreeHoldDetailRow>(&sql)
            .bind(id)
            .bind(root_issue_id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 创建一个 active hold 并 RETURNING id。
    pub async fn create(&self, h: &NewIssueTreeHold<'_>) -> sqlx::Result<Uuid> {
        let sql = format!(
            "INSERT INTO issue_tree_holds (company_id, root_issue_id, mode, status, reason, \
                release_policy, created_by_actor_type, created_by_user_id) \
             VALUES ($1, $2, $3, 'active', $4, COALESCE($5, '{{}}'::jsonb), 'user', $6) \
             RETURNING id"
        );
        let id: Uuid = sqlx::query_scalar(&sql)
            .bind(h.company_id)
            .bind(h.root_issue_id)
            .bind(h.mode)
            .bind(h.reason)
            .bind(&h.release_policy)
            .bind(h.created_by_user_id)
            .fetch_one(self.db.pool())
            .await?;
        Ok(id)
    }

    /// 释放 hold（UPDATE released_at = now()）；返回 rows_affected > 0 表示实际释放。
    /// 仅释放仍 active 的 hold（幂等）。
    pub async fn release(&self, issue_id: Uuid, hold_id: Uuid) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE issue_tree_holds SET released_at=now() \
             WHERE issue_id=$1 AND id=$2 AND released_at IS NULL",
        )
        .bind(issue_id)
        .bind(hold_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 按 issue 列出 active holds（路由层 `active_hold` 预览用）。
    pub async fn find_active_for_root(
        &self,
        root_issue_id: Uuid,
    ) -> sqlx::Result<Option<(Uuid, String)>> {
        sqlx::query_as(
            "SELECT id, mode FROM issue_tree_holds \
             WHERE root_issue_id = $1 AND status = 'active' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(root_issue_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn count_active(&self, root_issue_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM issue_tree_holds \
             WHERE root_issue_id = $1 AND status = 'active'",
        )
        .bind(root_issue_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_cols_contains_expected() {
        assert!(LIST_COLS.contains("id"));
        assert!(LIST_COLS.contains("root_issue_id"));
        assert!(LIST_COLS.contains("mode"));
        assert!(LIST_COLS.contains("status"));
        assert!(LIST_COLS.contains("reason"));
        assert!(LIST_COLS.contains("release_policy"));
        assert!(LIST_COLS.contains("created_at"));
    }

    #[test]
    fn full_cols_adds_released_at() {
        assert!(FULL_COLS.contains("released_at"));
        assert!(FULL_COLS.contains("created_at"));
    }
}
