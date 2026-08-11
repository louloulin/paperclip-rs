//! Service 实现 —— IssueContinuationSummaryService。
//!
//! 接收 `&Db` 引用，提供 build + get + refresh。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

use super::hook::{
    IssueContinuationSummaryHook, NoopIssueContinuationSummaryHook,
};
use super::markdown::{build_continuation_summary_markdown, extract_continuation_summary_next_action};
use super::types::{
    BuildContinuationSummaryInput, IssueContinuationSummaryDocument,
    IssueSummaryInput, RefreshContinuationSummaryInput, AgentSummaryInput,
    ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY, ISSUE_CONTINUATION_SUMMARY_TITLE,
};

/// 顶层公开函数：读取 issue 的 continuation summary document。
///
/// 与 Node `getIssueContinuationSummaryDocument` 1:1 对齐。
pub async fn get_continuation_summary(
    db: &Db,
    issue_id: Uuid,
) -> sqlx::Result<Option<IssueContinuationSummaryDocument>> {
    let row = sqlx::query(
        "SELECT d.title, d.latest_body, d.latest_revision_id, d.latest_revision_number, \
                d.source_trust, d.updated_at \
         FROM issue_documents idoc \
         INNER JOIN documents d ON idoc.document_id = d.id \
         WHERE idoc.issue_id = $1 AND idoc.key = $2",
    )
    .bind(issue_id)
    .bind(ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY)
    .fetch_optional(db.pool())
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(IssueContinuationSummaryDocument {
        key: ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY.to_string(),
        title: row.get("title"),
        body: row.get("latest_body"),
        latest_revision_id: row.get("latest_revision_id"),
        latest_revision_number: row.get("latest_revision_number"),
        source_trust: row.get("source_trust"),
        updated_at: row.get::<DateTime<Utc>, _>("updated_at"),
    }))
}

/// 顶层公开函数：刷新 issue 的 continuation summary（upsert）。
///
/// 与 Node `refreshIssueContinuationSummary` 1:1 对齐。
///
/// 行为：
/// 1. 读取 issue 当前信息
/// 2. 读取现有的 summary body
/// 3. 调用 markdown builder
/// 4. 通过 SQL upsert documents + issue_documents 关联
///
/// Returns `None` if issue doesn't exist.
pub async fn refresh_continuation_summary(
    db: &Db,
    input: RefreshContinuationSummaryInput,
) -> sqlx::Result<Option<IssueContinuationSummaryDocument>> {
    // 1. Fetch issue
    let issue_row = sqlx::query(
        "SELECT id, identifier, title, description, status, priority \
         FROM issues WHERE id = $1",
    )
    .bind(input.issue_id)
    .fetch_optional(db.pool())
    .await?;

    let Some(issue_row) = issue_row else {
        return Ok(None);
    };

    let issue = IssueSummaryInput {
        id: issue_row.get::<Uuid, _>("id").to_string(),
        identifier: issue_row
            .get::<Option<String>, _>("identifier"),
        title: issue_row.get("title"),
        description: issue_row.get("description"),
        status: issue_row.get("status"),
        priority: issue_row.get("priority"),
    };

    // 2. Fetch existing summary body
    let existing = get_continuation_summary(db, input.issue_id).await?;
    let previous_body = existing.as_ref().map(|d| d.body.clone());

    // 3. Build markdown
    let body = build_continuation_summary_markdown(&BuildContinuationSummaryInput {
        issue,
        run: input.run.clone(),
        agent: AgentSummaryInput {
            id: input.agent.id.clone(),
            name: input.agent.name.clone(),
            adapter_type: input.agent.adapter_type.clone(),
        },
        previous_summary_body: previous_body,
    });

    // 4. Upsert via SQL (mirror Node documentService.upsertIssueDocument).
    //    First, find or create document.
    let mut tx = db.pool().begin().await?;

    let existing_doc_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT document_id FROM issue_documents \
         WHERE issue_id = $1 AND key = $2",
    )
    .bind(input.issue_id)
    .bind(ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY)
    .fetch_optional(&mut *tx)
    .await?;

    let base_revision_id: Option<Uuid> = if let Some(doc_id) = existing_doc_id {
        sqlx::query_scalar("SELECT latest_revision_id FROM documents WHERE id = $1")
            .bind(doc_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten()
    } else {
        None
    };

    let document_id = if let Some(doc_id) = existing_doc_id {
        // Update existing document
        let next_rev: i32 = sqlx::query_scalar(
            "SELECT latest_revision_number FROM documents WHERE id = $1",
        )
        .bind(doc_id)
        .fetch_one(&mut *tx)
        .await?;

        // Insert revision
        let revision_id = Uuid::new_v4();
        let rev_num = next_rev + 1;
        sqlx::query(
            "INSERT INTO document_revisions \
             (id, company_id, document_id, revision_number, body, format, created_at) \
             VALUES ($1, $2, $3, $4, $5, 'markdown', now())",
        )
        .bind(revision_id)
        .bind(input.db_company_id)
        .bind(doc_id)
        .bind(rev_num)
        .bind(&body)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE documents SET latest_body = $1, latest_revision_id = $2, \
             latest_revision_number = $3, updated_at = now() WHERE id = $4",
        )
        .bind(&body)
        .bind(revision_id)
        .bind(rev_num)
        .bind(doc_id)
        .execute(&mut *tx)
        .await?;

        doc_id
    } else {
        // Create new document + issue_documents link
        let doc_id = Uuid::new_v4();
        let revision_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO documents \
             (id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, created_at, updated_at) \
             VALUES ($1, $2, $3, 'markdown', $4, $5, 1, now(), now())",
        )
        .bind(doc_id)
        .bind(input.db_company_id)
        .bind(ISSUE_CONTINUATION_SUMMARY_TITLE)
        .bind(&body)
        .bind(revision_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO document_revisions \
             (id, company_id, document_id, revision_number, body, format, created_at) \
             VALUES ($1, $2, $3, 1, $4, 'markdown', now())",
        )
        .bind(revision_id)
        .bind(input.db_company_id)
        .bind(doc_id)
        .bind(&body)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO issue_documents (id, company_id, issue_id, document_id, key, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now(), now())",
        )
        .bind(Uuid::new_v4())
        .bind(input.db_company_id)
        .bind(input.issue_id)
        .bind(doc_id)
        .bind(ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY)
        .execute(&mut *tx)
        .await?;

        doc_id
    };

    tx.commit().await?;

    // 5. Re-read and return
    get_continuation_summary(db, input.issue_id).await
}

/// Issue continuation summary service —— 封装 + Hook。
pub struct IssueContinuationSummaryService {
    hook: Arc<dyn IssueContinuationSummaryHook>,
}

impl std::fmt::Debug for IssueContinuationSummaryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueContinuationSummaryService").finish()
    }
}

impl Default for IssueContinuationSummaryService {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueContinuationSummaryService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopIssueContinuationSummaryHook),
        }
    }

    pub fn with_hook(hook: Arc<dyn IssueContinuationSummaryHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn IssueContinuationSummaryHook> {
        self.hook.clone()
    }

    /// Build markdown body（hook 集成）。
    pub fn build(&self, input: &BuildContinuationSummaryInput) -> String {
        self.hook.before_build(&input.issue.id, &input.run.id);
        let body = build_continuation_summary_markdown(input);
        self.hook.after_build(body.len());
        body
    }

    /// Extract next action from body。
    pub fn extract_next_action(&self, body: Option<&str>) -> Option<String> {
        extract_continuation_summary_next_action(body)
    }

    /// Refresh continuation summary（hook 集成）。
    pub async fn refresh(
        &self,
        db: &Db,
        input: RefreshContinuationSummaryInput,
    ) -> sqlx::Result<Option<IssueContinuationSummaryDocument>> {
        self.hook.before_refresh(&input);
        let result = refresh_continuation_summary(db, input).await?;
        if let Some(doc) = &result {
            self.hook.after_refresh(doc);
        }
        Ok(result)
    }

    /// Read continuation summary。
    pub async fn get(
        &self,
        db: &Db,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<IssueContinuationSummaryDocument>> {
        get_continuation_summary(db, issue_id).await
    }
}
