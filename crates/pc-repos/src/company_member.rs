//! `company_member` 域 — 一个公司的 *人类* 成员 (`principal_type='user'`)。
//!
//! Schema (paperclip `packages/db/src/schema/companies.ts` + `company_memberships.ts`)：
//! - `company_memberships(id, company_id, principal_type, principal_id, status, membership_role, created_at, updated_at)`
//! - 唯一索引：`company_memberships_company_principal_unique_idx(company_id, principal_type, principal_id)`
//! - 普通索引：`company_memberships_company_status_idx(company_id, status)`
//! - `"user"(id, name, email, email_verified, image, ...)` 由 `account` 等多张子表引用
//!
//! 与 Node `services/access.ts` 等价：
//! - 仅 `principal_type='user'` 行被当作 *成员*；agent 列在 `company_memberships` 里
//!   也存在但不在本模块范围内。
//! - `membership_role` 是字符串（`'owner' / 'admin' / 'member'`），没有约束。
//! - `status` 默认 `'active'`；被踢出后切到 `'archived'`（仓库层将这两态桥接到 Node 的 `archived_at`）。
//!
//! 设计：
//! - 完全独立于 `membership.rs`（项目/agent/document 成员），避免混在一起又变成 1000 行巨石。
//! - 不引入 `archived_at` 列（与 Node schema 一致）；通过 `status` 判定。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

const COLS: &str = "cm.id, cm.company_id, cm.principal_id, cm.membership_role, cm.status, \
    cm.created_at, cm.updated_at";

/// 公司成员 status 字符串，对应数据库列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    Active,
    Archived,
}

impl MemberStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemberStatus::Active => "active",
            MemberStatus::Archived => "archived",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(MemberStatus::Active),
            "archived" => Some(MemberStatus::Archived),
            _ => None,
        }
    }
}

/// 一条公司成员记录 + 关联 `"user"` 表字段（LEFT JOIN，可能 NULL）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyMemberRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub principal_id: String,
    pub membership_role: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// LEFT JOIN `"user"` — anonymous / not-found 时为 `None`。
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
}

/// 列表查询可选过滤项。
/// 列表过滤项。**注意** `Default::default()` 返回的是空字符串 principal_type，
/// 调用方应显式提供（或调用 `MemberFilter::user()`）。
#[derive(Debug, Clone, Default)]
pub struct MemberFilter<'a> {
    pub include_archived: bool,
    pub role: Option<&'a str>,
    /// 限定 `principal_type`，常用 `'user'`。
    pub principal_type: &'a str,
}

impl<'a> MemberFilter<'a> {
    /// 最常用：`include_archived=false`、`principal_type='user'`。
    pub fn user() -> Self {
        Self { include_archived: false, role: None, principal_type: "user" }
    }
}

/// patch 请求 payload。
#[derive(Debug, Clone, Default)]
pub struct MemberPatch {
    pub membership_role: Option<String>,
    pub status: Option<MemberStatus>,
}

pub struct CompanyMemberRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CompanyMemberRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 列出公司成员，按 role 后 fallback user name 排序。
    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        filter: MemberFilter<'_>,
    ) -> RepoResult<Vec<CompanyMemberRow>> {
        let mut sql = format!(
            "SELECT {COLS}, u.name, u.email, u.image \
             FROM company_memberships cm \
             LEFT JOIN \"user\" u ON u.id = cm.principal_id \
             WHERE cm.company_id = $1 AND cm.principal_type = $2"
        );
        if !filter.include_archived {
            sql.push_str(" AND cm.status = 'active'");
        }
        if filter.role.is_some() {
            sql.push_str(" AND cm.membership_role = $3");
        }
        sql.push_str(" ORDER BY cm.membership_role, COALESCE(u.name, cm.principal_id)");
        let mut q = sqlx::query_as::<_, CompanyMemberRow>(&sql)
            .bind(company_id)
            .bind(filter.principal_type);
        if let Some(r) = filter.role {
            q = q.bind(r);
        }
        let rows = q.fetch_all(self.db.pool()).await?;
        Ok(rows)
    }

    /// 通过 id 查找一条成员记录。
    pub async fn find_by_id(
        &self,
        company_id: Uuid,
        member_id: Uuid,
    ) -> RepoResult<Option<CompanyMemberRow>> {
        let sql = format!(
            "SELECT {COLS}, u.name, u.email, u.image \
             FROM company_memberships cm \
             LEFT JOIN \"user\" u ON u.id = cm.principal_id \
             WHERE cm.id = $1 AND cm.company_id = $2 LIMIT 1"
        );
        let row = sqlx::query_as::<_, CompanyMemberRow>(&sql)
            .bind(member_id)
            .bind(company_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    /// 通过 user_id (principal_id) 查找一条成员记录。
    pub async fn find_by_user(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> RepoResult<Option<CompanyMemberRow>> {
        let sql = format!(
            "SELECT {COLS}, u.name, u.email, u.image \
             FROM company_memberships cm \
             LEFT JOIN \"user\" u ON u.id = cm.principal_id \
             WHERE cm.company_id = $1 AND cm.principal_type = 'user' \
             AND cm.principal_id = $2 LIMIT 1"
        );
        let row = sqlx::query_as::<_, CompanyMemberRow>(&sql)
            .bind(company_id)
            .bind(user_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    /// patch 成员 role / status；空字段 = 不动。
    /// UPDATE 走 company_memberships 表本身，RETURNING 只取基础列；
    /// 然后再通过 `find_by_id` 拉一次（LEFT JOIN user）回填 name/email/image。
    pub async fn patch(
        &self,
        company_id: Uuid,
        member_id: Uuid,
        patch: MemberPatch,
    ) -> RepoResult<Option<CompanyMemberRow>> {
        if patch.membership_role.is_none() && patch.status.is_none() {
            // 没字段要改，直接回读
            return self.find_by_id(company_id, member_id).await;
        }
        let mut sql = String::from(
            "UPDATE company_memberships \
             SET updated_at = now()",
        );
        if patch.membership_role.is_some() {
            sql.push_str(", membership_role = $3");
        }
        if patch.status.is_some() {
            sql.push_str(", status = $4");
        }
        sql.push_str(
            " WHERE id = $1 AND company_id = $2",
        );
        let mut q = sqlx::query(&sql).bind(member_id).bind(company_id);
        if let Some(role) = patch.membership_role.as_ref() {
            q = q.bind(role);
        }
        if let Some(st) = patch.status {
            q = q.bind(st.as_str().to_string());
        }
        let r = q.execute(self.db.pool()).await?;
        if r.rows_affected() == 0 {
            return Ok(None);
        }
        // 左连接 user 取回 name/email/image
        self.find_by_id(company_id, member_id).await
    }

    /// 软归档：将 status 切到 `'archived'`；幂等。
    pub async fn archive(
        &self,
        company_id: Uuid,
        member_id: Uuid,
    ) -> RepoResult<bool> {
        let r = sqlx::query(
            "UPDATE company_memberships SET status = 'archived', updated_at = now() \
             WHERE id = $1 AND company_id = $2 AND status != 'archived'",
        )
        .bind(member_id)
        .bind(company_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// 计数：用于 sidebar / company stats。
    pub async fn count_active_for_company(&self, company_id: Uuid) -> RepoResult<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM company_memberships \
             WHERE company_id = $1 AND principal_type = 'user' AND status = 'active'",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_status_roundtrip() {
        assert_eq!(MemberStatus::Active.as_str(), "active");
        assert_eq!(MemberStatus::Archived.as_str(), "archived");
        assert_eq!(MemberStatus::parse("active"), Some(MemberStatus::Active));
        assert_eq!(MemberStatus::parse("archived"), Some(MemberStatus::Archived));
        assert_eq!(MemberStatus::parse("deleted"), None);
        assert_eq!(MemberStatus::parse(""), None);
    }

    #[test]
    fn member_patch_default_is_empty() {
        let p = MemberPatch::default();
        assert!(p.membership_role.is_none());
        assert!(p.status.is_none());
    }

    #[test]
    fn member_filter_user_helper_sets_active_user_only() {
        let f = MemberFilter::user();
        assert!(!f.include_archived);
        assert_eq!(f.principal_type, "user");
        assert!(f.role.is_none());
    }

    #[test]
    fn member_filter_default_is_empty() {
        let f = MemberFilter::default();
        assert!(!f.include_archived);
        assert_eq!(f.principal_type, "");
        assert!(f.role.is_none());
    }
}
