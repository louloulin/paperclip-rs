//! `goal` 域。
use crate::Db;
use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct GoalRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub level: String,
    pub status: String,
    pub parent_id: Option<Uuid>,
    pub owner_agent_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, title, description, level, status, parent_id, owner_agent_id, created_at, updated_at";

pub struct GoalRepo<'a> {
    pub db: &'a Db,
}
impl<'a> GoalRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list_by_company(&self, c: Uuid) -> sqlx::Result<Vec<GoalRow>> {
        let s = format!("SELECT {COLS} FROM goals WHERE company_id = $1 ORDER BY created_at DESC");
        sqlx::query_as::<_, GoalRow>(&s)
            .bind(c)
            .fetch_all(self.db.pool())
            .await
    }
    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<GoalRow>> {
        let s = format!("SELECT {COLS} FROM goals WHERE id = $1");
        sqlx::query_as::<_, GoalRow>(&s)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }
    pub async fn create(
        &self,
        c: Uuid,
        t: &str,
        d: Option<&str>,
        owner: Option<Uuid>,
    ) -> sqlx::Result<GoalRow> {
        let s = format!("INSERT INTO goals (company_id, title, description, owner_agent_id) VALUES ($1,$2,$3,$4) RETURNING {COLS}");
        sqlx::query_as::<_, GoalRow>(&s)
            .bind(c)
            .bind(t)
            .bind(d)
            .bind(owner)
            .fetch_one(self.db.pool())
            .await
    }
    pub async fn update(
        &self,
        id: Uuid,
        t: Option<&str>,
        s: Option<&str>,
    ) -> sqlx::Result<Option<GoalRow>> {
        let sql = format!("UPDATE goals SET title=COALESCE($2,title), status=COALESCE($3,status), updated_at=now() WHERE id=$1 RETURNING {COLS}");
        sqlx::query_as::<_, GoalRow>(&sql)
            .bind(id)
            .bind(t)
            .bind(s)
            .fetch_optional(self.db.pool())
            .await
    }
    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        Ok(sqlx::query("DELETE FROM goals WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected()
            > 0)
    }
}
