//! `approval` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ApprovalRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub approval_type: String,
    pub requested_by_agent_id: Option<Uuid>,
    pub requested_by_user_id: Option<String>,
    pub status: String,
    pub payload: serde_json::Value,
    pub decision_note: Option<String>,
    pub decided_by_user_id: Option<String>,
    pub decided_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, type AS approval_type, requested_by_agent_id, \
    requested_by_user_id, status, payload, decision_note, decided_by_user_id, decided_at, \
    created_at, updated_at";

pub struct ApprovalRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ApprovalRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<ApprovalRow>> {
        let sql =
            format!("SELECT {COLS} FROM approvals WHERE company_id = $1 ORDER BY created_at DESC");
        sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<ApprovalRow>> {
        let sql = format!("SELECT {COLS} FROM approvals WHERE id = $1");
        sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        approval_type: &str,
        payload: serde_json::Value,
    ) -> sqlx::Result<ApprovalRow> {
        let sql = format!(
            "INSERT INTO approvals (company_id, type, payload) VALUES ($1,$2,$3) RETURNING {COLS}"
        );
        sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(company_id)
            .bind(approval_type)
            .bind(payload)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn decide(
        &self,
        id: Uuid,
        status: &str,
        note: Option<&str>,
        decided_by: &str,
    ) -> sqlx::Result<Option<ApprovalRow>> {
        let sql = format!(
            "UPDATE approvals SET status=$2, decision_note=$3, decided_by_user_id=$4, decided_at=now(), updated_at=now() \
             WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, ApprovalRow>(&sql)
            .bind(id)
            .bind(status)
            .bind(note)
            .bind(decided_by)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM approvals WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
