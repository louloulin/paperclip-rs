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

/// Round 228: 完整 issue tree hold 投影（含 release 元数据 + actor 信息）。
///
/// 对应 Node `issueTreeHolds.$inferSelect`：
/// - 包含所有 release 字段（released_at, released_by_*, release_reason, release_metadata）
/// - 包含所有 created_by 字段
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeHoldFullRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub mode: String,
    pub status: String,
    pub reason: Option<String>,
    pub release_policy: serde_json::Value,
    pub created_by_actor_type: String,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub released_at: Option<Timestamp>,
    pub released_by_actor_type: Option<String>,
    pub released_by_agent_id: Option<Uuid>,
    pub released_by_user_id: Option<String>,
    pub released_by_run_id: Option<Uuid>,
    pub release_reason: Option<String>,
    pub release_metadata: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Round 228: release hold 输入参数。
#[derive(Debug, Clone)]
pub struct ReleaseHoldInput<'a> {
    pub company_id: Uuid,
    pub root_issue_id: Uuid,
    pub hold_id: Uuid,
    /// 释放原因
    pub reason: Option<&'a str>,
    /// 更新 release policy（可选，None 表示保留原值）
    pub release_policy: Option<&'a serde_json::Value>,
    /// release metadata（任意 JSON 对象）
    pub metadata: Option<&'a serde_json::Value>,
    /// Actor 信息
    pub actor_type: &'a str,
    pub actor_id: &'a str,
    pub agent_id: Option<Uuid>,
    pub user_id: Option<&'a str>,
    pub run_id: Option<Uuid>,
}

/// Round 228: release hold 错误。
#[derive(Debug)]
pub enum ReleaseHoldError {
    /// hold 不存在
    NotFound,
    /// hold 属于另一个 root
    WrongRoot,
    /// hold 已经 released（不可重复 release）
    AlreadyReleased,
    /// 数据库错误
    Db(sqlx::Error),
}

impl std::fmt::Display for ReleaseHoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "issue tree hold not found"),
            Self::WrongRoot => write!(f, "issue tree hold does not belong to the requested root issue"),
            Self::AlreadyReleased => write!(f, "issue tree hold is already released"),
            Self::Db(e) => write!(f, "db error: {e}"),
        }
    }
}

impl From<sqlx::Error> for ReleaseHoldError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
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

    /// Round 228: 完整 release hold 操作（与 Node `releaseHold` 对齐）。
    ///
    /// 业务流程：
    /// 1. 查找现有 hold（如果不存在 → NotFound）
    /// 2. 校验 hold.root_issue_id == input.root_issue_id（如果不匹配 → WrongRoot）
    /// 3. 校验 hold.status != 'released'（如果已 release → AlreadyReleased）
    /// 4. UPDATE 设置所有 release 字段 + 返回 updated row
    ///
    /// 释放策略可选择性更新（None 时保留原 release_policy）。
    /// 任意 actor 信息（agent / user / run）由调用方提供。
    pub async fn release_hold_v2(
        &self,
        input: &ReleaseHoldInput<'_>,
    ) -> Result<IssueTreeHoldFullRow, ReleaseHoldError> {
        // 1. 查找现有 hold
        let existing: Option<IssueTreeHoldFullRow> = sqlx::query_as(
            "SELECT id, company_id, root_issue_id, mode, status, reason, release_policy, \
                    created_by_actor_type, created_by_agent_id, created_by_user_id, \
                    created_by_run_id, released_at, released_by_actor_type, \
                    released_by_agent_id, released_by_user_id, released_by_run_id, \
                    release_reason, release_metadata, created_at, updated_at \
             FROM issue_tree_holds \
             WHERE id = $1 AND company_id = $2",
        )
        .bind(input.hold_id)
        .bind(input.company_id)
        .fetch_optional(self.db.pool())
        .await?;
        let existing = existing.ok_or(ReleaseHoldError::NotFound)?;
        // 2. 校验 root_issue_id
        if existing.root_issue_id != input.root_issue_id {
            return Err(ReleaseHoldError::WrongRoot);
        }
        // 3. 校验 status
        if existing.status == "released" {
            return Err(ReleaseHoldError::AlreadyReleased);
        }
        // 4. UPDATE 设置所有 release 字段
        let new_release_policy = input.release_policy.unwrap_or(&existing.release_policy);
        let updated: IssueTreeHoldFullRow = sqlx::query_as(
            "UPDATE issue_tree_holds SET \
                status = 'released', released_at = now(), \
                released_by_actor_type = $2, \
                released_by_agent_id = $3, released_by_user_id = $4, released_by_run_id = $5, \
                release_reason = $6, release_policy = $7, release_metadata = $8, \
                updated_at = now() \
             WHERE id = $1 AND company_id = $9 \
             RETURNING id, company_id, root_issue_id, mode, status, reason, release_policy, \
                    created_by_actor_type, created_by_agent_id, created_by_user_id, \
                    created_by_run_id, released_at, released_by_actor_type, \
                    released_by_agent_id, released_by_user_id, released_by_run_id, \
                    release_reason, release_metadata, created_at, updated_at",
        )
        .bind(input.hold_id)
        .bind(input.actor_type)
        .bind(input.agent_id)
        .bind(input.user_id)
        .bind(input.run_id)
        .bind(input.reason)
        .bind(new_release_policy)
        .bind(input.metadata)
        .bind(input.company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(updated)
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
    // ---- Round 165: issue_tree_control route 仓储化新增方法 ----

    /// Round 165: 列出 root issue 下的 hold 行（按 created_at DESC），含完整列。
    /// 注: schema 没有 `scope` 列（迁移 0066 用 mode/status）。本方法保留原路由 SELECT 的语义（取 schema 等价列）。
    /// 实际查询以 schema 真实列为准：`mode` 替代 `scope`，保留 `reason`/`created_by_user_id`/`created_at`/`released_at`。
    pub async fn list_holds_v1(
        &self,
        root_issue_id: Uuid,
    ) -> sqlx::Result<Vec<(Uuid, String, Option<String>, Option<String>, Timestamp, Option<Timestamp>)>> {
        let rows: Vec<(Uuid, String, Option<String>, Option<String>, Timestamp, Option<Timestamp>)> =
            sqlx::query_as(
                "SELECT id, mode, reason, created_by_user_id, created_at, released_at \
                 FROM issue_tree_holds WHERE root_issue_id = $1 ORDER BY created_at DESC",
            )
            .bind(root_issue_id)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// Round 213: 列出 company 的 tree holds。
    /// `include_released=false` 时仅返回 status='active' AND released_at IS NULL。
    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        include_released: bool,
    ) -> sqlx::Result<Vec<(Uuid, Uuid, String, String, Option<String>, Option<Timestamp>, Timestamp)>> {
        let rows: Vec<(Uuid, Uuid, String, String, Option<String>, Option<Timestamp>, Timestamp)> = if include_released {
            sqlx::query_as(
                "SELECT id, root_issue_id, mode, status, reason, released_at, created_at \
                 FROM issue_tree_holds WHERE company_id = $1 \
                 ORDER BY created_at DESC LIMIT 200",
            )
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, root_issue_id, mode, status, reason, released_at, created_at \
                 FROM issue_tree_holds WHERE company_id = $1 \
                   AND status = 'active' AND released_at IS NULL \
                 ORDER BY created_at DESC LIMIT 200",
            )
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?
        };
        Ok(rows)
    }


    /// Round 165: 按 id 取单条 hold，返回完整列。
    pub async fn get_hold_by_id_v1(
        &self,
        hold_id: Uuid,
    ) -> sqlx::Result<Option<(Uuid, Uuid, String, Option<String>, Option<String>, Timestamp, Option<Timestamp>)>> {
        let row: Option<(Uuid, Uuid, String, Option<String>, Option<String>, Timestamp, Option<Timestamp>)> =
            sqlx::query_as(
                "SELECT id, root_issue_id, mode, reason, created_by_user_id, created_at, released_at \
                 FROM issue_tree_holds WHERE id = $1",
            )
            .bind(hold_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    /// Round 165: 写入一条 tree hold (merge 模式, active 状态, created_by_user_id='local-board')。
    /// 返回 (id, created_at)。
    pub async fn create_v1(
        &self,
        company_id: Uuid,
        root_issue_id: Uuid,
        mode: &str,
        status: &str,
        reason: Option<&str>,
        created_by_user_id: &str,
    ) -> sqlx::Result<(Uuid, Timestamp)> {
        let row: (Uuid, Timestamp) = sqlx::query_as(
            "INSERT INTO issue_tree_holds \
                (company_id, root_issue_id, mode, status, reason, created_by_user_id) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, created_at",
        )
        .bind(company_id)
        .bind(root_issue_id)
        .bind(mode)
        .bind(status)
        .bind(reason)
        .bind(created_by_user_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 165: 释放 hold（按 id 定位，仅释放未释放的）。
    pub async fn release_by_id(&self, hold_id: Uuid) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE issue_tree_holds SET released_at = now() \
             WHERE id = $1 AND released_at IS NULL",
        )
        .bind(hold_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 165: hold 计数（按 released_at IS NULL 过滤，与原路由 SQL 一致）。
    pub async fn count_active_by_released_at(
        &self,
        root_issue_id: Uuid,
    ) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM issue_tree_holds \
             WHERE root_issue_id = $1 AND released_at IS NULL",
        )
        .bind(root_issue_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }

    /// Round 165: 取该 root 下最近的 hold 创建时间（MAX(created_at)）。
    pub async fn latest_change_at(
        &self,
        root_issue_id: Uuid,
    ) -> sqlx::Result<Option<Timestamp>> {
        let row: Option<(Option<Timestamp>,)> = sqlx::query_as(
            "SELECT MAX(created_at) FROM issue_tree_holds WHERE root_issue_id = $1",
        )
        .bind(root_issue_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.and_then(|(o,)| o))
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
