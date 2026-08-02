//! inbox_dismissals 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxItem {
    pub id: Uuid,
    pub company_id: Uuid,
    pub user_id: Option<String>,
    pub kind: String,
    pub summary: String,
    pub dismissed: bool,
    pub created_at: Timestamp,
}

pub struct InboxRepo<'a> { pub db: &'a Db }

impl<'a> InboxRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_for_user(&self, company_id: Uuid, user_id: &str) -> sqlx::Result<Vec<InboxItem>> {
        sqlx::query_as::<_, InboxItem>(
            "SELECT id, company_id, user_id, kind, summary, dismissed, created_at FROM inbox_dismissals WHERE company_id = $1 AND user_id = $2 ORDER BY created_at DESC",
        ).bind(company_id).bind(user_id).fetch_all(self.db.pool()).await
    }
}
