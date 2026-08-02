//! `company` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

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
    pub fn new(db: &'a Db) -> Self { Self { db } }

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

    pub async fn create(&self, name: &str, description: Option<&str>) -> sqlx::Result<CompanyRow> {
        sqlx::query_as::<_, CompanyRow>(
            "INSERT INTO companies (name, description) VALUES ($1, $2) \
             RETURNING id, name, description, status, pause_reason, paused_at, \
                       issue_prefix, issue_counter, budget_monthly_cents, spent_monthly_cents, \
                       attachment_max_bytes, default_responsible_user_id, \
                       require_board_approval_for_new_agents, feedback_data_sharing_enabled, \
                       feedback_data_sharing_consent_at, feedback_data_sharing_consent_by_user_id, \
                       feedback_data_sharing_terms_version, brand_color, created_at, updated_at",
        )
        .bind(name)
        .bind(description)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn update(&self, id: Uuid, name: Option<&str>, description: Option<&str>, status: Option<&str>) -> sqlx::Result<Option<CompanyRow>> {
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
        .bind(id).bind(name).bind(description).bind(status)
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

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM companies WHERE id = $1").bind(id)
            .execute(self.db.pool()).await?;
        Ok(r.rows_affected() > 0)
    }
}
