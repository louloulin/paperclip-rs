//! `inbox_dismissals` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct InboxRepo<'a> {
    pub db: &'a Db,
}

impl<'a> InboxRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list_for_user(
        &self,
        company_id: Uuid,
        _user_id: &str,
    ) -> sqlx::Result<Vec<InboxRow>> {
        sqlx::query_as::<_, InboxRow>(
            "SELECT id, company_id, kind AS name, '' AS status, created_at, created_at AS updated_at FROM inbox_dismissals WHERE company_id = $1 ORDER BY created_at DESC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<InboxRow>> {
        self.list_for_user(company_id, "").await
    }
}
