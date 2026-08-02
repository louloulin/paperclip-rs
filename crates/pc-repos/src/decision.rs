//! decisions 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    pub id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub rationale: Option<String>,
    pub status: String,
    pub made_by_user_id: Option<String>,
    pub made_by_agent_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct DecisionRepo<'a> { pub db: &'a Db }

impl<'a> DecisionRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Decision>> {
        sqlx::query_as::<_, Decision>(
            "SELECT id, company_id, title, rationale, status, made_by_user_id, made_by_agent_id, created_at, updated_at FROM decisions WHERE company_id = $1 ORDER BY created_at DESC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
