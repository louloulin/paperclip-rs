//! R608: `DocumentService` — high-level facade for document CRUD +
//! revisions + lock + annotations + issue-document links + lifecycle hooks.
//!
//! Aligned with `paperclip/server/src/services/documents.ts`. The Node service
//! is ~816 lines and includes legacy plan extraction, body reconciliation, and
//! import helpers. The Rust port focuses on the core CRUD + revision + lock +
//! annotation paths for the first cut; legacy plan extraction and import
//! helpers are deferred to R609+.

use std::sync::Arc;

use async_trait::async_trait;
use pc_errors::{forbidden, internal, unprocessable, validation, Error, Result};
use pc_repos::document::{
    AnnotationCommentRow, AnnotationThreadRow, DocumentRepo, DocumentRevisionRow, DocumentRow,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const ALLOWED_FORMATS: &[&str] = &["markdown", "plain", "html"];
const DEFAULT_FORMAT: &str = "markdown";

// =============================================================================
// R608: document lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DocumentHookEvent {
    Created {
        id: Uuid,
        company_id: Uuid,
        title: Option<String>,
        format: String,
    },
    Updated {
        id: Uuid,
        company_id: Uuid,
        title: Option<String>,
        latest_revision_number: i32,
    },
    Deleted {
        id: Uuid,
        company_id: Uuid,
    },
    Locked {
        id: Uuid,
        company_id: Uuid,
        locked_by_agent_id: Option<Uuid>,
        locked_by_user_id: Option<String>,
    },
    Unlocked {
        id: Uuid,
        company_id: Uuid,
    },
    RevisionRestored {
        document_id: Uuid,
        company_id: Uuid,
        restored_from_revision_number: i32,
        new_revision_id: Uuid,
    },
    AnnotationThreadCreated {
        thread_id: Uuid,
        document_id: Uuid,
        issue_id: Uuid,
        company_id: Uuid,
    },
    AnnotationThreadResolved {
        thread_id: Uuid,
        document_id: Uuid,
        company_id: Uuid,
        resolved_by_user_id: Option<String>,
    },
    AnnotationCommentCreated {
        comment_id: Uuid,
        thread_id: Uuid,
        document_id: Uuid,
        company_id: Uuid,
        author_type: String,
    },
}

// =============================================================================
// R608: DocumentHook trait
// =============================================================================

#[async_trait]
pub trait DocumentHook: Send + Sync {
    async fn on_document_event(&self, _event: DocumentHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopDocumentHook;
#[async_trait]
impl DocumentHook for NoopDocumentHook {}

#[derive(Default)]
pub struct RecordingDocumentHook {
    pub events: std::sync::Mutex<Vec<DocumentHookEvent>>,
}

#[async_trait]
impl DocumentHook for RecordingDocumentHook {
    async fn on_document_event(&self, event: DocumentHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingDocumentHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<DocumentHookEvent> {
        self.events.lock().expect("lock").clone()
    }

    pub fn clear(&self) {
        self.events.lock().expect("lock").clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.lock().expect("lock").is_empty()
    }
}

// =============================================================================
// R608: Public input / patch types
// =============================================================================

/// Input for `DocumentService::create`.
#[derive(Debug, Clone)]
pub struct CreateDocument {
    pub company_id: Uuid,
    pub title: Option<String>,
    pub format: Option<String>,
    pub body: String,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

impl CreateDocument {
    fn normalize(&self) -> Result<NormalizedCreate> {
        if self.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        if self.body.is_empty() {
            return Err(validation("document body must not be empty"));
        }
        let format = self
            .format
            .clone()
            .unwrap_or_else(|| DEFAULT_FORMAT.to_string());
        if !ALLOWED_FORMATS.contains(&format.as_str()) {
            return Err(validation(format!(
                "format must be one of markdown/plain/html, got {format}"
            )));
        }
        Ok(NormalizedCreate {
            title: self.title.clone(),
            format,
        })
    }
}

struct NormalizedCreate {
    title: Option<String>,
    format: String,
}

/// Partial update for a document. Empty `body` is treated as "no change" so
/// callers can update `title` without rewriting the body. To clear the
/// title, the caller must pass `Some(String::new())` explicitly.
#[derive(Debug, Clone, Default)]
pub struct DocumentPatch {
    pub title: Option<String>,
    pub format: Option<String>,
    pub body: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
}

impl DocumentPatch {
    fn validate(&self) -> Result<()> {
        if let Some(f) = &self.format {
            if !ALLOWED_FORMATS.contains(&f.as_str()) {
                return Err(validation(format!(
                    "format must be one of markdown/plain/html, got {f}"
                )));
            }
        }
        if let Some(b) = &self.body {
            if b.is_empty() {
                return Err(validation("document body must not be empty"));
            }
        }
        Ok(())
    }
}

/// Input for `DocumentService::create_annotation_thread`.
#[derive(Debug, Clone)]
pub struct CreateAnnotationThreadInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub document_id: Uuid,
    pub document_key: String,
    pub selected_text: String,
    pub prefix_text: String,
    pub suffix_text: String,
    pub normalized_start: i32,
    pub normalized_end: i32,
    pub markdown_start: i32,
    pub markdown_end: i32,
    pub anchor_confidence: Option<String>,
    pub anchor_selector: Option<Value>,
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
}

impl CreateAnnotationThreadInput {
    fn validate(&self) -> Result<()> {
        if self.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        if self.issue_id.is_nil() {
            return Err(validation("issueId is required"));
        }
        if self.document_id.is_nil() {
            return Err(validation("documentId is required"));
        }
        if self.document_key.trim().is_empty() {
            return Err(validation("documentKey must not be empty"));
        }
        if self.selected_text.is_empty() {
            return Err(validation("selectedText must not be empty"));
        }
        if self.normalized_end < self.normalized_start {
            return Err(unprocessable("normalizedEnd must be >= normalizedStart"));
        }
        if self.markdown_end < self.markdown_start {
            return Err(unprocessable("markdownEnd must be >= markdownStart"));
        }
        Ok(())
    }
}

/// Input for `DocumentService::create_annotation_comment`.
#[derive(Debug, Clone)]
pub struct CreateAnnotationComment {
    pub company_id: Uuid,
    pub thread_id: Uuid,
    pub issue_id: Uuid,
    pub document_id: Uuid,
    pub body: String,
    pub author_type: String,
    pub author_user_id: Option<String>,
    pub author_agent_id: Option<Uuid>,
}

impl CreateAnnotationComment {
    fn validate(&self) -> Result<()> {
        if self.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        if self.thread_id.is_nil() {
            return Err(validation("threadId is required"));
        }
        if self.issue_id.is_nil() {
            return Err(validation("issueId is required"));
        }
        if self.document_id.is_nil() {
            return Err(validation("documentId is required"));
        }
        if self.body.trim().is_empty() {
            return Err(validation("comment body must not be empty"));
        }
        match self.author_type.as_str() {
            "user" | "agent" | "system" => {}
            other => {
                return Err(validation(format!(
                    "authorType must be user/agent/system, got {other}"
                )))
            }
        }
        Ok(())
    }
}

/// Input for `DocumentService::upsert_issue_document`.
#[derive(Debug, Clone)]
pub struct UpsertIssueDocument {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub key: String,
    pub title: Option<String>,
    pub body: String,
    pub format: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

// =============================================================================
// R608: DocumentService
// =============================================================================

#[derive(Clone)]
pub struct DocumentService {
    db: pc_repos::Db,
    hooks: Vec<Arc<dyn DocumentHook>>,
}

impl DocumentService {
    #[must_use]
    pub fn new(db: pc_repos::Db) -> Self {
        Self {
            db,
            hooks: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_hooks(db: pc_repos::Db, hooks: Vec<Arc<dyn DocumentHook>>) -> Self {
        Self { db, hooks }
    }

    #[must_use]
    pub fn add_hook(mut self, hook: Arc<dyn DocumentHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    async fn dispatch(&self, event: DocumentHookEvent) -> Result<()> {
        for hook in &self.hooks {
            if let Err(e) = hook.on_document_event(event.clone()).await {
                tracing::warn!(?event, error = %e, "document hook failed");
            }
        }
        Ok(())
    }

    fn assert_unlocked(&self, row: &DocumentRow) -> Result<()> {
        if row.locked_at.is_some() {
            return Err(forbidden("document is locked; unlock before mutating"));
        }
        Ok(())
    }

    // ---- document CRUD ------------------------------------------------------

    pub async fn list_by_company(&self, company_id: Uuid) -> Result<Vec<DocumentRow>> {
        DocumentRepo::new(&self.db)
            .list_by_company(company_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn get(&self, document_id: Uuid) -> Result<Option<DocumentRow>> {
        DocumentRepo::new(&self.db)
            .get(document_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn get_in_company(
        &self,
        company_id: Uuid,
        document_id: Uuid,
    ) -> Result<Option<DocumentRow>> {
        DocumentRepo::new(&self.db)
            .get_in_company(company_id, document_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn create(&self, input: CreateDocument) -> Result<DocumentRow> {
        let normalized = input.normalize()?;
        let created = sqlx::query_as::<_, DocumentRow>(
            "INSERT INTO documents (company_id, title, format, latest_body, latest_revision_number,                 created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id)              VALUES ($1, $2, $3, $4, 1, $5, $6, $5, $6)              RETURNING id, company_id, title, format, latest_body, latest_revision_id,                 latest_revision_number, created_by_agent_id, created_by_user_id,                 updated_by_agent_id, updated_by_user_id, locked_at, locked_by_agent_id,                 locked_by_user_id, source_trust, created_at, updated_at",
        )
        .bind(input.company_id)
        .bind(normalized.title.as_deref())
        .bind(&normalized.format)
        .bind(&input.body)
        .bind(input.created_by_agent_id)
        .bind(input.created_by_user_id.as_deref())
        .fetch_one(self.db.pool())
        .await
        .map_err(|e| internal(format!("create document: {e}")))?;

        // Insert initial revision (revision_number=1).
        let _ = sqlx::query(
            "INSERT INTO document_revisions (company_id, document_id, revision_number, title,                 format, body, change_summary, created_by_agent_id, created_by_user_id)              VALUES ($1, $2, 1, $3, $4, $5, 'Created document', $6, $7)",
        )
        .bind(created.company_id)
        .bind(created.id)
        .bind(created.title.as_deref())
        .bind(&created.format)
        .bind(&created.latest_body)
        .bind(input.created_by_agent_id)
        .bind(input.created_by_user_id.as_deref())
        .execute(self.db.pool())
        .await
        .map_err(|e| internal(format!("create document revision: {e}")))?;

        self.dispatch(DocumentHookEvent::Created {
            id: created.id,
            company_id: created.company_id,
            title: created.title.clone(),
            format: created.format.clone(),
        })
        .await?;
        Ok(created)
    }

    pub async fn update(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        patch: DocumentPatch,
    ) -> Result<Option<DocumentRow>> {
        patch.validate()?;
        let repo = DocumentRepo::new(&self.db);
        let existing = repo
            .get_in_company(company_id, document_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| validation(format!("document {document_id} not found")))?;
        self.assert_unlocked(&existing)?;

        let new_body = patch
            .body
            .clone()
            .unwrap_or_else(|| existing.latest_body.clone());
        let new_format = patch
            .format
            .clone()
            .unwrap_or_else(|| existing.format.clone());
        let new_title = patch.title.clone().or_else(|| existing.title.clone());

        let updated = sqlx::query_as::<_, DocumentRow>(
            "UPDATE documents SET title = $2, format = $3, latest_body = $4,                 latest_revision_number = latest_revision_number + 1,                 updated_by_agent_id = $5, updated_by_user_id = $6, updated_at = now()              WHERE company_id = $1 AND id = $7              RETURNING id, company_id, title, format, latest_body, latest_revision_id,                 latest_revision_number, created_by_agent_id, created_by_user_id,                 updated_by_agent_id, updated_by_user_id, locked_at, locked_by_agent_id,                 locked_by_user_id, source_trust, created_at, updated_at",
        )
        .bind(company_id)
        .bind(new_title.as_deref())
        .bind(&new_format)
        .bind(&new_body)
        .bind(patch.updated_by_agent_id)
        .bind(patch.updated_by_user_id.as_deref())
        .bind(document_id)
        .fetch_one(self.db.pool())
        .await
        .map_err(|e| internal(format!("update document: {e}")))?;

        // Append a revision row so the body history is preserved.
        let _ = sqlx::query(
            "INSERT INTO document_revisions (company_id, document_id, revision_number, title,                 format, body, change_summary, created_by_agent_id, created_by_user_id)              VALUES ($1, $2, $3, $4, $5, $6, 'Updated document', $7, $8)",
        )
        .bind(company_id)
        .bind(document_id)
        .bind(updated.latest_revision_number)
        .bind(updated.title.as_deref())
        .bind(&updated.format)
        .bind(&updated.latest_body)
        .bind(patch.updated_by_agent_id)
        .bind(patch.updated_by_user_id.as_deref())
        .execute(self.db.pool())
        .await
        .map_err(|e| internal(format!("append document revision: {e}")))?;

        self.dispatch(DocumentHookEvent::Updated {
            id: updated.id,
            company_id: updated.company_id,
            title: updated.title.clone(),
            latest_revision_number: updated.latest_revision_number,
        })
        .await?;
        Ok(Some(updated))
    }

    pub async fn delete(&self, company_id: Uuid, document_id: Uuid) -> Result<bool> {
        let repo = DocumentRepo::new(&self.db);
        let existing = match repo
            .get_in_company(company_id, document_id)
            .await
            .map_err(map_sql_error)?
        {
            Some(row) => row,
            None => return Ok(false),
        };
        self.assert_unlocked(&existing)?;

        let removed = sqlx::query("DELETE FROM documents WHERE company_id = $1 AND id = $2")
            .bind(company_id)
            .bind(document_id)
            .execute(self.db.pool())
            .await
            .map_err(|e| internal(format!("delete document: {e}")))?
            .rows_affected()
            > 0;

        if removed {
            self.dispatch(DocumentHookEvent::Deleted {
                id: document_id,
                company_id,
            })
            .await?;
        }
        Ok(removed)
    }

    // ---- revisions ----------------------------------------------------------

    pub async fn list_revisions(&self, document_id: Uuid) -> Result<Vec<DocumentRevisionRow>> {
        DocumentRepo::new(&self.db)
            .list_revisions(document_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn restore_revision(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        revision_number: i32,
        actor_user_id: Option<&str>,
    ) -> Result<Option<DocumentRevisionRow>> {
        let repo = DocumentRepo::new(&self.db);
        let existing = repo
            .get_in_company(company_id, document_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| validation(format!("document {document_id} not found")))?;
        self.assert_unlocked(&existing)?;

        let new_rev = repo
            .restore_revision(document_id, revision_number, actor_user_id)
            .await
            .map_err(map_sql_error)?;

        if let Some(ref new_rev_row) = new_rev {
            self.dispatch(DocumentHookEvent::RevisionRestored {
                document_id,
                company_id,
                restored_from_revision_number: revision_number,
                new_revision_id: new_rev_row.id,
            })
            .await?;
        }
        Ok(new_rev)
    }

    // ---- lock / unlock ------------------------------------------------------

    pub async fn lock_document(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        actor_agent_id: Option<Uuid>,
        actor_user_id: Option<&str>,
    ) -> Result<Option<DocumentRow>> {
        let repo = DocumentRepo::new(&self.db);
        let existing = repo
            .get_in_company(company_id, document_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| validation(format!("document {document_id} not found")))?;
        if existing.locked_at.is_some() {
            return Err(conflict("document is already locked"));
        }
        let row = repo
            .lock_document(document_id, actor_agent_id, actor_user_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| conflict("document could not be locked (already locked?)"))?;
        self.dispatch(DocumentHookEvent::Locked {
            id: row.id,
            company_id: row.company_id,
            locked_by_agent_id: row.locked_by_agent_id,
            locked_by_user_id: row.locked_by_user_id.clone(),
        })
        .await?;
        Ok(Some(row))
    }

    pub async fn unlock_document(
        &self,
        company_id: Uuid,
        document_id: Uuid,
    ) -> Result<Option<DocumentRow>> {
        let repo = DocumentRepo::new(&self.db);
        let existing = repo
            .get_in_company(company_id, document_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| validation(format!("document {document_id} not found")))?;
        if existing.locked_at.is_none() {
            return Ok(Some(existing));
        }
        let row = repo
            .unlock_document(document_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| internal("unlock returned no row after confirmed lock"))?;
        self.dispatch(DocumentHookEvent::Unlocked {
            id: row.id,
            company_id: row.company_id,
        })
        .await?;
        Ok(Some(row))
    }

    // ---- annotations --------------------------------------------------------

    pub async fn list_annotation_threads(
        &self,
        document_id: Uuid,
        document_key: &str,
    ) -> Result<Vec<AnnotationThreadRow>> {
        DocumentRepo::new(&self.db)
            .list_annotation_threads(document_id, document_key)
            .await
            .map_err(map_sql_error)
    }

    pub async fn get_annotation_thread(
        &self,
        thread_id: Uuid,
    ) -> Result<Option<AnnotationThreadRow>> {
        DocumentRepo::new(&self.db)
            .get_annotation_thread(thread_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn create_annotation_thread(
        &self,
        input: CreateAnnotationThreadInput,
    ) -> Result<AnnotationThreadRow> {
        input.validate()?;
        let repo = DocumentRepo::new(&self.db);
        let doc = repo
            .get_in_company(input.company_id, input.document_id)
            .await
            .map_err(map_sql_error)?
            .ok_or_else(|| validation(format!("document {} not found", input.document_id)))?;
        let anchor_state = "anchored".to_string();
        let anchor_confidence = input
            .anchor_confidence
            .clone()
            .unwrap_or_else(|| "high".into());
        let anchor_selector = input.anchor_selector.clone().unwrap_or(Value::Null);
        let thread = sqlx::query_as::<_, AnnotationThreadRow>(
            "INSERT INTO document_annotation_threads                 (company_id, issue_id, document_id, document_key, status, anchor_state,                  original_revision_number, current_revision_number,                  selected_text, prefix_text, suffix_text,                  normalized_start, normalized_end, markdown_start, markdown_end,                  anchor_confidence, anchor_selector,                  created_by_agent_id, created_by_user_id)              VALUES ($1,$2,$3,$4,'open',$5, $6,$6, $7,$8,$9, $10,$11,$12,$13, $14,$15, $16,$17)              RETURNING id, company_id, issue_id, document_id, document_key, status, anchor_state,                 original_revision_id, original_revision_number, current_revision_id,                 current_revision_number, selected_text, prefix_text, suffix_text,                 normalized_start, normalized_end, markdown_start, markdown_end,                 anchor_confidence, anchor_selector, created_by_agent_id, created_by_user_id,                 resolved_by_agent_id, resolved_by_user_id, resolved_at,                 created_at, updated_at",
        )
        .bind(input.company_id)
        .bind(input.issue_id)
        .bind(input.document_id)
        .bind(&input.document_key)
        .bind(&anchor_state)
        .bind(doc.latest_revision_number)
        .bind(&input.selected_text)
        .bind(&input.prefix_text)
        .bind(&input.suffix_text)
        .bind(input.normalized_start)
        .bind(input.normalized_end)
        .bind(input.markdown_start)
        .bind(input.markdown_end)
        .bind(&anchor_confidence)
        .bind(&anchor_selector)
        .bind(input.created_by_agent_id)
        .bind(input.created_by_user_id.as_deref())
        .fetch_one(self.db.pool())
        .await
        .map_err(|e| internal(format!("create annotation thread: {e}")))?;

        self.dispatch(DocumentHookEvent::AnnotationThreadCreated {
            thread_id: thread.id,
            document_id: thread.document_id,
            issue_id: thread.issue_id,
            company_id: thread.company_id,
        })
        .await?;
        Ok(thread)
    }

    pub async fn resolve_annotation_thread(
        &self,
        thread_id: Uuid,
        resolved_by_user_id: Option<&str>,
    ) -> Result<Option<AnnotationThreadRow>> {
        let repo = DocumentRepo::new(&self.db);
        let thread = repo
            .resolve_annotation_thread(thread_id, resolved_by_user_id)
            .await
            .map_err(map_sql_error)?;
        if let Some(ref t) = thread {
            if t.status != "resolved" {
                return Err(internal(format!(
                    "resolve_annotation_thread returned status {} not resolved",
                    t.status
                )));
            }
            self.dispatch(DocumentHookEvent::AnnotationThreadResolved {
                thread_id: t.id,
                document_id: t.document_id,
                company_id: t.company_id,
                resolved_by_user_id: t.resolved_by_user_id.clone(),
            })
            .await?;
        }
        Ok(thread)
    }

    pub async fn list_annotation_comments(
        &self,
        thread_id: Uuid,
    ) -> Result<Vec<AnnotationCommentRow>> {
        DocumentRepo::new(&self.db)
            .list_annotation_comments(thread_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn create_annotation_comment(
        &self,
        input: CreateAnnotationComment,
    ) -> Result<AnnotationCommentRow> {
        input.validate()?;
        let comment = DocumentRepo::new(&self.db)
            .create_annotation_comment(
                input.company_id,
                input.thread_id,
                input.issue_id,
                input.document_id,
                &input.body,
                &input.author_type,
                input.author_user_id.as_deref(),
            )
            .await
            .map_err(map_sql_error)?;

        self.dispatch(DocumentHookEvent::AnnotationCommentCreated {
            comment_id: comment.id,
            thread_id: comment.thread_id,
            document_id: comment.document_id,
            company_id: comment.company_id,
            author_type: comment.author_type.clone(),
        })
        .await?;
        Ok(comment)
    }

    // ---- issue-document link ------------------------------------------------

    pub async fn list_issue_documents(&self, issue_id: Uuid) -> Result<Vec<DocumentRow>> {
        DocumentRepo::new(&self.db)
            .list_issue_documents(issue_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn get_issue_document_by_key(
        &self,
        issue_id: Uuid,
        key: &str,
    ) -> Result<Option<DocumentRow>> {
        DocumentRepo::new(&self.db)
            .get_issue_document_by_key(issue_id, key)
            .await
            .map_err(map_sql_error)
    }

    pub async fn upsert_issue_document(&self, input: UpsertIssueDocument) -> Result<DocumentRow> {
        if input.key.trim().is_empty() {
            return Err(validation("issue document key must not be empty"));
        }
        if input.body.is_empty() {
            return Err(validation("issue document body must not be empty"));
        }
        let format = input.format.unwrap_or_else(|| DEFAULT_FORMAT.to_string());
        let row = DocumentRepo::new(&self.db)
            .upsert_issue_document(
                input.company_id,
                input.issue_id,
                &input.key,
                input.title.as_deref(),
                &input.body,
                &format,
                input.created_by_user_id.as_deref(),
            )
            .await
            .map_err(map_sql_error)?;
        Ok(row)
    }

    pub async fn delete_issue_document(&self, issue_id: Uuid, key: &str) -> Result<bool> {
        DocumentRepo::new(&self.db)
            .delete_issue_document(issue_id, key)
            .await
            .map_err(map_sql_error)
    }
}

// =============================================================================
// helpers
// =============================================================================

fn map_sql_error(error: sqlx::Error) -> Error {
    internal(format!("document database operation failed: {error}"))
}

fn conflict(message: impl Into<String>) -> Error {
    pc_errors::conflict(message)
}
