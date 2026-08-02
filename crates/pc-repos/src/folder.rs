//! folders 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct Folder {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct FolderRepo<'a> { pub db: &'a Db }

impl<'a> FolderRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Folder>> {
        sqlx::query_as::<_, Folder>(
            "SELECT id, company_id, name, parent_id, created_at, updated_at FROM folders WHERE company_id = $1 ORDER BY name ASC",
        ).bind(company_id).fetch_all(self.db.pool()).await
    }
}
