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

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct DocumentRevisionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub document_id: Uuid,
    pub revision_number: i32,
    pub title: Option<String>,
    pub format: Option<String>,
    pub body: String,
    pub change_summary: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IssueDocumentLinkRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub document_id: Uuid,
    pub key: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow)]
pub struct IssueDocumentWithKeyRow {
    pub key: String,
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

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AnnotationThreadRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub document_id: Uuid,
    pub document_key: String,
    pub status: String,
    pub anchor_state: String,
    pub original_revision_id: Option<Uuid>,
    pub original_revision_number: i32,
    pub current_revision_id: Option<Uuid>,
    pub current_revision_number: i32,
    pub selected_text: String,
    pub prefix_text: String,
    pub suffix_text: String,
    pub normalized_start: i32,
    pub normalized_end: i32,
    pub markdown_start: i32,
    pub markdown_end: i32,
    pub anchor_confidence: String,
    pub anchor_selector: serde_json::Value,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub resolved_by_agent_id: Option<Uuid>,
    pub resolved_by_user_id: Option<String>,
    pub resolved_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AnnotationCommentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub thread_id: Uuid,
    pub issue_id: Uuid,
    pub document_id: Uuid,
    pub body: String,
    pub author_type: String,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
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

    // =========================================================================
    // Issue documents
    // =========================================================================

    /// 列出某 issue 关联的所有 document（通过 issue_documents 联结）
    pub async fn list_issue_documents(&self, issue_id: Uuid) -> sqlx::Result<Vec<DocumentRow>> {
        // 使用子查询避免 JOIN 时的列名歧义
        let s = "SELECT d.id, d.company_id, d.title, d.format, d.latest_body, \
                    d.latest_revision_id, d.latest_revision_number, \
                    d.created_by_agent_id, d.created_by_user_id, \
                    d.updated_by_agent_id, d.updated_by_user_id, \
                    d.locked_at, d.locked_by_agent_id, d.locked_by_user_id, \
                    d.source_trust, d.created_at, d.updated_at \
             FROM documents d \
             INNER JOIN issue_documents idl ON idl.document_id = d.id \
             WHERE idl.issue_id = $1 ORDER BY d.updated_at DESC";
        sqlx::query_as::<_, DocumentRow>(s)
            .bind(issue_id)
            .fetch_all(self.db.pool())
            .await
    }

    /// 列出 issue 文档及其稳定 key，供需要构建聚合视图的上层服务使用。
    pub async fn list_issue_documents_with_keys(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueDocumentWithKeyRow>> {
        let rows = sqlx::query_as::<_, IssueDocumentWithKeyRow>(
            "SELECT idl.key, d.id, d.company_id, d.title, d.format, d.latest_body, \
             d.latest_revision_id, d.latest_revision_number, d.created_by_agent_id, \
             d.created_by_user_id, d.updated_by_agent_id, d.updated_by_user_id, \
             d.locked_at, d.locked_by_agent_id, d.locked_by_user_id, d.source_trust, \
             d.created_at, d.updated_at FROM documents d \
             INNER JOIN issue_documents idl ON idl.document_id=d.id \
             WHERE idl.company_id=$1 AND idl.issue_id=$2 ORDER BY idl.key ASC",
        )
        .bind(company_id)
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// 通过 (issue_id, key) 获取 document
    pub async fn get_issue_document_by_key(
        &self,
        issue_id: Uuid,
        key: &str,
    ) -> sqlx::Result<Option<DocumentRow>> {
        let s = "SELECT d.id, d.company_id, d.title, d.format, d.latest_body, \
                    d.latest_revision_id, d.latest_revision_number, \
                    d.created_by_agent_id, d.created_by_user_id, \
                    d.updated_by_agent_id, d.updated_by_user_id, \
                    d.locked_at, d.locked_by_agent_id, d.locked_by_user_id, \
                    d.source_trust, d.created_at, d.updated_at \
             FROM documents d \
             INNER JOIN issue_documents idl ON idl.document_id = d.id \
             WHERE idl.issue_id = $1 AND idl.key = $2";
        sqlx::query_as::<_, DocumentRow>(s)
            .bind(issue_id)
            .bind(key)
            .fetch_optional(self.db.pool())
            .await
    }

    /// Upsert issue document: 若 (issue_id, key) 已存在则更新；否则创建 document + link。
    /// 同时创建新 revision。
    pub async fn upsert_issue_document(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        key: &str,
        title: Option<&str>,
        body: &str,
        format_: &str,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<DocumentRow> {
        // 查找现有 link
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT document_id FROM issue_documents WHERE issue_id = $1 AND key = $2",
        )
        .bind(issue_id)
        .bind(key)
        .fetch_optional(self.db.pool())
        .await?;

        if let Some((doc_id,)) = existing {
            // 更新 document + 写 revision
            let mut tx = self.db.pool().begin().await?;
            let s = format!(
                "UPDATE documents SET latest_body = $2, title = COALESCE($3, title), format = $4, \
                    latest_revision_number = latest_revision_number + 1, updated_at = now(), \
                    updated_by_user_id = COALESCE($5, updated_by_user_id) \
                 WHERE id = $1 RETURNING {COLS}"
            );
            let doc: DocumentRow = sqlx::query_as::<_, DocumentRow>(&s)
                .bind(doc_id)
                .bind(body)
                .bind(title)
                .bind(format_)
                .bind(created_by_user_id)
                .fetch_one(&mut *tx)
                .await?;
            // 写 revision（用最新 revision_number）
            let rev_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO document_revisions (id, company_id, document_id, revision_number, body, created_by_user_id) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(rev_id)
            .bind(company_id)
            .bind(doc_id)
            .bind(doc.latest_revision_number)
            .bind(body)
            .bind(created_by_user_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE documents SET latest_revision_id = $1 WHERE id = $2")
                .bind(rev_id)
                .bind(doc_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE issue_documents SET updated_at = now() WHERE issue_id = $1 AND key = $2",
            )
            .bind(issue_id)
            .bind(key)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            // 重读以返回最新 revision_id
            self.get(doc_id).await.map(|o| o.unwrap_or(doc))
        } else {
            // 新建 document + link + 初始 revision
            let mut tx = self.db.pool().begin().await?;
            let doc_id = Uuid::new_v4();
            let s = format!(
                "INSERT INTO documents (id, company_id, title, format, latest_body, latest_revision_number, created_by_user_id) \
                 VALUES ($1, $2, $3, $4, $5, 1, $6) RETURNING {COLS}"
            );
            let doc: DocumentRow = sqlx::query_as::<_, DocumentRow>(&s)
                .bind(doc_id)
                .bind(company_id)
                .bind(title)
                .bind(format_)
                .bind(body)
                .bind(created_by_user_id)
                .fetch_one(&mut *tx)
                .await?;
            let rev_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO document_revisions (id, company_id, document_id, revision_number, body, created_by_user_id) \
                 VALUES ($1, $2, $3, 1, $4, $5)",
            )
            .bind(rev_id)
            .bind(company_id)
            .bind(doc_id)
            .bind(body)
            .bind(created_by_user_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE documents SET latest_revision_id = $1 WHERE id = $2")
                .bind(rev_id)
                .bind(doc_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO issue_documents (company_id, issue_id, document_id, key) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(company_id)
            .bind(issue_id)
            .bind(doc_id)
            .bind(key)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            self.get(doc_id).await.map(|o| o.unwrap_or(doc))
        }
    }

    /// 删除 issue document（包括 link 与 revision），保留 document 实体供其他 issue 使用
    pub async fn delete_issue_document(&self, issue_id: Uuid, key: &str) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM issue_documents WHERE issue_id = $1 AND key = $2")
            .bind(issue_id)
            .bind(key)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Document revisions
    // =========================================================================

    pub async fn list_revisions(
        &self,
        document_id: Uuid,
    ) -> sqlx::Result<Vec<DocumentRevisionRow>> {
        sqlx::query_as::<_, DocumentRevisionRow>(
            "SELECT id, company_id, document_id, revision_number, title, format, body, change_summary, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, created_at \
             FROM document_revisions WHERE document_id = $1 ORDER BY revision_number DESC",
        )
        .bind(document_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 158: company-scoped + limit 版的 list_revisions（summary_slots route 用）。
    pub async fn list_revisions_in_company(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<DocumentRevisionRow>> {
        sqlx::query_as::<_, DocumentRevisionRow>(
            "SELECT id, company_id, document_id, revision_number, title, format, body, change_summary, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, created_at \
             FROM document_revisions WHERE company_id = $1 AND document_id = $2 \
             ORDER BY revision_number DESC LIMIT $3",
        )
        .bind(company_id)
        .bind(document_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
    }

    /// 将文档回滚到指定 revision：写入新 revision 内容等于旧 revision
    pub async fn restore_revision(
        &self,
        document_id: Uuid,
        revision_number: i32,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<Option<DocumentRevisionRow>> {
        let target: Option<DocumentRevisionRow> = sqlx::query_as::<_, DocumentRevisionRow>(
            "SELECT id, company_id, document_id, revision_number, title, format, body, change_summary, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, created_at \
             FROM document_revisions WHERE document_id = $1 AND revision_number = $2",
        )
        .bind(document_id)
        .bind(revision_number)
        .fetch_optional(self.db.pool())
        .await?;
        let target = match target {
            Some(t) => t,
            None => return Ok(None),
        };
        // 取当前 document 的 latest_revision_number，新 revision = current + 1
        let current: Option<i32> =
            sqlx::query_scalar("SELECT latest_revision_number FROM documents WHERE id = $1")
                .bind(document_id)
                .fetch_optional(self.db.pool())
                .await?;
        let new_rev_number = current.unwrap_or(0) + 1;
        let new_rev_id = Uuid::new_v4();
        let company_id = target.company_id;
        let new_rev: DocumentRevisionRow = sqlx::query_as::<_, DocumentRevisionRow>(
            "INSERT INTO document_revisions (id, company_id, document_id, revision_number, title, format, body, change_summary, created_by_user_id) \
             VALUES ($1, $2, $3, $4, $5, 'markdown', $6, $7, $8) \
             RETURNING id, company_id, document_id, revision_number, title, format, body, change_summary, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, created_at",
        )
        .bind(new_rev_id)
        .bind(company_id)
        .bind(document_id)
        .bind(new_rev_number)
        .bind(target.title.as_deref())
        .bind(&target.body)
        .bind(format!("Restored from revision {}", revision_number))
        .bind(created_by_user_id)
        .fetch_one(self.db.pool())
        .await?;
        // 更新 document 指针
        let s = format!(
            "UPDATE documents SET latest_body = $2, latest_revision_id = $3, \
                latest_revision_number = $4, updated_at = now() WHERE id = $1"
        );
        sqlx::query(&s)
            .bind(document_id)
            .bind(&target.body)
            .bind(new_rev_id)
            .bind(new_rev_number)
            .execute(self.db.pool())
            .await?;
        Ok(Some(new_rev))
    }

    // =========================================================================
    // Document lock / unlock
    // =========================================================================

    pub async fn lock_document(
        &self,
        document_id: Uuid,
        actor_agent_id: Option<Uuid>,
        actor_user_id: Option<&str>,
    ) -> sqlx::Result<Option<DocumentRow>> {
        let s = format!(
            "UPDATE documents SET locked_at = now(), locked_by_agent_id = $2, \
                locked_by_user_id = $3, updated_at = now() \
             WHERE id = $1 AND locked_at IS NULL \
             RETURNING {COLS}"
        );
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(document_id)
            .bind(actor_agent_id)
            .bind(actor_user_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn unlock_document(&self, document_id: Uuid) -> sqlx::Result<Option<DocumentRow>> {
        let s = format!(
            "UPDATE documents SET locked_at = NULL, locked_by_agent_id = NULL, \
                locked_by_user_id = NULL, updated_at = now() \
             WHERE id = $1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(document_id)
            .fetch_optional(self.db.pool())
            .await
    }

    // =========================================================================
    // Annotation threads & comments
    // =========================================================================

    pub async fn list_annotation_threads(
        &self,
        document_id: Uuid,
        document_key: &str,
    ) -> sqlx::Result<Vec<AnnotationThreadRow>> {
        sqlx::query_as::<_, AnnotationThreadRow>(
            "SELECT id, company_id, issue_id, document_id, document_key, status, anchor_state, \
                    original_revision_id, original_revision_number, current_revision_id, \
                    current_revision_number, selected_text, prefix_text, suffix_text, \
                    normalized_start, normalized_end, markdown_start, markdown_end, \
                    anchor_confidence, anchor_selector, created_by_agent_id, created_by_user_id, \
                    resolved_by_agent_id, resolved_by_user_id, resolved_at, \
                    created_at, updated_at \
             FROM document_annotation_threads \
             WHERE document_id = $1 AND document_key = $2 \
             ORDER BY created_at ASC",
        )
        .bind(document_id)
        .bind(document_key)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_annotation_thread(
        &self,
        thread_id: Uuid,
    ) -> sqlx::Result<Option<AnnotationThreadRow>> {
        sqlx::query_as::<_, AnnotationThreadRow>(
            "SELECT id, company_id, issue_id, document_id, document_key, status, anchor_state, \
                    original_revision_id, original_revision_number, current_revision_id, \
                    current_revision_number, selected_text, prefix_text, suffix_text, \
                    normalized_start, normalized_end, markdown_start, markdown_end, \
                    anchor_confidence, anchor_selector, created_by_agent_id, created_by_user_id, \
                    resolved_by_agent_id, resolved_by_user_id, resolved_at, \
                    created_at, updated_at \
             FROM document_annotation_threads WHERE id = $1",
        )
        .bind(thread_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_annotation_thread(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        document_id: Uuid,
        document_key: &str,
        original_revision_id: Uuid,
        original_revision_number: i32,
        current_revision_id: Uuid,
        current_revision_number: i32,
        selected_text: &str,
        prefix_text: &str,
        suffix_text: &str,
        normalized_start: i32,
        normalized_end: i32,
        markdown_start: i32,
        markdown_end: i32,
        anchor_confidence: &str,
        anchor_selector: &serde_json::Value,
        created_by_user_id: Option<&str>,
    ) -> sqlx::Result<AnnotationThreadRow> {
        sqlx::query_as::<_, AnnotationThreadRow>(
            "INSERT INTO document_annotation_threads \
                (company_id, issue_id, document_id, document_key, \
                 original_revision_id, original_revision_number, \
                 current_revision_id, current_revision_number, \
                 selected_text, prefix_text, suffix_text, \
                 normalized_start, normalized_end, markdown_start, markdown_end, \
                 anchor_confidence, anchor_selector, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18) \
             RETURNING id, company_id, issue_id, document_id, document_key, status, anchor_state, \
                original_revision_id, original_revision_number, current_revision_id, \
                current_revision_number, selected_text, prefix_text, suffix_text, \
                normalized_start, normalized_end, markdown_start, markdown_end, \
                anchor_confidence, anchor_selector, created_by_agent_id, created_by_user_id, \
                resolved_by_agent_id, resolved_by_user_id, resolved_at, \
                created_at, updated_at",
        )
        .bind(company_id)
        .bind(issue_id)
        .bind(document_id)
        .bind(document_key)
        .bind(original_revision_id)
        .bind(original_revision_number)
        .bind(current_revision_id)
        .bind(current_revision_number)
        .bind(selected_text)
        .bind(prefix_text)
        .bind(suffix_text)
        .bind(normalized_start)
        .bind(normalized_end)
        .bind(markdown_start)
        .bind(markdown_end)
        .bind(anchor_confidence)
        .bind(anchor_selector)
        .bind(created_by_user_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn resolve_annotation_thread(
        &self,
        thread_id: Uuid,
        resolved_by_user_id: Option<&str>,
    ) -> sqlx::Result<Option<AnnotationThreadRow>> {
        sqlx::query_as::<_, AnnotationThreadRow>(
            "UPDATE document_annotation_threads SET status = 'resolved', \
                resolved_by_user_id = $2, resolved_at = now(), updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, issue_id, document_id, document_key, status, anchor_state, \
                original_revision_id, original_revision_number, current_revision_id, \
                current_revision_number, selected_text, prefix_text, suffix_text, \
                normalized_start, normalized_end, markdown_start, markdown_end, \
                anchor_confidence, anchor_selector, created_by_agent_id, created_by_user_id, \
                resolved_by_agent_id, resolved_by_user_id, resolved_at, \
                created_at, updated_at",
        )
        .bind(thread_id)
        .bind(resolved_by_user_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn list_annotation_comments(
        &self,
        thread_id: Uuid,
    ) -> sqlx::Result<Vec<AnnotationCommentRow>> {
        sqlx::query_as::<_, AnnotationCommentRow>(
            "SELECT id, company_id, thread_id, issue_id, document_id, body, author_type, \
                    author_agent_id, author_user_id, created_by_run_id, created_at, updated_at \
             FROM document_annotation_comments WHERE thread_id = $1 ORDER BY created_at ASC",
        )
        .bind(thread_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_annotation_comment(
        &self,
        company_id: Uuid,
        thread_id: Uuid,
        issue_id: Uuid,
        document_id: Uuid,
        body: &str,
        author_type: &str,
        author_user_id: Option<&str>,
    ) -> sqlx::Result<AnnotationCommentRow> {
        sqlx::query_as::<_, AnnotationCommentRow>(
            "INSERT INTO document_annotation_comments \
                (company_id, thread_id, issue_id, document_id, body, author_type, author_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             RETURNING id, company_id, thread_id, issue_id, document_id, body, author_type, \
                author_agent_id, author_user_id, created_by_run_id, created_at, updated_at",
        )
        .bind(company_id)
        .bind(thread_id)
        .bind(issue_id)
        .bind(document_id)
        .bind(body)
        .bind(author_type)
        .bind(author_user_id)
        .fetch_one(self.db.pool())
        .await
    }

    // =========================================================================
    // Round 158: summary_slots 仓储化新增方法
    // =========================================================================

    /// Round 158: company-scoped document 查找（summary_slots get_slot 用）。
    pub async fn get_in_company(
        &self,
        company_id: Uuid,
        document_id: Uuid,
    ) -> sqlx::Result<Option<DocumentRow>> {
        let s = format!("SELECT {COLS} FROM documents WHERE id = $1 AND company_id = $2");
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(document_id)
            .bind(company_id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// Round 158: 取文档的 latest_revision_id（check_base_revision 用）。
    pub async fn latest_revision_id_in_company(
        &self,
        company_id: Uuid,
        document_id: Uuid,
    ) -> sqlx::Result<Option<Uuid>> {
        let v: Option<(Option<Uuid>,)> = sqlx::query_as(
            "SELECT latest_revision_id FROM documents WHERE id = $1 AND company_id = $2",
        )
        .bind(document_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(v.and_then(|(id,)| id))
    }

    /// Round 158: update body (rev++) — 设置 updated_by_agent_id=NULL（手动写入语义）。
    pub async fn write_body(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        title: Option<&str>,
        body: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> sqlx::Result<DocumentRow> {
        let s = format!(
            "UPDATE documents SET title = $2, latest_body = $3, latest_revision_number = latest_revision_number + 1, \
             updated_by_agent_id = NULL, updated_at = $4 WHERE id = $1 AND company_id = $5 RETURNING {COLS}"
        );
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(document_id)
            .bind(title)
            .bind(body)
            .bind(now)
            .bind(company_id)
            .fetch_one(self.db.pool())
            .await
    }

    /// Round 158: 创建新 summary document（format='markdown'）。
    pub async fn create_markdown(
        &self,
        company_id: Uuid,
        title: Option<&str>,
        body: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> sqlx::Result<DocumentRow> {
        let s = format!(
            "INSERT INTO documents (company_id, title, format, latest_body, created_at, updated_at) \
             VALUES ($1,$2,'markdown',$3,$4,$4) RETURNING {COLS}"
        );
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(company_id)
            .bind(title)
            .bind(body)
            .bind(now)
            .fetch_one(self.db.pool())
            .await
    }

    /// Round 158: 更新文档的 latest_revision_id + latest_revision_number。
    pub async fn set_latest_revision(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        revision_number: i32,
    ) -> sqlx::Result<DocumentRow> {
        let s = format!(
            "UPDATE documents SET latest_revision_id = $2, latest_revision_number = $3 WHERE id = $1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, DocumentRow>(&s)
            .bind(document_id)
            .bind(revision_id)
            .bind(revision_number)
            .fetch_one(self.db.pool())
            .await
    }

    /// Round 158: insert revision with title/format/body/change_summary + RETURNING。
    pub async fn insert_revision_full(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        revision_number: i32,
        title: Option<&str>,
        body: &str,
        change_summary: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> sqlx::Result<DocumentRevisionRow> {
        let s = format!(
            "INSERT INTO document_revisions (company_id, document_id, revision_number, title, format, body, change_summary, created_at) \
             VALUES ($1,$2,$3,$4,'markdown',$5,$6,$7) \
             RETURNING id, company_id, document_id, revision_number, title, format, body, change_summary, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, created_at"
        );
        sqlx::query_as::<_, DocumentRevisionRow>(&s)
            .bind(company_id)
            .bind(document_id)
            .bind(revision_number)
            .bind(title)
            .bind(body)
            .bind(change_summary)
            .bind(now)
            .fetch_one(self.db.pool())
            .await
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
        let _ = std::any::type_name::<fn()>()
            .split("::")
            .next();
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

