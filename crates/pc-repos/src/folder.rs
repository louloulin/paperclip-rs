use crate::Db;
use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct FolderRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub kind: String,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub system_key: Option<String>,
    pub color: Option<String>,
    pub position: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
const COLS: &str = "id, company_id, kind, parent_id, name, slug, system_key, color, position, created_at, updated_at";
pub struct FolderRepo<'a> {
    pub db: &'a Db,
}
impl<'a> FolderRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list_by_company(&self, c: Uuid) -> sqlx::Result<Vec<FolderRow>> {
        let s = format!("SELECT {COLS} FROM folders WHERE company_id=$1 ORDER BY position, name");
        sqlx::query_as::<_, FolderRow>(&s)
            .bind(c)
            .fetch_all(self.db.pool())
            .await
    }
    pub async fn create(
        &self,
        c: Uuid,
        kind: &str,
        name: &str,
        slug: &str,
    ) -> sqlx::Result<FolderRow> {
        let s = format!("INSERT INTO folders (company_id, kind, name, slug) VALUES ($1,$2,$3,$4) RETURNING {COLS}");
        sqlx::query_as::<_, FolderRow>(&s)
            .bind(c)
            .bind(kind)
            .bind(name)
            .bind(slug)
            .fetch_one(self.db.pool())
            .await
    }
    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        Ok(sqlx::query("DELETE FROM folders WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected()
            > 0)
    }
}
