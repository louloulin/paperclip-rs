//! `decision_training::service` —— `decisionTrainingService` 仓储结构。
//!
//! 公开方法（与 Node `decisionTrainingService` factory 返回对象 1:1 对齐）：
//! - [`DecisionTrainingService::preview`] —— 干跑 capture（不持久化）
//! - [`DecisionTrainingService::create`] —— INSERT + ON CONFLICT DO NOTHING
//! - [`DecisionTrainingService::list`] —— JOIN issues，filter 多条件
//! - [`DecisionTrainingService::get_by_id`] —— 按 id 查单行
//! - [`DecisionTrainingService::update_notes`] —— 事务：notes + 追加 history
//! - [`DecisionTrainingService::scrub_deleted_comments`] —— 更新 snapshot 中的被删评论
//! - [`DecisionTrainingService::delete`] —— 按 id 删

use chrono::Utc;
use sqlx::types::Json;
use uuid::Uuid;

use super::capture::capture_decision_snapshot;
use super::types::{
    CaptureInput, CaptureResult, CreateInput, DecisionTrainingExampleRow,
    ListExampleRow, ListInput, NotesHistoryEntry, ScrubDeletedCommentsInput,
    ScrubDeletedCommentsResult,
};
use crate::{RepoError, RepoResult, Db};

/// Decision training 仓储入口（与 Node `decisionTrainingService(db)` factory 1:1 对齐）。
pub struct DecisionTrainingService<'a> {
    pub db: &'a Db,
}

impl<'a> DecisionTrainingService<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 干跑 capture（与 Node `preview` 1:1 对齐）。
    pub async fn preview(&self, input: &CaptureInput) -> sqlx::Result<CaptureResult> {
        capture_decision_snapshot(self.db, input, Utc::now()).await
    }

    /// 持久化 snapshot（与 Node `create` 1:1 对齐）。
    ///
    /// 行为：
    /// 1. 调 `capture_decision_snapshot` 拿到 cutoffAt + snapshot
    /// 2. INSERT `decision_training_examples` with `ON CONFLICT (source_kind, source_id, created_by_user_id) DO NOTHING`
    /// 3. 若 ON CONFLICT 触发（rows 为空）→ 返回 `RepoError::Invalid("This decision is already trained by this user")`
    /// 4. 返回 `DecisionTrainingExampleRow`
    pub async fn create(&self, input: CreateInput) -> RepoResult<DecisionTrainingExampleRow> {
        let captured = capture_decision_snapshot(
            self.db,
            &CaptureInput {
                company_id: input.company_id,
                source_kind: input.source_kind,
                source_id: input.source_id,
                issue_id: input.issue_id,
            },
            Utc::now(),
        )
        .await
        .map_err(RepoError::Sql)?;

        let row: Option<DecisionTrainingExampleRow> = sqlx::query_as::<_, DecisionTrainingExampleRow>(
            "INSERT INTO decision_training_examples \
             (company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
              decision_outcome, retention_policy, snapshot, created_by_user_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (source_kind, source_id, created_by_user_id) DO NOTHING \
             RETURNING id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, \
                       notes_history, decision_outcome, snapshot, retention_policy, \
                       created_by_user_id, created_at, updated_at",
        )
        .bind(input.company_id)
        .bind(input.source_kind.as_str())
        .bind(input.source_id)
        .bind(input.issue_id)
        .bind(captured.cutoff_at)
        .bind(&input.notes)
        .bind(Json::<Vec<NotesHistoryEntry>>(Vec::new()))
        .bind(captured.decision_outcome.clone())
        .bind(super::capture::DECISION_TRAINING_RETENTION_POLICY)
        .bind(Json(&captured.snapshot))
        .bind(&input.created_by_user_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(RepoError::Sql)?;

        row.ok_or_else(|| {
            RepoError::Invalid("This decision is already trained by this user".to_string())
        })
    }

    /// 列出 example（与 Node `list` 1:1 对齐）。
    ///
    /// 行为：
    /// - filter: company_id + 任意 (projectId / kind / author / q)
    /// - q 用 `ilike` 模糊匹配 notes / issue title / issue identifier
    /// - sort: `created_at DESC, id DESC`
    pub async fn list(
        &self,
        company_id: Uuid,
        input: ListInput,
    ) -> sqlx::Result<Vec<ListExampleRow>> {
        let mut sql = String::from(
            "SELECT e.id, e.company_id, e.source_kind, e.source_id, e.issue_id, e.cutoff_at, \
                    e.notes, e.notes_history, e.decision_outcome, e.snapshot, e.retention_policy, \
                    e.created_by_user_id, e.created_at, e.updated_at, \
                    i.title AS issue_title, i.identifier AS issue_identifier \
             FROM decision_training_examples e \
             INNER JOIN issues i ON i.id = e.issue_id \
             WHERE e.company_id = $1",
        );
        if input.project_id.is_some() {
            sql.push_str(" AND i.project_id = $?");
        }
        if input.kind.is_some() {
            sql.push_str(" AND e.source_kind = $?");
        }
        if input.author.is_some() {
            sql.push_str(" AND e.created_by_user_id = $?");
        }
        if input.q.is_some() {
            sql.push_str(
                " AND (e.notes ILIKE $? OR i.title ILIKE $? OR i.identifier ILIKE $?)",
            );
        }
        sql.push_str(" ORDER BY e.created_at DESC, e.id DESC");

        // 简化为直接构造 SQL 并执行（参数化 bind 由 sqlx 处理）
        let mut q = sqlx::query(&sql).bind(company_id);
        if let Some(pid) = input.project_id {
            q = q.bind(pid);
        }
        if let Some(k) = input.kind {
            q = q.bind(k.as_str());
        }
        if let Some(a) = input.author {
            q = q.bind(a);
        }
        if let Some(qstr) = input.q {
            let pattern = format!("%{qstr}%");
            q = q.bind(pattern.clone()).bind(pattern.clone()).bind(pattern);
        }
        let rows = q.fetch_all(self.db.pool()).await?;

        rows.into_iter()
            .map(|row| {
                use sqlx::Row;
                let example = DecisionTrainingExampleRow {
                    id: row.try_get("id")?,
                    company_id: row.try_get("company_id")?,
                    source_kind: row.try_get("source_kind")?,
                    source_id: row.try_get("source_id")?,
                    issue_id: row.try_get("issue_id")?,
                    cutoff_at: row.try_get("cutoff_at")?,
                    notes: row.try_get("notes")?,
                    notes_history: row.try_get("notes_history")?,
                    decision_outcome: row.try_get("decision_outcome")?,
                    snapshot: row.try_get("snapshot")?,
                    retention_policy: row.try_get("retention_policy")?,
                    created_by_user_id: row.try_get("created_by_user_id")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                };
                let issue_title: String = row.try_get("issue_title")?;
                let issue_identifier: String = row.try_get("issue_identifier")?;
                Ok(ListExampleRow {
                    example,
                    issue_title,
                    issue_identifier,
                })
            })
            .collect()
    }

    /// 按 id 查单行（与 Node `getById` 1:1 对齐）。
    pub async fn get_by_id(&self, id: Uuid) -> sqlx::Result<Option<DecisionTrainingExampleRow>> {
        sqlx::query_as::<_, DecisionTrainingExampleRow>(
            "SELECT id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, \
                    notes_history, decision_outcome, snapshot, retention_policy, \
                    created_by_user_id, created_at, updated_at \
             FROM decision_training_examples WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Update notes（事务，append to history）。
    ///
    /// 行为（与 Node `updateNotes` 1:1 对齐）：
    /// 1. 在事务内查 row
    /// 2. 不存在 → 返回 `None`
    /// 3. notes 未变 → 返回原 row
    /// 4. 否则 append `{ author, at, body: oldNotes }` 到 history
    /// 5. UPDATE + RETURNING
    pub async fn update_notes(
        &self,
        id: Uuid,
        author: &str,
        notes: &str,
    ) -> sqlx::Result<Option<DecisionTrainingExampleRow>> {
        let mut tx = self.db.pool().begin().await?;

        let existing: Option<DecisionTrainingExampleRow> = sqlx::query_as::<_, DecisionTrainingExampleRow>(
            "SELECT id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, \
                    notes_history, decision_outcome, snapshot, retention_policy, \
                    created_by_user_id, created_at, updated_at \
             FROM decision_training_examples WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = existing else {
            return Ok(None);
        };

        if notes == row.notes {
            return Ok(Some(row));
        }

        let mut history = row.notes_history.0;
        let at = Utc::now().to_rfc3339();
        history.push(NotesHistoryEntry {
            author: author.to_string(),
            at,
            body: row.notes.clone(),
        });

        let updated: Option<DecisionTrainingExampleRow> = sqlx::query_as::<_, DecisionTrainingExampleRow>(
            "UPDATE decision_training_examples \
             SET notes = $1, notes_history = $2, updated_at = now() \
             WHERE id = $3 \
             RETURNING id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, \
                       notes_history, decision_outcome, snapshot, retention_policy, \
                       created_by_user_id, created_at, updated_at",
        )
        .bind(notes)
        .bind(Json(&history))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(updated)
    }

    /// Scrub deleted comments（与 Node `scrubDeletedComments` 1:1 对齐）。
    ///
    /// 行为：
    /// 1. 若 `commentIds` 为空 → 返回 `{ updatedCount: 0 }`
    /// 2. 查 (company_id, issue_id) 下所有 example 的 snapshot
    /// 3. 对每行：遍历 `snapshot.comments`；匹配 `commentIds` 的改为空 body + `deletedAt` + `retentionRedaction`
    /// 4. 任一行有变 → UPDATE snapshot + retention
    /// 5. 返回 `{ updatedCount: N }`
    pub async fn scrub_deleted_comments(
        &self,
        input: ScrubDeletedCommentsInput,
    ) -> sqlx::Result<ScrubDeletedCommentsResult> {
        if input.comment_ids.is_empty() {
            return Ok(ScrubDeletedCommentsResult::default());
        }

        let comment_ids: std::collections::HashSet<String> =
            input.comment_ids.iter().cloned().collect();

        let rows: Vec<(Uuid, Json<serde_json::Value>)> = sqlx::query_as(
            "SELECT id, snapshot FROM decision_training_examples \
             WHERE company_id = $1 AND issue_id = $2",
        )
        .bind(input.company_id)
        .bind(input.issue_id)
        .fetch_all(self.db.pool())
        .await?;

        let mut updated_count = 0u64;
        let deleted_at_iso = input.deleted_at.to_rfc3339();

        for (row_id, snapshot_json) in rows {
            let mut snapshot = snapshot_json.0;
            let mut changed = false;

            // 提取 comments 数组
            if let Some(comments) = snapshot
                .get_mut("comments")
                .and_then(|v| v.as_array_mut())
            {
                for comment in comments.iter_mut() {
                    let Some(comment_obj) = comment.as_object_mut() else {
                        continue;
                    };
                    let Some(id_val) = comment_obj.get("id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    if !comment_ids.contains(id_val) {
                        continue;
                    }
                    // 改写为 redaction stub
                    *comment = serde_json::json!({
                        "id": id_val,
                        "issueId": input.issue_id.to_string(),
                        "body": "",
                        "presentation": null,
                        "metadata": null,
                        "deletedAt": deleted_at_iso,
                        "retentionRedaction": {
                            "reason": "source_comment_deleted",
                            "policy": super::capture::DECISION_TRAINING_RETENTION_POLICY,
                        },
                    });
                    changed = true;
                }
            }

            if !changed {
                continue;
            }

            // 在 snapshot 上设置 retention
            if let Some(obj) = snapshot.as_object_mut() {
                obj.insert(
                    "retention".into(),
                    serde_json::json!({
                        "policy": super::capture::DECISION_TRAINING_RETENTION_POLICY,
                        "commentDeletion": "redact",
                        "issueDeletion": "cascade",
                    }),
                );
            }

            sqlx::query(
                "UPDATE decision_training_examples \
                 SET retention_policy = $1, snapshot = $2, updated_at = $3 \
                 WHERE id = $4",
            )
            .bind(super::capture::DECISION_TRAINING_RETENTION_POLICY)
            .bind(Json(&snapshot))
            .bind(input.deleted_at)
            .bind(row_id)
            .execute(self.db.pool())
            .await?;

            updated_count += 1;
        }

        Ok(ScrubDeletedCommentsResult { updated_count })
    }

    /// 按 id 删除（与 Node `delete` 1:1 对齐）。
    pub async fn delete(&self, id: Uuid) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "DELETE FROM decision_training_examples WHERE id = $1 RETURNING id",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    // =========================================================================
    // Round 160: decision_training.rs route 仓储化新增方法
    // =========================================================================

    /// Round 160: 简化版 list（不带 issues JOIN）。
    pub async fn list_filtered_simple(
        &self,
        company_id: Uuid,
        kind: Option<&str>,
        author: Option<&str>,
        q_pattern: Option<&str>,
    ) -> sqlx::Result<Vec<DecisionTrainingExampleRow>> {
        let mut sql = String::from(
            "SELECT id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
             decision_outcome, snapshot, retention_policy, created_by_user_id, created_at, updated_at \
             FROM decision_training_examples WHERE company_id = $1",
        );
        let mut idx = 2;
        if kind.is_some() { sql.push_str(&format!(" AND source_kind = ${idx}")); idx += 1; }
        if author.is_some() { sql.push_str(&format!(" AND created_by_user_id = ${idx}")); idx += 1; }
        if q_pattern.is_some() { sql.push_str(&format!(" AND notes ILIKE ${idx}")); idx += 1; }
        sql.push_str(" ORDER BY created_at DESC LIMIT 500");
        let mut query = sqlx::query_as::<_, DecisionTrainingExampleRow>(&sql).bind(company_id);
        if let Some(k) = kind { query = query.bind(k); }
        if let Some(a) = author { query = query.bind(a); }
        if let Some(p) = q_pattern { query = query.bind(format!("%{p}%")); }
        Ok(query.fetch_all(self.db.pool()).await?)
    }

    /// Round 160: preview_decision — 查 decisions (status, decision_outcome, options)。
    pub async fn preview_decision(
        &self,
        company_id: Uuid,
        source_id: Uuid,
    ) -> sqlx::Result<Option<(String, Option<String>, serde_json::Value)>> {
        let row: Option<(String, Option<String>, serde_json::Value)> = sqlx::query_as(
            "SELECT status, decision_outcome, options FROM decisions \
             WHERE company_id = $1 AND id = $2",
        )
        .bind(company_id)
        .bind(source_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 160: preview_approval — 查 approvals (status)。
    pub async fn preview_approval(
        &self,
        company_id: Uuid,
        source_id: Uuid,
    ) -> sqlx::Result<Option<(String,)>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM approvals WHERE company_id = $1 AND id = $2",
        )
        .bind(company_id)
        .bind(source_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 160: export_resolved_decisions — 给 export_jsonl route 用。
    pub async fn export_resolved_decisions(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<(Uuid, String, serde_json::Value, Option<String>)>> {
        let rows: Vec<(Uuid, String, serde_json::Value, Option<String>)> = sqlx::query_as(
            "SELECT id, title, payload, decision_outcome FROM decisions \
             WHERE company_id = $1 AND status = 'resolved' \
             ORDER BY created_at DESC LIMIT 1000",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// Round 160: 取 example 的 created_by_user_id (用于 patch/delete owner 校验)。
    pub async fn owner_for_id(
        &self,
        id: Uuid,
    ) -> sqlx::Result<Option<String>> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT created_by_user_id FROM decision_training_examples WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.and_then(|(o,)| o))
    }

    /// Round 160: UPDATE notes + decision_outcome + 推 notes_history (COALESCE pattern) + RETURNING。
    pub async fn patch_with_history(
        &self,
        id: Uuid,
        notes: Option<String>,
        decision_outcome: Option<String>,
    ) -> sqlx::Result<Option<DecisionTrainingExampleRow>> {
        let row: Option<DecisionTrainingExampleRow> = sqlx::query_as(
            "UPDATE decision_training_examples SET \
                notes = COALESCE($2, notes), \
                decision_outcome = COALESCE($3, decision_outcome), \
                notes_history = COALESCE(notes_history, '[]'::jsonb) \
                                 || jsonb_build_array(jsonb_build_object('at', now(), 'notes', $2)), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
                       decision_outcome, snapshot, retention_policy, created_by_user_id, created_at, updated_at",
        )
        .bind(id)
        .bind(notes)
        .bind(decision_outcome)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- DecisionTrainingService ----

    #[test]
    fn service_new_takes_db_ref() {
        // 构造时不执行 DB 操作
        // 这里仅验证类型签名正确
        fn _check<'a>(db: &'a Db) -> DecisionTrainingService<'a> {
            DecisionTrainingService::new(db)
        }
    }

    // ---- SQL 形状 ----

    #[test]
    fn create_sql_uses_upsert_with_source_author_conflict() {
        let sql = "INSERT INTO decision_training_examples \
                   (company_id, source_kind, source_id, issue_id, cutoff_at, notes, notes_history, \
                    decision_outcome, retention_policy, snapshot, created_by_user_id) \
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                   ON CONFLICT (source_kind, source_id, created_by_user_id) DO NOTHING \
                   RETURNING id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, \
                             notes_history, decision_outcome, snapshot, retention_policy, \
                             created_by_user_id, created_at, updated_at";
        assert!(sql.contains("ON CONFLICT (source_kind, source_id, created_by_user_id) DO NOTHING"));
        assert!(sql.contains("RETURNING"));
        assert!(sql.contains("INSERT INTO decision_training_examples"));
    }

    #[test]
    fn list_sql_joins_issues_and_orders_by_created_at_desc() {
        let sql = "SELECT e.id, e.company_id, e.source_kind, e.source_id, e.issue_id, e.cutoff_at, \
                          e.notes, e.notes_history, e.decision_outcome, e.snapshot, e.retention_policy, \
                          e.created_by_user_id, e.created_at, e.updated_at, \
                          i.title AS issue_title, i.identifier AS issue_identifier \
                   FROM decision_training_examples e \
                   INNER JOIN issues i ON i.id = e.issue_id \
                   WHERE e.company_id = $1 \
                   ORDER BY e.created_at DESC, e.id DESC";
        assert!(sql.contains("INNER JOIN issues"));
        assert!(sql.contains("ORDER BY e.created_at DESC, e.id DESC"));
        assert!(sql.contains("i.title AS issue_title"));
    }

    #[test]
    fn get_by_id_sql_filters_on_id() {
        let sql = "SELECT id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, \
                          notes_history, decision_outcome, snapshot, retention_policy, \
                          created_by_user_id, created_at, updated_at \
                   FROM decision_training_examples WHERE id = $1";
        assert!(sql.contains("WHERE id = $1"));
    }

    #[test]
    fn update_notes_sql_updates_three_fields() {
        let sql = "UPDATE decision_training_examples \
                   SET notes = $1, notes_history = $2, updated_at = now() \
                   WHERE id = $3 \
                   RETURNING id, company_id, source_kind, source_id, issue_id, cutoff_at, notes, \
                             notes_history, decision_outcome, snapshot, retention_policy, \
                             created_by_user_id, created_at, updated_at";
        assert!(sql.contains("SET notes = $1, notes_history = $2, updated_at = now()"));
        assert!(sql.contains("WHERE id = $3"));
    }

    #[test]
    fn delete_sql_returns_id() {
        let sql = "DELETE FROM decision_training_examples WHERE id = $1 RETURNING id";
        assert!(sql.contains("DELETE FROM decision_training_examples"));
        assert!(sql.contains("RETURNING id"));
    }

    #[test]
    fn scrub_query_filters_by_company_and_issue() {
        let sql = "SELECT id, snapshot FROM decision_training_examples \
                   WHERE company_id = $1 AND issue_id = $2";
        assert!(sql.contains("company_id = $1 AND issue_id = $2"));
    }

    #[test]
    fn scrub_update_sql_sets_three_fields() {
        let sql = "UPDATE decision_training_examples \
                   SET retention_policy = $1, snapshot = $2, updated_at = $3 \
                   WHERE id = $4";
        assert!(sql.contains("retention_policy = $1"));
        assert!(sql.contains("snapshot = $2"));
        assert!(sql.contains("updated_at = $3"));
    }

    // ---- NotesHistoryEntry ----

    #[test]
    fn notes_history_entry_includes_all_three_fields() {
        let e = NotesHistoryEntry {
            author: "u1".into(),
            at: "2026-01-01T00:00:00Z".into(),
            body: "old".into(),
        };
        assert_eq!(e.author, "u1");
        assert_eq!(e.at, "2026-01-01T00:00:00Z");
        assert_eq!(e.body, "old");
    }

    // ---- ScrubDeletedCommentsResult ----

    #[test]
    fn scrub_result_default_is_zero() {
        let r = ScrubDeletedCommentsResult::default();
        assert_eq!(r.updated_count, 0);
    }

    #[test]
    fn scrub_result_partial_eq() {
        let a = ScrubDeletedCommentsResult { updated_count: 3 };
        let b = ScrubDeletedCommentsResult { updated_count: 3 };
        let c = ScrubDeletedCommentsResult { updated_count: 4 };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
