//! `issue_reference_mentions` 域 — issue 间自动引用提及持久化。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

pub const SOURCE_KIND_TITLE: &str = "title";
pub const SOURCE_KIND_DESCRIPTION: &str = "description";
pub const SOURCE_KIND_DOCUMENT: &str = "document";
pub const SOURCE_KIND_COMMENT: &str = "comment";

pub const LIST_COLS: &str = "id, company_id, source_issue_id, target_issue_id, source_kind,     source_record_id, document_key, matched_text, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReferenceMentionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub target_issue_id: Uuid,
    pub source_kind: String,
    pub source_record_id: Option<Uuid>,
    pub document_key: Option<String>,
    pub matched_text: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct NewIssueReferenceMention<'a> {
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub target_issue_id: Uuid,
    pub source_kind: &'a str,
    pub source_record_id: Option<Uuid>,
    pub document_key: Option<&'a str>,
    pub matched_text: Option<&'a str>,
}

pub struct IssueReferenceMentionRepo<'a> {
    pub db: &'a Db,
}

impl<'a> IssueReferenceMentionRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn insert(
        &self,
        m: &NewIssueReferenceMention<'_>,
    ) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO issue_reference_mentions                 (company_id, source_issue_id, target_issue_id, source_kind,                  source_record_id, document_key, matched_text)              VALUES ($1,$2,$3,$4,$5,$6,$7)              RETURNING id",
        )
        .bind(m.company_id)
        .bind(m.source_issue_id)
        .bind(m.target_issue_id)
        .bind(m.source_kind)
        .bind(m.source_record_id)
        .bind(m.document_key)
        .bind(m.matched_text)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn delete_for_source(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
        source_kind: &str,
        source_record_id: Option<Uuid>,
    ) -> sqlx::Result<u64> {
        let n = if let Some(record_id) = source_record_id {
            sqlx::query(
                "DELETE FROM issue_reference_mentions WHERE company_id = $1                  AND source_issue_id = $2 AND source_kind = $3 AND source_record_id = $4",
            )
            .bind(company_id)
            .bind(source_issue_id)
            .bind(source_kind)
            .bind(record_id)
            .execute(self.db.pool())
            .await?
            .rows_affected()
        } else {
            sqlx::query(
                "DELETE FROM issue_reference_mentions WHERE company_id = $1                  AND source_issue_id = $2 AND source_kind = $3 AND source_record_id IS NULL",
            )
            .bind(company_id)
            .bind(source_issue_id)
            .bind(source_kind)
            .execute(self.db.pool())
            .await?
            .rows_affected()
        };
        Ok(n)
    }

    pub async fn delete_for_source_tx(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
        source_kind: &str,
        source_record_id: Option<Uuid>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> sqlx::Result<u64> {
        let n = if let Some(record_id) = source_record_id {
            sqlx::query(
                "DELETE FROM issue_reference_mentions WHERE company_id = $1                  AND source_issue_id = $2 AND source_kind = $3 AND source_record_id = $4",
            )
            .bind(company_id)
            .bind(source_issue_id)
            .bind(source_kind)
            .bind(record_id)
            .execute(&mut **tx)
            .await?
            .rows_affected()
        } else {
            sqlx::query(
                "DELETE FROM issue_reference_mentions WHERE company_id = $1                  AND source_issue_id = $2 AND source_kind = $3 AND source_record_id IS NULL",
            )
            .bind(company_id)
            .bind(source_issue_id)
            .bind(source_kind)
            .execute(&mut **tx)
            .await?
            .rows_affected()
        };
        Ok(n)
    }

    pub async fn insert_in_tx(
        &self,
        m: &NewIssueReferenceMention<'_>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO issue_reference_mentions                 (company_id, source_issue_id, target_issue_id, source_kind,                  source_record_id, document_key, matched_text)              VALUES ($1,$2,$3,$4,$5,$6,$7)              RETURNING id",
        )
        .bind(m.company_id)
        .bind(m.source_issue_id)
        .bind(m.target_issue_id)
        .bind(m.source_kind)
        .bind(m.source_record_id)
        .bind(m.document_key)
        .bind(m.matched_text)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn list_for_source(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueReferenceMentionRow>> {
        let sql = format!(
            "SELECT {LIST_COLS} FROM issue_reference_mentions              WHERE company_id = $1 AND source_issue_id = $2              ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, IssueReferenceMentionRow>(&sql)
            .bind(company_id)
            .bind(source_issue_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_for_target(
        &self,
        company_id: Uuid,
        target_issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueReferenceMentionRow>> {
        let sql = format!(
            "SELECT {LIST_COLS} FROM issue_reference_mentions              WHERE company_id = $1 AND target_issue_id = $2              ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, IssueReferenceMentionRow>(&sql)
            .bind(company_id)
            .bind(target_issue_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn count_for_source(&self, company_id: Uuid, source_issue_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT target_issue_id) FROM issue_reference_mentions              WHERE company_id = $1 AND source_issue_id = $2",
        )
        .bind(company_id)
        .bind(source_issue_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }

    pub async fn count_for_target(&self, company_id: Uuid, target_issue_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT source_issue_id) FROM issue_reference_mentions              WHERE company_id = $1 AND target_issue_id = $2",
        )
        .bind(company_id)
        .bind(target_issue_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }

    pub async fn resolve_identifiers(
        &self,
        company_id: Uuid,
        identifiers: &[String],
    ) -> sqlx::Result<Vec<(Uuid, String)>> {
        if identifiers.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, identifier FROM issues              WHERE company_id = $1 AND identifier IS NOT NULL              AND identifier = ANY($2)",
        )
        .bind(company_id)
        .bind(identifiers)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_cols_contains_expected() {
        for col in ["id", "company_id", "source_issue_id", "target_issue_id", "source_kind"] {
            assert!(LIST_COLS.contains(col), "missing column {col}");
        }
    }
}
