//! `feedback_vote` 域 — `feedback_votes` 表。
//!
//! Schema (drizzle 0047_overjoyed_groot.sql)：
//! - `feedback_votes(id, company_id, issue_id, target_type, target_id,
//!   author_user_id, vote, reason, shared_with_labs, shared_at,
//!   consent_version, redaction_summary, created_at, updated_at)`
//!
//! 对齐 Node `feedbackVotes` 模块（Round 95 修复后）：
//! - 列名 `voter_kind` → `target_type`；`score` → `vote` (text)
//! - 必填字段补齐 `company_id`（从 issues 查）+ `author_user_id`（默认 'system'）

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

const COLS: &str = "id, company_id, issue_id, target_type, target_id, \
    author_user_id, vote, reason, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackVoteRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub target_type: String,
    pub target_id: String,
    pub author_user_id: String,
    pub vote: String,
    pub reason: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Default)]
pub struct NewFeedbackVote {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub target_type: String,
    pub target_id: String,
    pub author_user_id: String,
    pub vote: String,
    pub reason: Option<String>,
}

pub struct FeedbackVoteRepo<'a> {
    pub db: &'a Db,
}

impl<'a> FeedbackVoteRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 按 issue 列出 feedback votes（按 created_at DESC + LIMIT 100）。
    pub async fn list_by_issue(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<FeedbackVoteRow>> {
        let sql = format!(
            "SELECT {COLS} FROM feedback_votes WHERE issue_id = $1 \
             ORDER BY created_at DESC LIMIT $2"
        );
        sqlx::query_as::<_, FeedbackVoteRow>(&sql)
            .bind(issue_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    /// 按 id 取单条。
    pub async fn get_by_id(&self, id: Uuid) -> sqlx::Result<Option<FeedbackVoteRow>> {
        let sql = format!("SELECT {COLS} FROM feedback_votes WHERE id = $1");
        sqlx::query_as::<_, FeedbackVoteRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 创建一条 feedback vote 并 RETURNING id。
    pub async fn create(&self, v: &NewFeedbackVote) -> sqlx::Result<Uuid> {
        let sql = format!(
            "INSERT INTO feedback_votes (company_id, issue_id, target_type, target_id, \
                author_user_id, vote, reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id"
        );
        let id: Uuid = sqlx::query_scalar(&sql)
            .bind(v.company_id)
            .bind(v.issue_id)
            .bind(&v.target_type)
            .bind(&v.target_id)
            .bind(&v.author_user_id)
            .bind(&v.vote)
            .bind(v.reason.as_deref())
            .fetch_one(self.db.pool())
            .await?;
        Ok(id)
    }

    /// 查 issue 所属 company_id（用于 create 前补齐 company_id 必填字段）。
    pub async fn issue_company_id(&self, issue_id: Uuid) -> sqlx::Result<Option<Uuid>> {
        sqlx::query_scalar("SELECT company_id FROM issues WHERE id = $1")
            .bind(issue_id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 复合方法：先查 issue 的 company_id，再 INSERT；返回新 vote 的 id。
    /// issue 不存在返回 RepoError 的 Err（由调用方映射为 NotFound）。
    pub async fn create_for_issue(
        &self,
        issue_id: Uuid,
        target_type: &str,
        target_id: &str,
        author_user_id: &str,
        vote: &str,
        reason: Option<&str>,
    ) -> sqlx::Result<Uuid> {
        let company_id = self
            .issue_company_id(issue_id)
            .await?
            .ok_or_else(|| sqlx::Error::RowNotFound)?;
        let input = NewFeedbackVote {
            company_id,
            issue_id,
            target_type: target_type.into(),
            target_id: target_id.into(),
            author_user_id: author_user_id.into(),
            vote: vote.into(),
            reason: reason.map(str::to_string),
        };
        self.create(&input).await
    }

    pub async fn count_by_issue(&self, issue_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feedback_votes WHERE issue_id = $1")
            .bind(issue_id)
            .fetch_one(self.db.pool())
            .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cols_lists_expected_fields() {
        assert!(COLS.contains("id"));
        assert!(COLS.contains("company_id"));
        assert!(COLS.contains("issue_id"));
        assert!(COLS.contains("target_type"));
        assert!(COLS.contains("target_id"));
        assert!(COLS.contains("author_user_id"));
        assert!(COLS.contains("vote"));
        assert!(COLS.contains("reason"));
        assert!(COLS.contains("created_at"));
    }
}
