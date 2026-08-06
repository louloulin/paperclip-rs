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
    /// pipeline_cases 表行数（Round 132 扩展）
    pub case_count: i64,
    /// company_memberships active 行数（Round 132 扩展）
    pub user_count: i64,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CompanyRow {
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
    /// Round 148: 轻量按 id 取 name（invite_onboarding / onboarding_txt 用）。
    pub async fn find_name_by_id(&self, id: Uuid) -> sqlx::Result<Option<String>> {
        sqlx::query_scalar("SELECT name FROM companies WHERE id = $1")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

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

    /// 更新 branding 字段。
    ///
    /// 行为对齐 Node `updateBranding`：
    /// - `name` 提供时 UPDATE
    /// - `logo_url` 提供时嵌入 description 后缀（`<!-- logo:{url} -->`），实际项目
    ///   应使用独立 branding 表，但当前 schema 仅 companies.description 可写
    ///
    /// 复合方法：返回 Option<CompanyRow> 表示更新后的整行；None 表示 company 不存在。
    pub async fn update_branding(
        &self,
        id: Uuid,
        name: Option<&str>,
        logo_url: Option<&str>,
    ) -> sqlx::Result<Option<CompanyRow>> {
        let mut current_desc: Option<String> = None;
        if logo_url.is_some() {
            let row: Option<(Option<String>,)> =
                sqlx::query_as("SELECT description FROM companies WHERE id = $1")
                    .bind(id)
                    .fetch_optional(self.db.pool())
                    .await?;
            current_desc = row.and_then(|(d,)| d);
        }
        let new_desc = if let Some(logo) = logo_url {
            let prev = current_desc.unwrap_or_default();
            Some(format!("{}\n<!-- logo:{} -->", prev, logo))
        } else {
            None
        };
        sqlx::query_as::<_, CompanyRow>(
            "UPDATE companies SET                 name = COALESCE($2, name),                 description = COALESCE($3, description),                 updated_at = now()              WHERE id = $1              RETURNING id, name, description, status, pause_reason, paused_at,                        issue_prefix, issue_counter, budget_monthly_cents, spent_monthly_cents,                        attachment_max_bytes, default_responsible_user_id,                        require_board_approval_for_new_agents, feedback_data_sharing_enabled,                        feedback_data_sharing_consent_at, feedback_data_sharing_consent_by_user_id,                        feedback_data_sharing_terms_version, brand_color, created_at, updated_at",
        )
        .bind(id)
        .bind(name)
        .bind(new_desc.as_deref())
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
        let goal_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM goals WHERE company_id = $1")
            .bind(company_id)
            .fetch_one(pool)
            .await?;
        let open_issue_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM issues WHERE company_id = $1 AND status NOT IN ('done','cancelled','completed') AND hidden_at IS NULL",
        )
        .bind(company_id)
        .fetch_one(pool)
        .await?;
        let case_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM pipeline_cases WHERE company_id = $1")
                .bind(company_id)
                .fetch_one(pool)
                .await?;
        let user_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM company_memberships WHERE company_id = $1 AND status = 'active'",
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
            case_count: case_count.0,
            user_count: user_count.0,
        })
    }

    /// 列出当前 user 拥有 active membership 的所有 company（按 name 排序）。
    ///
    /// 用于 board-only `/companies/stats` 端点（Round 132 仓储化）：
    /// 替代原 route 内联 SQL（INNER JOIN company_memberships）。
    pub async fn list_accessible_for_user(
        &self,
        user_id: &str,
    ) -> sqlx::Result<Vec<CompanyListRow>> {
        sqlx::query_as::<_, CompanyListRow>(
            "SELECT c.id, c.name FROM companies c              INNER JOIN company_memberships cm ON cm.company_id = c.id              WHERE cm.principal_id = $1 AND cm.status = 'active'              ORDER BY c.name",
        )
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// 批量拉取多 company 的 stats（issues/agents/pipelines/projects/goals）。
    ///
    /// 返回 `HashMap<Uuid, CompanyStatsRow>`，缺失 company 视为全 0。
    /// 用于 board-only `/companies/stats` 端点（Round 132 仓储化）：
    /// 替代原 route 内 1 + N*4 = 1+4N 次 SQL 循环。
    pub async fn stats_for_companies(
        &self,
        company_ids: &[Uuid],
    ) -> sqlx::Result<std::collections::HashMap<Uuid, CompanyStatsRow>> {
        use std::collections::HashMap;
        let mut out: HashMap<Uuid, CompanyStatsRow> = HashMap::new();
        if company_ids.is_empty() {
            return Ok(out);
        }
        // 初始化占位（确保 list 中所有 id 都有 entry）
        for id in company_ids {
            out.insert(
                *id,
                CompanyStatsRow {
                    company_id: *id,
                    issue_count: 0,
                    open_issue_count: 0,
                    agent_count: 0,
                    pipeline_count: 0,
                    project_count: 0,
                    goal_count: 0,
                    case_count: 0,
                    user_count: 0,
                },
            );
        }
        // 6 个独立 aggregate query，每个一次：ANY($1::uuid[]) WHERE company_id = ANY
        macro_rules! agg {
            ($sql:expr, $field:ident) => {{
                let rows: Vec<(Uuid, i64)> = sqlx::query_as($sql)
                    .bind(company_ids)
                    .fetch_all(self.db.pool())
                    .await?;
                for (cid, n) in rows {
                    if let Some(entry) = out.get_mut(&cid) {
                        entry.$field = n;
                    }
                }
            }};
        }
        agg!(
            "SELECT company_id, COUNT(*) FROM issues              WHERE company_id = ANY($1::uuid[]) AND hidden_at IS NULL              GROUP BY company_id",
            issue_count
        );
        agg!(
            "SELECT company_id, COUNT(*) FROM issues              WHERE company_id = ANY($1::uuid[]) AND hidden_at IS NULL                AND status NOT IN ('done','cancelled','completed')              GROUP BY company_id",
            open_issue_count
        );
        agg!(
            "SELECT company_id, COUNT(*) FROM agents              WHERE company_id = ANY($1::uuid[]) GROUP BY company_id",
            agent_count
        );
        agg!(
            "SELECT company_id, COUNT(*) FROM pipelines              WHERE company_id = ANY($1::uuid[]) AND archived_at IS NULL              GROUP BY company_id",
            pipeline_count
        );
        agg!(
            "SELECT company_id, COUNT(*) FROM projects              WHERE company_id = ANY($1::uuid[]) GROUP BY company_id",
            project_count
        );
        agg!(
            "SELECT company_id, COUNT(*) FROM goals              WHERE company_id = ANY($1::uuid[]) GROUP BY company_id",
            goal_count
        );
        agg!(
            "SELECT company_id, COUNT(*) FROM pipeline_cases              WHERE company_id = ANY($1::uuid[]) GROUP BY company_id",
            case_count
        );
        agg!(
            "SELECT company_id, COUNT(*) FROM company_memberships              WHERE company_id = ANY($1::uuid[]) AND status = 'active'              GROUP BY company_id",
            user_count
        );
        Ok(out)
    }

    /// 创建 / 升级 company owner membership。
    ///
    /// 对齐 Node `create` 端点 ON CONFLICT 升级逻辑：
    /// - INSERT principal_type='user' status='active' membership_role='owner'
    /// - 已存在时 UPDATE status='active', membership_role=COALESCE(..., 'owner')
    ///
    /// 原子性由 SQL ON CONFLICT 单条 SQL 保证。
    pub async fn create_owner_membership(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO company_memberships                 (company_id, principal_type, principal_id, status, membership_role)              VALUES ($1, 'user', $2, 'active', 'owner')              ON CONFLICT (company_id, principal_type, principal_id) DO UPDATE SET                 status = 'active',                 membership_role = COALESCE(company_memberships.membership_role, 'owner'),                 updated_at = now()",
        )
        .bind(company_id)
        .bind(user_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM companies WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Round 168: 取 company 的 budget_monthly_cents。
    pub async fn get_budget(&self, company_id: Uuid) -> sqlx::Result<Option<i32>> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT budget_monthly_cents FROM companies WHERE id = $1")
                .bind(company_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(b,)| b))
    }
    /// Round 174: 实例统计用 —— 列出所有 company_id（按 created_at 升序，与 /api/stats 行为一致）。
    pub async fn list_ids(&self) -> sqlx::Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM companies ORDER BY created_at")
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Round 176: 设置 company 的 budget_monthly_cents，返回更新后的 (id, budget) 元组。
    pub async fn set_budget(
        &self,
        company_id: Uuid,
        budget_monthly_cents: i32,
    ) -> sqlx::Result<Option<(Uuid, i32)>> {
        let row: Option<(Uuid, i32)> = sqlx::query_as(
            "UPDATE companies SET budget_monthly_cents = $2, updated_at = now() \
             WHERE id = $1 RETURNING id, budget_monthly_cents",
        )
        .bind(company_id)
        .bind(budget_monthly_cents)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 182: 设置 company 的 logo_url。
    pub async fn set_logo_url(&self, company_id: Uuid, logo_url: &str) -> sqlx::Result<bool> {
        let n = sqlx::query("UPDATE companies SET logo_url = $1, updated_at = now() WHERE id = $2")
            .bind(logo_url)
            .bind(company_id)
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
    fn derives_compatible_issue_prefix_candidates() {
        assert_eq!(issue_prefix_candidate("Paper Clip", 1), "PAP");
        assert_eq!(issue_prefix_candidate("Paper Clip", 2), "PAPA");
        assert_eq!(issue_prefix_candidate("123", 1), "PC");
    }
}
