//! `issue_diagnostics` 域 — issue 调试 / 诊断视图。
//!
//! 包含三个诊断端点：
//! - `blockers`：subtree 中 status='blocked' 的子 issues（含 parent 自身）
//! - `wakes`：该 issue 的 assignee_agent 收到的 wakeup_requests
//! - `subtree`：递归 parent_id 链上的所有 issues（含 edges / readiness）

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSummaryRow {
    pub id: Uuid,
    pub title: String,
    pub status: Option<String>,
    pub created_at: Timestamp,
}

/// subtree 节点（含 parent_id 与 depth）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtreeNodeRow {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub status: Option<String>,
    pub created_at: Timestamp,
    pub depth: i32,
}

/// agent_wakeup_requests 轻量投影。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeRequestRow {
    pub id: Uuid,
    pub source: String,
    pub reason: Option<String>,
    pub status: String,
    pub requested_at: Timestamp,
    pub claimed_at: Option<Timestamp>,
}

pub struct IssueDiagnosticsRepo<'a> {
    pub db: &'a Db,
}

impl<'a> IssueDiagnosticsRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 列出 issue 的 blockers：subtree 中 status='blocked' 或 hidden 的子 issues。
    pub async fn list_blockers(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<IssueSummaryRow>> {
        sqlx::query_as::<_, IssueSummaryRow>(
            "SELECT id, title, status, created_at FROM issues \
             WHERE company_id = (SELECT company_id FROM issues WHERE id = $1) \
               AND (parent_id = $1 OR id = $1) \
               AND (status = 'blocked' OR hidden_at IS NOT NULL) \
               AND hidden_at IS NULL \
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(issue_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
    }

    /// 查 issue 的 assignee_agent_id（可能为 NULL）。
    pub async fn assignee_agent_id(&self, issue_id: Uuid) -> sqlx::Result<Option<Uuid>> {
        sqlx::query_scalar("SELECT assignee_agent_id FROM issues WHERE id = $1")
            .bind(issue_id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 按 agent 列出 wakeup_requests（按 requested_at DESC + LIMIT）。
    pub async fn list_wake_requests_for_agent(
        &self,
        issue_id: Uuid,
        agent_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<WakeRequestRow>> {
        sqlx::query_as::<_, WakeRequestRow>(
            "SELECT id, source, reason, status, requested_at, claimed_at \
             FROM agent_wakeup_requests \
             WHERE company_id = (SELECT company_id FROM issues WHERE id = $1) \
               AND agent_id = $2 \
             ORDER BY requested_at DESC LIMIT $3",
        )
        .bind(issue_id)
        .bind(agent_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
    }

    /// 递归列出 subtree（含 parent_id / depth，最大 depth 8）。
    pub async fn list_subtree(
        &self,
        issue_id: Uuid,
        max_depth: i32,
    ) -> sqlx::Result<Vec<SubtreeNodeRow>> {
        sqlx::query_as::<_, SubtreeNodeRow>(
            "WITH RECURSIVE subtree AS (
                SELECT id, parent_id, title, status, created_at, 0 AS depth
                FROM issues WHERE id = $1
                UNION ALL
                SELECT i.id, i.parent_id, i.title, i.status, i.created_at, s.depth + 1
                FROM issues i
                INNER JOIN subtree s ON i.parent_id = s.id
                WHERE s.depth < $2 AND i.hidden_at IS NULL
             )
             SELECT id, parent_id, title, status, created_at, depth FROM subtree \
             ORDER BY depth, created_at",
        )
        .bind(issue_id)
        .bind(max_depth)
        .fetch_all(self.db.pool())
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_field_names_camelcase() {
        let s = IssueSummaryRow {
            id: Uuid::nil(),
            title: "t".into(),
            status: Some("todo".into()),
            created_at: Timestamp::now(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("id").is_some());
        assert!(v.get("createdAt").is_some());
    }
}
