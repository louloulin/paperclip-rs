use crate::Db;
use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct DocumentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub title: Option<String>,
    pub format: String,
    pub latest_body: String,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub locked_at: Option<Timestamp>,
    pub locked_by_agent_id: Option<Uuid>,
    pub locked_by_user_id: Option<String>,
    pub source_trust: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
const COLS: &str = "id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id, locked_at, locked_by_agent_id, locked_by_user_id, source_trust, created_at, updated_at";
pub struct DocumentRepo<'a> {
    pub db: &'a Db,
}
impl<'a> DocumentRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list_by_company(&self, c: Uuid) -> sqlx::Result<Vec<DocumentRow>> {
        let s = format!(
            "SELECT {COLS} FROM documents WHERE company_id=$1 ORDER BY updated_at DESC LIMIT 200"
        );
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(c)
            .fetch_all(self.db.pool())
            .await
    }
    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<DocumentRow>> {
        let s = format!("SELECT {COLS} FROM documents WHERE id=$1");
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }
    pub async fn create(
        &self,
        c: Uuid,
        title: Option<&str>,
        body: &str,
    ) -> sqlx::Result<DocumentRow> {
        let s = format!("INSERT INTO documents (company_id, title, latest_body) VALUES ($1,$2,$3) RETURNING {COLS}");
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(c)
            .bind(title)
            .bind(body)
            .fetch_one(self.db.pool())
            .await
    }
    pub async fn update(
        &self,
        id: Uuid,
        title: Option<&str>,
        body: Option<&str>,
    ) -> sqlx::Result<Option<DocumentRow>> {
        let s = format!("UPDATE documents SET title=COALESCE($2,title), latest_body=COALESCE($3,latest_body), latest_revision_number=latest_revision_number+1, updated_at=now() WHERE id=$1 RETURNING {COLS}");
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(id)
            .bind(title)
            .bind(body)
            .fetch_optional(self.db.pool())
            .await
    }
    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        Ok(sqlx::query("DELETE FROM documents WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected()
            > 0)
    }
}
