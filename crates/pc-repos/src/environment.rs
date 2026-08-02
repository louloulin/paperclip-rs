//! environments 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Environment {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub config: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct EnvironmentRepo<'a> { pub db: &'a Db }

impl<'a> EnvironmentRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Environment>> {
        sqlx::query_as::<_, Environment>(
            "SELECT id, company_id, name, kind, config, created_at, updated_at FROM environments WHERE company_id = $1 ORDER BY created_at ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
