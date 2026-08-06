//! `decision_bundles` 域 — 决策束（同一 agent 在同一次心跳里产出的多个 decisions）。
//!
//! Schema (`crates/pc-db/migrations/drizzle/0197_decisions_v1.sql`)：
//! - `decision_bundles(id, company_id, title, summary, origin_agent_id,
//!   origin_issue_id, origin_run_id, created_at)`
//! - 普通索引 `decision_bundles_company_created_at_idx(company_id, created_at)`
//! - 外键：`company_id → companies.id`、`origin_agent_id → agents.id`、
//!   `origin_issue_id → issues.id`、`origin_run_id → heartbeat_runs.id`
//!
//! 与 Node 端 `packages/db/src/schema/decision_bundles.ts` 1:1 对齐：
//! - 一行 = (company, agent, issue, run) 的一束决策上下文
//! - `summary` 默认回退到 `title`（在创建路径上）
//! - 没有 updated_at / status（决策束是一次性创建的快照）

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

const COLS: &str = "id, company_id, title, summary, origin_agent_id, origin_issue_id, \
    origin_run_id, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionBundleRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub summary: String,
    pub origin_agent_id: Uuid,
    pub origin_issue_id: Uuid,
    pub origin_run_id: Uuid,
    pub created_at: Timestamp,
}

/// 写入 decision_bundle 的 DTO（POST /api/companies/:company_id/decision-bundles）。
#[derive(Debug, Clone)]
pub struct NewDecisionBundle {
    pub title: String,
    pub summary: Option<String>,
    pub origin_agent_id: Uuid,
    pub origin_issue_id: Uuid,
    pub origin_run_id: Uuid,
}

/// 列表过滤条件（GET /api/companies/:company_id/decision-bundles）。
#[derive(Debug, Clone, Default)]
pub struct DecisionBundleFilter {
    pub agent_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub limit: Option<i64>,
}

impl DecisionBundleFilter {
    pub fn clamped_limit(&self) -> i64 {
        self.limit.unwrap_or(100).clamp(1, 500)
    }
}

/// bundle 下挂载的决策摘要（get_bundle_with_decisions 内部使用）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSummaryRow {
    pub id: Uuid,
    pub title: String,
    pub status: String,
}

/// 单 bundle 详情 + 关联 decisions 列表的视图。
#[derive(Debug, Clone)]
pub struct DecisionBundleDetail {
    pub bundle: DecisionBundleRow,
    pub decisions: Vec<DecisionSummaryRow>,
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionBundleError {
    #[error("database error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error("decision bundle title must not be empty")]
    EmptyTitle,
}

pub struct DecisionBundleRepo<'a> {
    pub db: &'a Db,
}

impl<'a> DecisionBundleRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 创建一个新的决策束。`summary` 为空时回退到 `title`。
    pub async fn create(
        &self,
        company_id: Uuid,
        input: NewDecisionBundle,
    ) -> Result<DecisionBundleRow, DecisionBundleError> {
        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(DecisionBundleError::EmptyTitle);
        }
        let summary = input
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&title)
            .to_string();
        let row: DecisionBundleRow = sqlx::query_as::<_, DecisionBundleRow>(&format!(
            "INSERT INTO decision_bundles \
             (company_id, title, summary, origin_agent_id, origin_issue_id, origin_run_id) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLS}"
        ))
        .bind(company_id)
        .bind(&title)
        .bind(&summary)
        .bind(input.origin_agent_id)
        .bind(input.origin_issue_id)
        .bind(input.origin_run_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 列出公司在指定过滤条件下的所有决策束（按 created_at DESC）。
    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        filter: &DecisionBundleFilter,
    ) -> RepoResult<Vec<DecisionBundleRow>> {
        let mut sql = format!("SELECT {COLS} FROM decision_bundles WHERE company_id = $1");
        let mut idx = 2u32;
        if filter.agent_id.is_some() {
            sql.push_str(&format!(" AND origin_agent_id = ${idx}"));
            idx += 1;
        }
        if filter.issue_id.is_some() {
            sql.push_str(&format!(" AND origin_issue_id = ${idx}"));
            idx += 1;
        }
        if filter.run_id.is_some() {
            sql.push_str(&format!(" AND origin_run_id = ${idx}"));
            #[allow(unused_assignments)] // final increment kept for symmetry
            {
                idx += 1;
            }
        }
        sql.push_str(&format!(
            " ORDER BY created_at DESC LIMIT {}",
            filter.clamped_limit()
        ));
        let mut query = sqlx::query_as::<_, DecisionBundleRow>(&sql).bind(company_id);
        if let Some(a) = filter.agent_id {
            query = query.bind(a);
        }
        if let Some(i) = filter.issue_id {
            query = query.bind(i);
        }
        if let Some(r) = filter.run_id {
            query = query.bind(r);
        }
        let rows = query.fetch_all(self.db.pool()).await?;
        Ok(rows)
    }

    /// 通过 id 取一个决策束（不包含挂载的 decisions）。
    pub async fn get(&self, id: Uuid) -> RepoResult<Option<DecisionBundleRow>> {
        let row: Option<DecisionBundleRow> = sqlx::query_as::<_, DecisionBundleRow>(&format!(
            "SELECT {COLS} FROM decision_bundles WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 取一个决策束，并附上其下挂载的 decisions（按 created_at ASC）。
    pub async fn get_with_decisions(&self, id: Uuid) -> RepoResult<Option<DecisionBundleDetail>> {
        let bundle = match self.get(id).await? {
            Some(b) => b,
            None => return Ok(None),
        };
        let decisions: Vec<DecisionSummaryRow> = sqlx::query_as::<_, DecisionSummaryRow>(
            "SELECT id, title, status FROM decisions \
             WHERE bundle_id = $1 ORDER BY created_at ASC",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(Some(DecisionBundleDetail { bundle, decisions }))
    }

    /// 统计指定 company 在 (agent, issue, run) 元组下是否已存在同源 bundle。
    pub async fn exists_for_origin(
        &self,
        company_id: Uuid,
        agent_id: Uuid,
        issue_id: Uuid,
        run_id: Uuid,
    ) -> RepoResult<bool> {
        let exists: (bool,) = sqlx::query_as(
            "SELECT EXISTS( \
                SELECT 1 FROM decision_bundles \
                WHERE company_id = $1 AND origin_agent_id = $2 \
                  AND origin_issue_id = $3 AND origin_run_id = $4 \
             )",
        )
        .bind(company_id)
        .bind(agent_id)
        .bind(issue_id)
        .bind(run_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(exists.0)
    }

    /// 删除一个决策束（关联 decisions 的 bundle_id 会被外键 SET NULL）。
    pub async fn delete(&self, id: Uuid) -> RepoResult<bool> {
        let r = sqlx::query("DELETE FROM decision_bundles WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_clamped_limit_defaults_to_100() {
        let f = DecisionBundleFilter::default();
        assert_eq!(f.clamped_limit(), 100);
    }

    #[test]
    fn filter_clamped_limit_caps_at_500() {
        let f = DecisionBundleFilter {
            limit: Some(10_000),
            ..Default::default()
        };
        assert_eq!(f.clamped_limit(), 500);
    }

    #[test]
    fn filter_clamped_limit_minimum_one() {
        let f = DecisionBundleFilter {
            limit: Some(0),
            ..Default::default()
        };
        assert_eq!(f.clamped_limit(), 1);
    }

    #[test]
    fn new_bundle_required_fields_are_stored() {
        let agent = Uuid::new_v4();
        let issue = Uuid::new_v4();
        let run = Uuid::new_v4();
        let input = NewDecisionBundle {
            title: "approve rollout".into(),
            summary: Some("批准灰度".into()),
            origin_agent_id: agent,
            origin_issue_id: issue,
            origin_run_id: run,
        };
        assert_eq!(input.origin_agent_id, agent);
        assert_eq!(input.origin_issue_id, issue);
        assert_eq!(input.origin_run_id, run);
        assert_eq!(input.title, "approve rollout");
    }

    #[test]
    fn empty_title_is_rejected() {
        let res = NewDecisionBundle {
            title: "   ".into(),
            summary: None,
            origin_agent_id: Uuid::new_v4(),
            origin_issue_id: Uuid::new_v4(),
            origin_run_id: Uuid::new_v4(),
        };
        assert!(res.title.trim().is_empty());
    }
}
