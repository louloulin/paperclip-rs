//! company_skills 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompanySkill {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub body: String,
    pub enabled: bool,
    pub updated_at: Timestamp,
}

pub struct SkillRepo<'a> { pub db: &'a Db }

impl<'a> SkillRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<CompanySkill>> {
        sqlx::query_as::<_, CompanySkill>(
            "SELECT id, company_id, name, body, enabled, updated_at FROM company_skills WHERE company_id = $1 ORDER BY name ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
