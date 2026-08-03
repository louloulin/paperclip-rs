//! `case` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CaseRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub case_number: i32,
    pub identifier: String,
    pub case_type: String,
    pub key: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub status: String,
    pub fields: serde_json::Value,
    pub parent_case_id: Option<Uuid>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub completed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, project_id, case_number, identifier, case_type, key, title, summary, status, fields, parent_case_id, created_by_agent_id, created_by_user_id, completed_at, created_at, updated_at";

pub struct CaseRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CaseRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<CaseRow>> {
        let sql = format!(
            "SELECT {COLS} FROM cases WHERE company_id = $1 ORDER BY created_at DESC LIMIT 200"
        );
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    /// 列出全部（跨公司）；limit 默认 200。
    pub async fn list_all(&self, limit: i64) -> sqlx::Result<Vec<CaseRow>> {
        let sql = format!("SELECT {COLS} FROM cases ORDER BY created_at DESC LIMIT $1");
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<CaseRow>> {
        let sql = format!("SELECT {COLS} FROM cases WHERE id = $1");
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        case_type: &str,
        title: &str,
        project_id: Option<Uuid>,
        summary: Option<&str>,
    ) -> sqlx::Result<CaseRow> {
        // 用 (company_id, max(case_number)+1) 与 CASE-<uuid> 保证唯一约束
        let next_number: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(case_number), 0) + 1 FROM cases WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        let identifier = format!("CASE-{}", Uuid::new_v4().simple());
        let sql = format!(
            "INSERT INTO cases (company_id, case_type, title, project_id, summary, case_number, identifier) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING {COLS}"
        );
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(company_id)
            .bind(case_type)
            .bind(title)
            .bind(project_id)
            .bind(summary)
            .bind(next_number)
            .bind(&identifier)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn update(
        &self,
        id: Uuid,
        title: Option<&str>,
        summary: Option<&str>,
        status: Option<&str>,
    ) -> sqlx::Result<Option<CaseRow>> {
        let sql = format!(
            "UPDATE cases SET title=COALESCE($2,title), summary=COALESCE($3,summary), \
             status=COALESCE($4,status), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(id)
            .bind(title)
            .bind(summary)
            .bind(status)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM cases WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
