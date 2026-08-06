//! `feedback_trace` 域 — `issue_feedback_traces` 表。
//!
//! Schema（参考 Node `server/src/services/feedbackTraces.ts`）：
//! - `issue_feedback_traces(id, issue_id, kind, payload, created_at)` + issue 外键
//! - 跨 company 聚合通过 JOIN issues 表
//!
//! 注：当前 schema 不一定包含 `issue_feedback_traces` 表，所以 list_for_company
//! 失败时返回空集合（与 Node 兼容：表不存在视为"无 traces"）。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

const COLS: &str = "id, kind, payload, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackTraceRow {
    pub id: Uuid,
    pub kind: String,
    pub payload: Option<serde_json::Value>,
    pub created_at: Timestamp,
}

/// 按 company 聚合所有 issue 的 feedback traces（按 created_at DESC）。
///
/// JOIN issues 取 company_id；表不存在或 schema 不匹配时返回空集合。
pub struct FeedbackTraceRepo<'a> {
    pub db: &'a Db,
}

impl<'a> FeedbackTraceRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_for_company(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<FeedbackTraceRow>> {
        let sql = format!(
            "SELECT {COLS} FROM issue_feedback_traces t \
             JOIN issues i ON i.id = t.issue_id \
             WHERE i.company_id = $1 \
             ORDER BY t.created_at DESC LIMIT $2"
        );
        sqlx::query_as::<_, FeedbackTraceRow>(&sql)
            .bind(company_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    /// 按 issue 列出 feedback traces（按 created_at DESC + LIMIT）。
    /// Round 135 仓储化 issues.rs list_issue_feedback_traces 端点。
    pub async fn list_by_issue(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<FeedbackTraceRow>> {
        let sql = format!(
            "SELECT {COLS} FROM issue_feedback_traces              WHERE issue_id = $1 ORDER BY created_at DESC LIMIT $2"
        );
        sqlx::query_as::<_, FeedbackTraceRow>(&sql)
            .bind(issue_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    /// 按 id 查单条（返回 issue_id / kind / payload / created_at）。
    pub async fn get_by_id_full(
        &self,
        id: Uuid,
    ) -> sqlx::Result<Option<(Uuid, String, Option<serde_json::Value>, Timestamp)>> {
        sqlx::query_as(
            "SELECT issue_id, kind, payload, created_at FROM issue_feedback_traces WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// 按 id 查单条（返回 issue_id + payload）。
    pub async fn get_bundle(
        &self,
        id: Uuid,
    ) -> sqlx::Result<Option<(Uuid, Option<serde_json::Value>)>> {
        sqlx::query_as("SELECT issue_id, payload FROM issue_feedback_traces WHERE id = $1")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 按 id 删除，返回 rows_affected > 0 表示实际删除。
    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let n = sqlx::query("DELETE FROM issue_feedback_traces WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cols_lists_expected_fields() {
        assert!(COLS.contains("id"));
        assert!(COLS.contains("kind"));
        assert!(COLS.contains("payload"));
        assert!(COLS.contains("created_at"));
    }
}
