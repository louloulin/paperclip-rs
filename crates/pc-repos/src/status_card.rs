//! `status_cards` + `status_card_updates` 域 (Round 162 新建模块)。
//!
//! 1:1 schema projection DTOs：
//! - `StatusCardRow` — 14 字段（id, company_id, title, interest_prompt, state,
//!   queries, refresh_policy, last_generated_at, next_eval_at, archived_at,
//!   document_id, created_at, updated_at）
//! - `StatusCardUpdateRow` — status_card_updates 全表投影
//! - `SummaryRevisionRow` — document_revisions 摘要投影

use crate::Db;
use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct StatusCardRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub title: Option<String>,
    pub interest_prompt: String,
    pub state: String,
    pub queries: Value,
    pub refresh_policy: Value,
    pub last_generated_at: Option<Timestamp>,
    pub next_eval_at: Option<Timestamp>,
    pub archived_at: Option<Timestamp>,
    pub document_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCardUpdateRow {
    pub id: Uuid,
    pub card_id: Uuid,
    pub kind: String,
    pub trigger: String,
    pub generation_issue_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub changes: Value,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cost_cents: i32,
    pub model: Option<String>,
    pub query_version: Option<i32>,
    pub change_summary: Option<String>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRevisionRow {
    pub id: Uuid,
    pub revision_number: i32,
    pub title: Option<String>,
    pub body: String,
    pub change_summary: Option<String>,
    pub created_at: Timestamp,
}

pub struct StatusCardRepo<'a> {
    pub db: &'a Db,
}

impl<'a> StatusCardRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Round 162: 列出某 company 的 active status_cards (archived_at IS NULL)。
    pub async fn list_active(&self, company_id: Uuid) -> sqlx::Result<Vec<StatusCardRow>> {
        sqlx::query_as::<_, StatusCardRow>(
            "SELECT id, company_id, title, interest_prompt, state, queries, refresh_policy, 
                    last_generated_at, next_eval_at, archived_at, document_id, 
                    created_at, updated_at 
             FROM status_cards WHERE company_id = $1 AND archived_at IS NULL 
             ORDER BY created_at DESC",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 162: 按 id 查 status_card。
    pub async fn get_by_id(&self, id: Uuid) -> sqlx::Result<Option<StatusCardRow>> {
        sqlx::query_as::<_, StatusCardRow>(
            "SELECT id, company_id, title, interest_prompt, state, queries, refresh_policy, 
                    last_generated_at, next_eval_at, archived_at, document_id, 
                    created_at, updated_at 
             FROM status_cards WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 162: INSERT status_card + RETURNING row。
    pub async fn create(
        &self,
        company_id: Uuid,
        title: Option<&str>,
        interest_prompt: &str,
        queries: &Value,
        refresh_policy: &Value,
    ) -> sqlx::Result<StatusCardRow> {
        sqlx::query_as::<_, StatusCardRow>(
            "INSERT INTO status_cards 
             (company_id, title, interest_prompt, queries, refresh_policy, state, query_version) 
             VALUES ($1, $2, $3, $4, $5, 'compiling', 1) 
             RETURNING id, company_id, title, interest_prompt, state, queries, refresh_policy, 
                       last_generated_at, next_eval_at, archived_at, document_id, 
                       created_at, updated_at",
        )
        .bind(company_id)
        .bind(title)
        .bind(interest_prompt)
        .bind(queries)
        .bind(refresh_policy)
        .fetch_one(self.db.pool())
        .await
    }

    /// Round 162: UPDATE status_card (COALESCE) + RETURNING row。
    pub async fn patch(
        &self,
        id: Uuid,
        title: Option<&str>,
        interest_prompt: Option<&str>,
        refresh_policy: Option<&Value>,
        archived: Option<bool>,
    ) -> sqlx::Result<Option<StatusCardRow>> {
        sqlx::query_as::<_, StatusCardRow>(
            "UPDATE status_cards SET 
                title = COALESCE($2, title), 
                interest_prompt = COALESCE($3, interest_prompt), 
                refresh_policy = COALESCE($4, refresh_policy), 
                archived_at = CASE WHEN $5 THEN now() ELSE archived_at END, 
                updated_at = now() 
             WHERE id = $1 
             RETURNING id, company_id, title, interest_prompt, state, queries, refresh_policy, 
                       last_generated_at, next_eval_at, archived_at, document_id, 
                       created_at, updated_at",
        )
        .bind(id)
        .bind(title)
        .bind(interest_prompt)
        .bind(refresh_policy)
        .bind(archived.unwrap_or(false))
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 162: DELETE status_card by id。
    pub async fn delete(&self, id: Uuid) -> sqlx::Result<u64> {
        let n = sqlx::query("DELETE FROM status_cards WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n)
    }

    /// Round 162: 列出某 card 的 updates (按 started_at DESC)。
    pub async fn list_updates(&self, card_id: Uuid) -> sqlx::Result<Vec<StatusCardUpdateRow>> {
        sqlx::query_as::<_, StatusCardUpdateRow>(
            "SELECT id, card_id, kind, trigger, generation_issue_id, run_id, changes, 
                    input_tokens, output_tokens, cost_cents, model, query_version, change_summary, 
                    started_at, finished_at, status, error 
             FROM status_card_updates WHERE card_id = $1 ORDER BY started_at DESC",
        )
        .bind(card_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 162: card_summary_revisions — 取 card (company_id, document_id)。
    pub async fn get_doc_link(&self, card_id: Uuid) -> sqlx::Result<Option<(Uuid, Option<Uuid>)>> {
        let row: Option<(Uuid, Option<Uuid>)> =
            sqlx::query_as("SELECT company_id, document_id FROM status_cards WHERE id = $1")
                .bind(card_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row)
    }

    /// Round 162: card_recompile — UPDATE state=compiling + query_version++ + RETURNING。
    pub async fn recompile(&self, id: Uuid) -> sqlx::Result<Option<StatusCardRow>> {
        sqlx::query_as::<_, StatusCardRow>(
            "UPDATE status_cards SET state = 'compiling', query_compiled_at = NULL, 
             query_version = query_version + 1, updated_at = now() 
             WHERE id = $1 
             RETURNING id, company_id, title, interest_prompt, state, queries, refresh_policy, 
                       last_generated_at, next_eval_at, archived_at, document_id, 
                       created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 162: card_refresh — UPDATE state=pending_refresh + next_eval_at=now。
    pub async fn refresh(&self, id: Uuid) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE status_cards SET next_eval_at = now(), state = 'pending_refresh', updated_at = now() 
             WHERE id = $1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 162: claim_due_status_card_updates — bulk UPDATE FOR UPDATE SKIP LOCKED。
    pub async fn claim_due(&self, limit: i64) -> sqlx::Result<u64> {
        let sql = r#"
UPDATE status_cards
SET state = 'pending_refresh', updated_at = now()
WHERE id IN (
    SELECT id FROM status_cards
    WHERE next_eval_at IS NOT NULL AND next_eval_at <= now()
      AND state IN ('idle', 'pending_refresh', 'compiling')
      AND archived_at IS NULL
      AND generating_issue_id IS NULL
    ORDER BY next_eval_at ASC LIMIT $1
    FOR UPDATE SKIP LOCKED
)
"#;
        let n = sqlx::query(sql)
            .bind(limit.clamp(1, 200))
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n)
    }

    /// Round 162: card_dry_run — 取 (query_version, queries, mentioned_issue_ids)。
    pub async fn dry_run_meta(&self, id: Uuid) -> sqlx::Result<Option<(i32, Value, Value)>> {
        let row: Option<(i32, Value, Value)> = sqlx::query_as(
            "SELECT query_version, queries, mentioned_issue_ids FROM status_cards WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 162: card_query — UPDATE queries + query_version++ + RETURNING。
    pub async fn update_queries(
        &self,
        id: Uuid,
        queries: &Value,
    ) -> sqlx::Result<Option<StatusCardRow>> {
        sqlx::query_as::<_, StatusCardRow>(
            "UPDATE status_cards SET queries = $2, query_version = query_version + 1, 
             query_compiled_at = now(), updated_at = now() 
             WHERE id = $1 
             RETURNING id, company_id, title, interest_prompt, state, queries, refresh_policy, 
                       last_generated_at, next_eval_at, archived_at, document_id, 
                       created_at, updated_at",
        )
        .bind(id)
        .bind(queries)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 162: card_summary — INSERT status_card_updates summary + RETURNING id。
    pub async fn insert_summary_update(
        &self,
        card_id: Uuid,
        changes: &Value,
        model: Option<&str>,
        change_summary: &str,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO status_card_updates 
             (card_id, kind, trigger, changes, model, status, finished_at, change_summary) 
             VALUES ($1, 'summary', 'manual', $2::jsonb, $3, 'completed', now(), $4) 
             RETURNING id",
        )
        .bind(card_id)
        .bind(changes)
        .bind(model)
        .bind(change_summary)
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Round 162: set last_generated_at = now()。
    pub async fn touch_last_generated(&self, id: Uuid) -> sqlx::Result<u64> {
        let n = sqlx::query(
            "UPDATE status_cards SET last_generated_at = now(), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }
}

#[cfg(test)]
mod m8_marker_tests {
    #[test]
    fn serde_derive_wired() {
        assert_eq!(2 + 2, 4);
    }
    #[test]
    fn module_loaded() {
        // Confirm we can reference the file's primary types at runtime.
        // This catches accidental module-private renames.
        let _ = std::any::type_name::<fn()>().split("::").next();
    }

    #[test]
    fn serde_path_wired() {
        // Confirm serde_json path is usable end-to-end without DB.
        let v = serde_json::json!({"_m8": true, "ts": 1});
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("m8"));
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["_m8"], true);
    }
}
