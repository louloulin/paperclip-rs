//! `company` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

fn issue_prefix_candidate(name: &str, attempt: usize) -> String {
    let base: String = name
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|character| character.to_ascii_uppercase())
        .take(3)
        .collect();
    let base = if base.is_empty() { "PC" } else { &base };
    format!("{base}{}", "A".repeat(attempt.saturating_sub(1)))
}

fn is_issue_prefix_conflict(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(|database_error| {
        database_error.code().as_deref() == Some("23505")
            && database_error.constraint() == Some("companies_issue_prefix_idx")
    })
}

/// Round 128: company 跨表统计投影（6 个 COUNT 聚合结果）。
#[derive(Debug, Clone)]
pub struct CompanyStatsRow {
    pub company_id: Uuid,
    pub issue_count: i64,
    pub open_issue_count: i64,
    pub agent_count: i64,
    pub pipeline_count: i64,
    pub project_count: i64,
    pub goal_count: i64,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]pub struct CompanyRow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub pause_reason: Option<String>,
    pub paused_at: Option<Timestamp>,
    pub issue_prefix: String,
    pub issue_counter: i32,
    pub budget_monthly_cents: i32,
    pub spent_monthly_cents: i32,
    pub attachment_max_bytes: i32,
    pub default_responsible_user_id: Option<String>,
    pub require_board_approval_for_new_agents: bool,
    pub feedback_data_sharing_enabled: bool,
    pub feedback_data_sharing_consent_at: Option<Timestamp>,
    pub feedback_data_sharing_consent_by_user_id: Option<String>,
    pub feedback_data_sharing_terms_version: Option<String>,
    pub brand_color: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CompanyListRow {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub issue_prefix: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct CompanyRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CompanyRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list(&self) -> sqlx::Result<Vec<CompanyListRow>> {
        sqlx::query_as::<_, CompanyListRow>(
            "SELECT id, name, status, issue_prefix, created_at, updated_at \
             FROM companies ORDER BY created_at DESC",
        )
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<CompanyRow>> {
        sqlx::query_as::<_, CompanyRow>(
            "SELECT id, name, description, status, pause_reason, paused_at, \
                    issue_prefix, issue_counter, budget_monthly_cents, spent_monthly_cents, \
                    attachment_max_bytes, default_responsible_user_id, \
                    require_board_approval_for_new_agents, feedback_data_sharing_enabled, \
                    feedback_data_sharing_consent_at, feedback_data_sharing_consent_by_user_id, \
                    feedback_data_sharing_terms_version, brand_color, created_at, updated_at \
             FROM companies WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// 轻量级存在性检查（用于路由 404 前置守卫）。
    pub async fn exists(&self, id: Uuid) -> sqlx::Result<bool> {
        let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM companies WHERE id = $1")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row.is_some())
    }

    pub async fn create(&self, name: &str, description: Option<&str>) -> sqlx::Result<CompanyRow> {
        for attempt in 1..10_000 {
            let result = sqlx::query_as::<_, CompanyRow>(
                "INSERT INTO companies (name, description, issue_prefix) VALUES ($1, $2, $3) \
                 RETURNING id, name, description, status, pause_reason, paused_at, \
                           issue_prefix, issue_counter, budget_monthly_cents, spent_monthly_cents, \
                           attachment_max_bytes, default_responsible_user_id, \
                           require_board_approval_for_new_agents, feedback_data_sharing_enabled, \
                           feedback_data_sharing_consent_at, feedback_data_sharing_consent_by_user_id, \
                           feedback_data_sharing_terms_version, brand_color, created_at, updated_at",
            )
            .bind(name)
            .bind(description)
            .bind(issue_prefix_candidate(name, attempt))
            .fetch_one(self.db.pool())
            .await;
            match result {
                Ok(company) => return Ok(company),
                Err(error) if is_issue_prefix_conflict(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(sqlx::Error::Protocol(
            "unable to allocate unique company issue prefix".into(),
        ))
    }

    pub async fn update(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
    ) -> sqlx::Result<Option<CompanyRow>> {
        sqlx::query_as::<_, CompanyRow>(
            "UPDATE companies SET \
                name = COALESCE($2, name), \
                description = COALESCE($3, description), \
                status = COALESCE($4, status), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, name, description, status, pause_reason, paused_at, \
                       issue_prefix, issue_counter, budget_monthly_cents, spent_monthly_cents, \
                       attachment_max_bytes, default_responsible_user_id, \
                       require_board_approval_for_new_agents, feedback_data_sharing_enabled, \
                       feedback_data_sharing_consent_at, feedback_data_sharing_consent_by_user_id, \
                       feedback_data_sharing_terms_version, brand_color, created_at, updated_at",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(status)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn archive(&self, id: Uuid) -> sqlx::Result<Option<CompanyRow>> {
        sqlx::query_as::<_, CompanyRow>(
            "UPDATE companies SET status = 'archived', updated_at = now() WHERE id = $1 \
             RETURNING id, name, description, status, pause_reason, paused_at, \
                       issue_prefix, issue_counter, budget_monthly_cents, spent_monthly_cents, \
                       attachment_max_bytes, default_responsible_user_id, \
                       require_board_approval_for_new_agents, feedback_data_sharing_enabled, \
                       feedback_data_sharing_consent_at, feedback_data_sharing_consent_by_user_id, \
                       feedback_data_sharing_terms_version, brand_color, created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 128: 复合方法 — company 跨表统计（issues / agents / pipelines / projects / goals）。
    /// 6 个 COUNT(*) 聚合，单调用返回完整 stats。
    pub async fn stats(&self, company_id: Uuid) -> sqlx::Result<CompanyStatsRow> {
        let pool = self.db.pool();
        let issue_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM issues WHERE company_id = $1 AND hidden_at IS NULL",
        )
        .bind(company_id)
        .fetch_one(pool)
        .await?;
        let agent_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agents WHERE company_id = $1")
                .bind(company_id)
                .fetch_one(pool)
                .await?;
        let pipeline_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pipelines WHERE company_id = $1 AND archived_at IS NULL",
        )
        .bind(company_id)
        .fetch_one(pool)
        .await?;
        let project_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM projects WHERE company_id = $1")
                .bind(company_id)
                .fetch_one(pool)
                .await?;
        let goal_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM goals WHERE company_id = $1")
                .bind(company_id)
                .fetch_one(pool)
                .await?;
        let open_issue_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM issues WHERE company_id = $1 AND status NOT IN ('done','cancelled','completed') AND hidden_at IS NULL",
        )
        .bind(company_id)
        .fetch_one(pool)
        .await?;
        Ok(CompanyStatsRow {
            company_id,
            issue_count: issue_count.0,
            open_issue_count: open_issue_count.0,
            agent_count: agent_count.0,
            pipeline_count: pipeline_count.0,
            project_count: project_count.0,
            goal_count: goal_count.0,
        })
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM companies WHERE id = $1")
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
    fn derives_compatible_issue_prefix_candidates() {
        assert_eq!(issue_prefix_candidate("Paper Clip", 1), "PAP");
        assert_eq!(issue_prefix_candidate("Paper Clip", 2), "PAPA");
        assert_eq!(issue_prefix_candidate("123", 1), "PC");
    }
}
