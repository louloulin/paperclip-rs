//! sidebar preferences 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSidebarPreference {
    pub id: Uuid,
    pub user_id: String,
    pub company_order: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompanySidebarPreference {
    pub id: Uuid,
    pub company_id: Uuid,
    pub user_id: String,
    pub project_order: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct SidebarRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SidebarRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn get_company_order(
        &self,
        user_id: &str,
    ) -> sqlx::Result<Option<UserSidebarPreference>> {
        sqlx::query_as(
            "SELECT id, user_id, company_order, created_at, updated_at FROM user_sidebar_preferences WHERE user_id=$1",
        )
        .bind(user_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn get(&self, user_id: &str) -> sqlx::Result<Option<UserSidebarPreference>> {
        self.get_company_order(user_id).await
    }

    pub async fn upsert_company_order(
        &self,
        user_id: &str,
        ordered_ids: &[String],
    ) -> sqlx::Result<UserSidebarPreference> {
        sqlx::query_as(
            "INSERT INTO user_sidebar_preferences (user_id, company_order) VALUES ($1,$2) \
             ON CONFLICT (user_id) DO UPDATE SET company_order=EXCLUDED.company_order, updated_at=now() \
             RETURNING id, user_id, company_order, created_at, updated_at",
        )
        .bind(user_id)
        .bind(serde_json::json!(ordered_ids))
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn get_project_order(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Option<CompanySidebarPreference>> {
        sqlx::query_as(
            "SELECT id, company_id, user_id, project_order, created_at, updated_at \
             FROM company_user_sidebar_preferences WHERE company_id=$1 AND user_id=$2",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn upsert_project_order(
        &self,
        company_id: Uuid,
        user_id: &str,
        ordered_ids: &[String],
    ) -> sqlx::Result<CompanySidebarPreference> {
        sqlx::query_as(
            "INSERT INTO company_user_sidebar_preferences (company_id, user_id, project_order) \
             VALUES ($1,$2,$3) ON CONFLICT (company_id,user_id) DO UPDATE SET \
             project_order=EXCLUDED.project_order, updated_at=now() \
             RETURNING id, company_id, user_id, project_order, created_at, updated_at",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(serde_json::json!(ordered_ids))
        .fetch_one(self.db.pool())
        .await
    }
}

pub fn normalize_ordered_ids(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !normalized.iter().any(|existing| existing == value) {
            normalized.push(value.to_owned());
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_sidebar_order() {
        assert_eq!(
            normalize_ordered_ids(vec![" a ".into(), String::new(), "a".into(), "b".into(),]),
            vec!["a", "b"]
        );
    }
}
