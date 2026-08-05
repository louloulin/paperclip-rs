//! `company_export` 域 — 导出快照 preview。
//!
//! Schema：
//! - issues(id, title, status, priority, company_id, …)
//! - agents(id, name, role, company_id, …)
//! - pipelines(id, key, name, company_id, archived_at, …)
//!
//! 对齐 Node `getExportPreview`：返回公司基础信息 + 关键实体的轻量投影，
//! 用于 board UI 在 apply import 前确认范围。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSummary {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub priority: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub id: Uuid,
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineSummary {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyExportPreview {
    pub company_id: Uuid,
    pub issues: Vec<IssueSummary>,
    pub agents: Vec<AgentSummary>,
    pub pipelines: Vec<PipelineSummary>,
}

pub struct CompanyExportRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CompanyExportRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 按 company 列出 issues 的轻量摘要（最多 1000 条）。
    pub async fn list_issue_summaries(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<IssueSummary>> {
        sqlx::query_as::<_, IssueSummary>(
            "SELECT id, title, status, priority FROM issues \
             WHERE company_id = $1 AND hidden_at IS NULL \
             ORDER BY created_at DESC LIMIT 1000",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// 按 company 列出 agents 的轻量摘要（最多 1000 条）。
    pub async fn list_agent_summaries(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<AgentSummary>> {
        sqlx::query_as::<_, AgentSummary>(
            "SELECT id, name, role FROM agents WHERE company_id = $1 \
             ORDER BY name LIMIT 1000",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// 按 company 列出非 archived pipelines 的轻量摘要。
    pub async fn list_pipeline_summaries(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<PipelineSummary>> {
        sqlx::query_as::<_, PipelineSummary>(
            "SELECT id, key, name FROM pipelines \
             WHERE company_id = $1 AND archived_at IS NULL \
             ORDER BY name",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// 复合方法：一次拉取 issues + agents + pipelines。
    /// 调用方按需 `.await?` 任一字段失败都会立即返回 Err。
    pub async fn preview(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<CompanyExportPreview> {
        let issues = self.list_issue_summaries(company_id).await?;
        let agents = self.list_agent_summaries(company_id).await?;
        let pipelines = self.list_pipeline_summaries(company_id).await?;
        Ok(CompanyExportPreview {
            company_id,
            issues,
            agents,
            pipelines,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_structs_use_camelcase() {
        // 简单编译期测试：camelCase 字段映射
        let i = IssueSummary {
            id: Uuid::nil(),
            title: "t".into(),
            status: "todo".into(),
            priority: "normal".into(),
        };
        let v = serde_json::to_value(&i).unwrap();
        assert!(v.get("id").is_some());
        assert!(v.get("title").is_some());
        assert!(v.get("status").is_some());
        assert!(v.get("priority").is_some());
    }
}
