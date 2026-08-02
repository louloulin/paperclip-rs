//! companies 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Company {
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

#[derive(Debug, Clone)]
pub struct NewCompany {
    pub name: String,
    pub description: Option<String>,
    pub issue_prefix: String,
    pub budget_monthly_cents: i32,
    pub attachment_max_bytes: i32,
}

const SELECT: &str = "id, name, description, status, pause_reason, paused_at, issue_prefix, issue_counter, budget_monthly_cents, spent_monthly_cents, attachment_max_bytes, default_responsible_user_id, require_board_approval_for_new_agents, feedback_data_sharing_enabled, feedback_data_sharing_consent_at, feedback_data_sharing_consent_by_user_id, feedback_data_sharing_terms_version, brand_color, created_at, updated_at";

pub struct CompanyRepo<'a> { pub db: &'a Db }

impl<'a> CompanyRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list(&self) -> RepoResult<Vec<Company>> {
        let sql = format!("SELECT {SELECT} FROM companies ORDER BY created_at ASC");
        Ok(sqlx::query_as::<_, Company>(&sql).fetch_all(self.db.pool()).await?)
    }
    pub async fn find(&self, id: Uuid) -> RepoResult<Option<Company>> {
        let sql = format!("SELECT {SELECT} FROM companies WHERE id = $1");
        Ok(sqlx::query_as::<_, Company>(&sql).bind(id).fetch_optional(self.db.pool()).await?)
    }
    pub async fn create(&self, new: NewCompany) -> RepoResult<Company> {
        if new.name.trim().is_empty() {
            return Err(RepoError::Invalid("company name cannot be empty".into()));
        }
        let sql = format!("INSERT INTO companies (name, description, issue_prefix, budget_monthly_cents, attachment_max_bytes) VALUES ($1, $2, $3, $4, $5) RETURNING {SELECT}");
        Ok(sqlx::query_as::<_, Company>(&sql)
            .bind(new.name)
            .bind(new.description)
            .bind(new.issue_prefix)
            .bind(new.budget_monthly_cents)
            .bind(new.attachment_max_bytes)
            .fetch_one(self.db.pool())
            .await?)
    }
    pub async fn pause(&self, id: Uuid, reason: &str) -> RepoResult<Company> {
        let sql = format!("UPDATE companies SET status = 'paused', pause_reason = $2, paused_at = now(), updated_at = now() WHERE id = $1 RETURNING {SELECT}");
        sqlx::query_as::<_, Company>(&sql).bind(id).bind(reason).fetch_optional(self.db.pool()).await?
            .ok_or(RepoError::NotFound { entity: "company", id: id.to_string() })
    }
}
