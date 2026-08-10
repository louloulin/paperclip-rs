//! Plan review context.
//!
//! 1:1 port of Node `paperclip/server/src/services/plan-review-context.ts`.
//!
//! Builds a snapshot of "what's currently under review on the plan
//! document for an issue" so the model can see open annotation threads
//! + comments + the most recent plan-review interaction when issuing
//! or planning work.
//!
//! Pure logic (truncation, JSON target/result parsing, author
//! extraction) is fully unit-tested; the SQL layer is exercised by
//! e2e tests against the real Postgres test instance.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pc_repos::Db;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

// ---------------------------------------------------------------------
// Public constants (mirror Node PLAN_REVIEW_CONTEXT_LIMITS)
// ---------------------------------------------------------------------

pub const PLAN_REVIEW_CONTEXT_LIMITS: PlanReviewContextLimits = PlanReviewContextLimits {
    max_threads: 20,
    max_comments: 80,
    max_body_chars: 1_200,
    max_total_body_chars: 12_000,
    max_anchor_text_chars: 500,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewContextLimits {
    pub max_threads: usize,
    pub max_comments: usize,
    pub max_body_chars: usize,
    pub max_total_body_chars: usize,
    pub max_anchor_text_chars: usize,
}

// ---------------------------------------------------------------------
// Public DTOs
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewContextAuthor {
    #[serde(rename = "type")]
    pub author_type: String,
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewInteractionTargetContext {
    pub issue_id: String,
    pub document_id: Option<String>,
    pub key: String,
    pub revision_id: Option<String>,
    pub revision_number: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewInteractionResultContext {
    pub outcome: Option<String>,
    pub reason: Option<String>,
    pub comment_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewInteractionContext {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub continuation_policy: Option<String>,
    pub source_comment_id: Option<String>,
    pub source_run_id: Option<String>,
    pub target: PlanReviewInteractionTargetContext,
    /// Set to the target when the interaction was accepted.
    pub accepted_target_revision: Option<PlanReviewInteractionTargetContext>,
    pub result: Option<PlanReviewInteractionResultContext>,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewContextComment {
    pub id: String,
    pub thread_id: String,
    pub body: String,
    pub body_truncated: bool,
    pub author: PlanReviewContextAuthor,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewContextThread {
    pub id: String,
    pub document_key: String,
    pub document_id: String,
    pub status: String,
    pub revision_id: Option<String>,
    pub revision_number: Option<i32>,
    pub anchor_state: Option<String>,
    pub anchor_confidence: Option<String>,
    pub selected_text: String,
    pub selected_text_truncated: bool,
    pub prefix_text: String,
    pub prefix_text_truncated: bool,
    pub suffix_text: String,
    pub suffix_text_truncated: bool,
    pub author: PlanReviewContextAuthor,
    pub comment_count: i64,
    pub comments: Vec<PlanReviewContextComment>,
    pub comments_truncated: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewContextTotals {
    pub open_thread_count: i64,
    pub included_thread_count: i64,
    pub omitted_thread_count: i64,
    pub comment_count: i64,
    pub included_comment_count: i64,
    pub omitted_comment_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReviewContext {
    pub document_key: String,
    pub issue_id: String,
    pub latest_revision_id: Option<String>,
    pub latest_revision_number: Option<i32>,
    pub threads: Vec<PlanReviewContextThread>,
    pub interaction: Option<PlanReviewInteractionContext>,
    pub totals: PlanReviewContextTotals,
    pub limits: PlanReviewContextLimits,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BuildPlanReviewContextInput {
    pub company_id: String,
    pub issue_id: String,
    pub issue_work_mode: Option<String>,
    pub include_for_issue_comment: bool,
    pub include_for_annotation_delta: bool,
    pub interaction_id: Option<String>,
}

// ---------------------------------------------------------------------
// Pure helpers (private but unit-tested)
// ---------------------------------------------------------------------

pub(crate) fn non_empty_string(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

pub(crate) fn truncate_text(value: &str, max_chars: usize) -> TruncateResult {
    if value.chars().count() <= max_chars {
        TruncateResult {
            text: value.to_string(),
            truncated: false,
        }
    } else {
        TruncateResult {
            text: value.chars().take(max_chars).collect(),
            truncated: true,
        }
    }
}

pub(crate) struct TruncateResult {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AuthorFromRow {
    pub author_type: Option<String>,
    pub author_agent_id: Option<String>,
    pub author_user_id: Option<String>,
}

pub(crate) fn author_from(row: &AuthorFromRow) -> PlanReviewContextAuthor {
    if let Some(agent_id) = &row.author_agent_id {
        return PlanReviewContextAuthor {
            author_type: "agent".to_string(),
            id: Some(agent_id.clone()),
        };
    }
    if let Some(user_id) = &row.author_user_id {
        return PlanReviewContextAuthor {
            author_type: "user".to_string(),
            id: Some(user_id.clone()),
        };
    }
    let t = row
        .author_type
        .as_deref()
        .unwrap_or("system");
    let valid = matches!(t, "agent" | "user" | "system");
    PlanReviewContextAuthor {
        author_type: if valid { t.to_string() } else { "system".to_string() },
        id: None,
    }
}

pub(crate) fn read_plan_target(
    value: &Value,
    issue_id: &str,
) -> Option<PlanReviewInteractionTargetContext> {
    let target = value.as_object()?;
    if target.get("type").and_then(|v| v.as_str()) != Some("issue_document") {
        return None;
    }
    if target.get("key").and_then(|v| v.as_str()) != Some("plan") {
        return None;
    }
    if non_empty_string(target.get("issueId")?).as_deref() != Some(issue_id) {
        return None;
    }
    let revision_number = match target.get("revisionNumber") {
        Some(Value::Number(n)) => n.as_i64().map(|x| x as i32),
        _ => None,
    };
    Some(PlanReviewInteractionTargetContext {
        issue_id: issue_id.to_string(),
        document_id: non_empty_string(target.get("documentId").unwrap_or(&Value::Null)),
        key: "plan".to_string(),
        revision_id: non_empty_string(target.get("revisionId").unwrap_or(&Value::Null)),
        revision_number,
    })
}

pub(crate) fn read_result(value: &Value) -> Option<PlanReviewInteractionResultContext> {
    let obj = value.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let outcome = non_empty_string(obj.get("outcome").unwrap_or(&Value::Null));
    let reason = non_empty_string(obj.get("reason").unwrap_or(&Value::Null))
        .or_else(|| non_empty_string(obj.get("rejectionReason").unwrap_or(&Value::Null)));
    let comment_id = non_empty_string(obj.get("commentId").unwrap_or(&Value::Null));
    Some(PlanReviewInteractionResultContext {
        outcome,
        reason,
        comment_id,
    })
}

// ---------------------------------------------------------------------
// DB trait (so the e2e can run against real PG)
// ---------------------------------------------------------------------

#[async_trait]
pub trait PlanReviewDb: Send + Sync {
    async fn fetch_interaction(
        &self,
        company_id: &str,
        issue_id: &str,
        interaction_id: &str,
    ) -> sqlx::Result<Option<InteractionRow>>;
    async fn fetch_plan_document(
        &self,
        company_id: &str,
        issue_id: &str,
    ) -> sqlx::Result<Option<PlanDocumentRow>>;
    async fn count_open_threads(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
    ) -> sqlx::Result<i64>;
    async fn fetch_threads(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
        limit: i64,
    ) -> sqlx::Result<Vec<ThreadRow>>;
    async fn fetch_comments(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
        thread_ids: &[Uuid],
        limit: i64,
    ) -> sqlx::Result<Vec<CommentRow>>;
    async fn count_comments(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
    ) -> sqlx::Result<i64>;
}

#[derive(Debug, Clone)]
pub struct InteractionRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub continuation_policy: Option<String>,
    pub source_comment_id: Option<String>,
    pub source_run_id: Option<String>,
    pub payload: Value,
    pub result: Value,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct PlanDocumentRow {
    pub document_id: Uuid,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub document_key: String,
    pub status: String,
    pub revision_id: Option<Uuid>,
    pub revision_number: Option<i32>,
    pub anchor_state: Option<String>,
    pub anchor_confidence: Option<String>,
    pub selected_text: Option<String>,
    pub prefix_text: Option<String>,
    pub suffix_text: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CommentRow {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub body: String,
    pub author_type: Option<String>,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
impl PlanReviewDb for Db {
    async fn fetch_interaction(
        &self,
        company_id: &str,
        issue_id: &str,
        interaction_id: &str,
    ) -> sqlx::Result<Option<InteractionRow>> {
        let interaction_uuid = Uuid::parse_str(&interaction_id).unwrap_or(Uuid::nil());
        let company_uuid = Uuid::parse_str(company_id).unwrap_or(Uuid::nil());
        let issue_uuid = Uuid::parse_str(issue_id).unwrap_or(Uuid::nil());
        let row = sqlx::query(
            "SELECT id::text AS id, kind, status, continuation_policy, \
                    source_comment_id::text AS source_comment_id, \
                    source_run_id::text AS source_run_id, \
                    payload, result, resolved_at \
             FROM issue_thread_interactions \
             WHERE id = $1 AND company_id = $2 AND issue_id = $3",
        )
        .bind(interaction_uuid)
        .bind(company_uuid)
        .bind(issue_uuid)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| InteractionRow {
            id: r.get::<String, _>("id"),
            kind: r.get::<String, _>("kind"),
            status: r.get::<String, _>("status"),
            continuation_policy: r.try_get("continuation_policy").ok().flatten(),
            source_comment_id: r.try_get("source_comment_id").ok().flatten(),
            source_run_id: r.try_get("source_run_id").ok().flatten(),
            payload: r.try_get("payload").unwrap_or(Value::Null),
            result: r.try_get("result").unwrap_or(Value::Null),
            resolved_at: r.try_get("resolved_at").ok().flatten(),
        }))
    }

    async fn fetch_plan_document(
        &self,
        company_id: &str,
        issue_id: &str,
    ) -> sqlx::Result<Option<PlanDocumentRow>> {
        let row = sqlx::query(
            "SELECT d.id AS document_id, d.latest_revision_id, d.latest_revision_number \
             FROM issue_documents idoc \
             INNER JOIN documents d ON d.id = idoc.document_id \
             WHERE idoc.company_id = $1 AND idoc.issue_id = $2 AND idoc.key = 'plan' \
               AND d.company_id = $1",
        )
        .bind(Uuid::parse_str(company_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(issue_id).unwrap_or(Uuid::nil()))
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| PlanDocumentRow {
            document_id: r.get("document_id"),
            latest_revision_id: r.try_get("latest_revision_id").ok().flatten(),
            latest_revision_number: r.try_get("latest_revision_number").ok().flatten(),
        }))
    }

    async fn count_open_threads(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
    ) -> sqlx::Result<i64> {
        let row = sqlx::query(
            "SELECT count(*)::bigint AS n FROM document_annotation_threads \
             WHERE company_id = $1 AND issue_id = $2 AND document_id = $3 \
               AND document_key = 'plan' AND status = 'open'",
        )
        .bind(Uuid::parse_str(company_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(issue_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(document_id).unwrap_or(Uuid::nil()))
        .fetch_one(self.pool())
        .await?;
        Ok(row.get::<i64, _>("n"))
    }

    async fn fetch_threads(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
        limit: i64,
    ) -> sqlx::Result<Vec<ThreadRow>> {
        let rows = sqlx::query(
            "SELECT id, document_id, document_key, status, current_revision_id, \
                    current_revision_number, anchor_state, anchor_confidence, \
                    selected_text, prefix_text, suffix_text, \
                    created_by_agent_id, created_by_user_id, created_at, updated_at \
             FROM document_annotation_threads \
             WHERE company_id = $1 AND issue_id = $2 AND document_id = $3 \
               AND document_key = 'plan' AND status = 'open' \
             ORDER BY updated_at DESC, id DESC \
             LIMIT $4",
        )
        .bind(Uuid::parse_str(company_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(issue_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(document_id).unwrap_or(Uuid::nil()))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ThreadRow {
                id: r.get("id"),
                document_id: r.get("document_id"),
                document_key: r.get("document_key"),
                status: r.get("status"),
                revision_id: r.try_get("current_revision_id").ok().flatten(),
                revision_number: r.try_get("current_revision_number").ok().flatten(),
                anchor_state: r.try_get("anchor_state").ok().flatten(),
                anchor_confidence: r.try_get("anchor_confidence").ok().flatten(),
                selected_text: r.try_get("selected_text").ok().flatten(),
                prefix_text: r.try_get("prefix_text").ok().flatten(),
                suffix_text: r.try_get("suffix_text").ok().flatten(),
                created_by_agent_id: r.try_get("created_by_agent_id").ok().flatten(),
                created_by_user_id: r.try_get("created_by_user_id").ok().flatten(),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            });
        }
        Ok(out)
    }

    async fn fetch_comments(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
        thread_ids: &[Uuid],
        limit: i64,
    ) -> sqlx::Result<Vec<CommentRow>> {
        if thread_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT id, thread_id, body, author_type, author_agent_id, author_user_id, \
                    created_at, updated_at \
             FROM document_annotation_comments \
             WHERE company_id = $1 AND issue_id = $2 AND document_id = $3 \
               AND thread_id = ANY($4::uuid[]) \
             ORDER BY created_at ASC, id ASC \
             LIMIT $5",
        )
        .bind(Uuid::parse_str(company_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(issue_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(document_id).unwrap_or(Uuid::nil()))
        .bind(thread_ids)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(CommentRow {
                id: r.get("id"),
                thread_id: r.get("thread_id"),
                body: r.get("body"),
                author_type: r.try_get("author_type").ok().flatten(),
                author_agent_id: r.try_get("author_agent_id").ok().flatten(),
                author_user_id: r.try_get("author_user_id").ok().flatten(),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            });
        }
        Ok(out)
    }

    async fn count_comments(
        &self,
        company_id: &str,
        issue_id: &str,
        document_id: &str,
    ) -> sqlx::Result<i64> {
        let row = sqlx::query(
            "SELECT count(*)::bigint AS n FROM document_annotation_comments c \
             INNER JOIN document_annotation_threads t ON c.thread_id = t.id \
             WHERE c.company_id = $1 AND c.issue_id = $2 AND c.document_id = $3 \
               AND t.company_id = $1 AND t.issue_id = $2 AND t.document_id = $3 \
               AND t.document_key = 'plan' AND t.status = 'open'",
        )
        .bind(Uuid::parse_str(company_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(issue_id).unwrap_or(Uuid::nil()))
        .bind(Uuid::parse_str(document_id).unwrap_or(Uuid::nil()))
        .fetch_one(self.pool())
        .await?;
        Ok(row.get::<i64, _>("n"))
    }
}

// ---------------------------------------------------------------------
// Public async entry
// ---------------------------------------------------------------------

pub async fn get_plan_interaction_context(
    db: &Db,
    input: GetPlanInteractionInput<'_>,
) -> sqlx::Result<Option<PlanReviewInteractionContext>> {
    let Some(interaction_id) = non_empty_string(&Value::String(input.interaction_id.to_string())) else {
        return Ok(None);
    };
    let Some(row) = PlanReviewDb::fetch_interaction(db, input.company_id, input.issue_id, &interaction_id).await? else {
        return Ok(None);
    };
    let target_value = row.payload.get("target").unwrap_or(&Value::Null);
    let Some(target) = read_plan_target(target_value, input.issue_id) else {
        return Ok(None);
    };
    let result = read_result(&row.result);
    let accepted_target_revision = if row.status == "accepted" {
        Some(target.clone())
    } else {
        None
    };
    Ok(Some(PlanReviewInteractionContext {
        id: row.id,
        kind: row.kind,
        status: row.status,
        continuation_policy: row.continuation_policy,
        source_comment_id: row.source_comment_id,
        source_run_id: row.source_run_id,
        target,
        accepted_target_revision,
        result,
        resolved_at: row.resolved_at.map(|d| d.to_rfc3339()),
    }))
}


#[derive(Debug, Clone, Copy)]
pub struct GetPlanInteractionInput<'a> {
    pub company_id: &'a str,
    pub issue_id: &'a str,
    pub interaction_id: &'a str,
}

/// Build the plan review context for an issue. Returns `None` if
/// the issue is not in planning mode and no interaction / annotation
/// hook is provided.
pub async fn build_plan_review_context(
    db: &Db,
    input: BuildPlanReviewContextInput,
) -> sqlx::Result<Option<PlanReviewContext>> {
    let interaction_id_str = input.interaction_id.clone().unwrap_or_default();
    let interaction = get_plan_interaction_context(
        db,
        GetPlanInteractionInput {
            company_id: &input.company_id,
            issue_id: &input.issue_id,
            interaction_id: &interaction_id_str,
        },
    )
    .await?;

    let should_include = input.issue_work_mode.as_deref() == Some("planning")
        || input.include_for_issue_comment
        || input.include_for_annotation_delta
        || interaction.is_some();
    if !should_include {
        return Ok(None);
    }

    let plan_document = match PlanReviewDb::fetch_plan_document(db, &input.company_id, &input.issue_id).await? {
        Some(d) => d,
        None => return Ok(None),
    };

    let document_id_str = plan_document.document_id.to_string();
    let open_thread_count =
        PlanReviewDb::count_open_threads(db, &input.company_id, &input.issue_id, &document_id_str).await?;
    let thread_rows = PlanReviewDb::fetch_threads(
        db,
        &input.company_id,
        &input.issue_id,
        &document_id_str,
        PLAN_REVIEW_CONTEXT_LIMITS.max_threads as i64,
    )
    .await?;
    let thread_uuids: Vec<Uuid> = thread_rows.iter().map(|t| t.id).collect();
    let comment_rows = PlanReviewDb::fetch_comments(
        db,
        &input.company_id,
        &input.issue_id,
        &document_id_str,
        &thread_uuids,
        PLAN_REVIEW_CONTEXT_LIMITS.max_comments as i64,
    )
    .await?;
    let comment_count =
        PlanReviewDb::count_comments(db, &input.company_id, &input.issue_id, &document_id_str).await?;

    // Group comments by thread_id.
    let mut comments_by_thread: std::collections::HashMap<Uuid, Vec<CommentRow>> =
        std::collections::HashMap::new();
    for c in comment_rows {
        comments_by_thread.entry(c.thread_id).or_default().push(c);
    }

    let mut remaining_body_chars = PLAN_REVIEW_CONTEXT_LIMITS.max_total_body_chars;
    let mut included_comment_count: i64 = 0;
    let mut truncated = open_thread_count > thread_rows.len() as i64;
    let max_body_chars = PLAN_REVIEW_CONTEXT_LIMITS.max_body_chars;
    let max_anchor_text_chars = PLAN_REVIEW_CONTEXT_LIMITS.max_anchor_text_chars;
    let max_comments = PLAN_REVIEW_CONTEXT_LIMITS.max_comments;

    let mut threads: Vec<PlanReviewContextThread> = Vec::with_capacity(thread_rows.len());
    for thread in thread_rows {
        let selected = thread
            .selected_text
            .as_deref()
            .map(|s| truncate_text(s, max_anchor_text_chars))
            .unwrap_or(TruncateResult { text: String::new(), truncated: false });
        let prefix = thread
            .prefix_text
            .as_deref()
            .map(|s| truncate_text(s, max_anchor_text_chars))
            .unwrap_or(TruncateResult { text: String::new(), truncated: false });
        let suffix = thread
            .suffix_text
            .as_deref()
            .map(|s| truncate_text(s, max_anchor_text_chars))
            .unwrap_or(TruncateResult { text: String::new(), truncated: false });
        if selected.truncated || prefix.truncated || suffix.truncated {
            truncated = true;
        }

        let thread_comments = comments_by_thread.remove(&thread.id).unwrap_or_default();
        let mut comments: Vec<PlanReviewContextComment> = Vec::new();
        for c in thread_comments.iter() {
            if included_comment_count as usize >= max_comments || remaining_body_chars <= 0 {
                truncated = true;
                break;
            }
            let allowed = std::cmp::min(max_body_chars, remaining_body_chars as usize);
            let body = truncate_text(&c.body, allowed);
            if body.truncated {
                truncated = true;
            }
            remaining_body_chars = remaining_body_chars.saturating_sub(body.text.chars().count());
            included_comment_count += 1;
            comments.push(PlanReviewContextComment {
                id: c.id.to_string(),
                thread_id: c.thread_id.to_string(),
                body: body.text,
                body_truncated: body.truncated,
                author: author_from(&AuthorFromRow {
                    author_type: c.author_type.clone(),
                    author_agent_id: c.author_agent_id.map(|u| u.to_string()),
                    author_user_id: c.author_user_id.clone(),
                }),
                created_at: c.created_at.to_rfc3339(),
                updated_at: c.updated_at.to_rfc3339(),
            });
        }

        let comments_truncated = (comments.len() as i64) < thread_comments.len() as i64;
        if comments_truncated {
            truncated = true;
        }

        threads.push(PlanReviewContextThread {
            id: thread.id.to_string(),
            document_key: thread.document_key,
            document_id: thread.document_id.to_string(),
            status: thread.status,
            revision_id: thread.revision_id.map(|u| u.to_string()),
            revision_number: thread.revision_number,
            anchor_state: thread.anchor_state,
            anchor_confidence: thread.anchor_confidence,
            selected_text: selected.text,
            selected_text_truncated: selected.truncated,
            prefix_text: prefix.text,
            prefix_text_truncated: prefix.truncated,
            suffix_text: suffix.text,
            suffix_text_truncated: suffix.truncated,
            author: author_from(&AuthorFromRow {
                author_type: None,
                author_agent_id: thread.created_by_agent_id.map(|u| u.to_string()),
                author_user_id: thread.created_by_user_id.clone(),
            }),
            comment_count: thread_comments.len() as i64,
            comments,
            comments_truncated,
            created_at: thread.created_at.to_rfc3339(),
            updated_at: thread.updated_at.to_rfc3339(),
        });
    }

    let omitted_comment_count = std::cmp::max(0, comment_count - included_comment_count);
    if omitted_comment_count > 0 {
        truncated = true;
    }

    let included_thread_count = threads.len() as i64;
    let omitted_thread_count = std::cmp::max(0, open_thread_count - included_thread_count);

    Ok(Some(PlanReviewContext {
        document_key: "plan".to_string(),
        issue_id: input.issue_id,
        latest_revision_id: plan_document.latest_revision_id.map(|u| u.to_string()),
        latest_revision_number: plan_document.latest_revision_number,
        threads,
        interaction,
        totals: PlanReviewContextTotals {
            open_thread_count,
            included_thread_count,
            omitted_thread_count,
            comment_count,
            included_comment_count,
            omitted_comment_count,
        },
        limits: PLAN_REVIEW_CONTEXT_LIMITS,
        truncated,
    }))
}

// (finalize logic was inlined into build_plan_review_context; this function kept for API stability but is a no-op)
pub fn build_plan_review_context_finalize(ctx: PlanReviewContext) -> PlanReviewContext {
    ctx
}

// ---------------------------------------------------------------------
// Tests — pure logic only
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -------- non_empty_string --------

    #[test]
    fn non_empty_string_returns_trimmed_string() {
        assert_eq!(non_empty_string(&json!("  hi  ")).unwrap(), "hi");
        assert_eq!(non_empty_string(&json!("x")).unwrap(), "x");
    }

    #[test]
    fn non_empty_string_skips_empty_or_blank_or_non_string() {
        assert!(non_empty_string(&json!("")).is_none());
        assert!(non_empty_string(&json!("   ")).is_none());
        assert!(non_empty_string(&json!(1)).is_none());
        assert!(non_empty_string(&json!(null)).is_none());
    }

    // -------- truncate_text --------

    #[test]
    fn truncate_keeps_short_text_unchanged() {
        let r = truncate_text("hello", 100);
        assert_eq!(r.text, "hello");
        assert!(!r.truncated);
    }

    #[test]
    fn truncate_clips_long_text() {
        let r = truncate_text(&"a".repeat(1_000), 50);
        assert_eq!(r.text.chars().count(), 50);
        assert!(r.truncated);
    }

    // -------- author_from --------

    #[test]
    fn author_prefers_agent_id() {
        let a = author_from(&AuthorFromRow {
            author_type: Some("user".to_string()),
            author_agent_id: Some("agent-1".to_string()),
            author_user_id: Some("user-1".to_string()),
        });
        assert_eq!(a.author_type, "agent");
        assert_eq!(a.id, Some("agent-1".to_string()));
    }

    #[test]
    fn author_falls_back_to_user_id() {
        let a = author_from(&AuthorFromRow {
            author_type: None,
            author_agent_id: None,
            author_user_id: Some("user-1".to_string()),
        });
        assert_eq!(a.author_type, "user");
        assert_eq!(a.id, Some("user-1".to_string()));
    }

    #[test]
    fn author_falls_back_to_author_type_or_system() {
        let a = author_from(&AuthorFromRow {
            author_type: Some("system".to_string()),
            author_agent_id: None,
            author_user_id: None,
        });
        assert_eq!(a.author_type, "system");
        assert!(a.id.is_none());

        let a2 = author_from(&AuthorFromRow {
            author_type: Some("unknown".to_string()),
            author_agent_id: None,
            author_user_id: None,
        });
        assert_eq!(a2.author_type, "system");
    }

    #[test]
    fn author_defaults_to_system_when_nothing_known() {
        let a = author_from(&AuthorFromRow::default());
        assert_eq!(a.author_type, "system");
        assert!(a.id.is_none());
    }

    // -------- read_plan_target --------

    #[test]
    fn read_plan_target_accepts_matching_payload() {
        let v = json!({
            "type": "issue_document",
            "key": "plan",
            "issueId": "i-1",
            "documentId": "d-1",
            "revisionId": "r-1",
            "revisionNumber": 3
        });
        let t = read_plan_target(&v, "i-1").unwrap();
        assert_eq!(t.issue_id, "i-1");
        assert_eq!(t.key, "plan");
        assert_eq!(t.document_id, Some("d-1".to_string()));
        assert_eq!(t.revision_id, Some("r-1".to_string()));
        assert_eq!(t.revision_number, Some(3));
    }

    #[test]
    fn read_plan_target_rejects_wrong_type_or_key() {
        let v = json!({"type": "issue", "key": "plan", "issueId": "i-1"});
        assert!(read_plan_target(&v, "i-1").is_none());
        let v = json!({"type": "issue_document", "key": "spec", "issueId": "i-1"});
        assert!(read_plan_target(&v, "i-1").is_none());
    }

    #[test]
    fn read_plan_target_rejects_mismatched_issue() {
        let v = json!({"type": "issue_document", "key": "plan", "issueId": "i-2"});
        assert!(read_plan_target(&v, "i-1").is_none());
    }

    #[test]
    fn read_plan_target_rejects_non_object() {
        assert!(read_plan_target(&json!("nope"), "i-1").is_none());
        assert!(read_plan_target(&json!(null), "i-1").is_none());
    }

    #[test]
    fn read_plan_target_handles_missing_optional_fields() {
        let v = json!({"type": "issue_document", "key": "plan", "issueId": "i-1"});
        let t = read_plan_target(&v, "i-1").unwrap();
        assert!(t.document_id.is_none());
        assert!(t.revision_id.is_none());
        assert!(t.revision_number.is_none());
    }

    // -------- read_result --------

    #[test]
    fn read_result_returns_null_for_empty_object() {
        assert!(read_result(&json!({})).is_none());
    }

    #[test]
    fn read_result_returns_null_for_non_object() {
        assert!(read_result(&json!("nope")).is_none());
        assert!(read_result(&json!(null)).is_none());
    }

    #[test]
    fn read_result_picks_reason_or_rejection_reason() {
        let r = read_result(&json!({"outcome": "approved"})).unwrap();
        assert_eq!(r.outcome, Some("approved".to_string()));
        assert!(r.reason.is_none());
        let r = read_result(&json!({"reason": "good"})).unwrap();
        assert_eq!(r.reason, Some("good".to_string()));
        let r = read_result(&json!({"rejectionReason": "bad"})).unwrap();
        assert_eq!(r.reason, Some("bad".to_string()));
    }

    #[test]
    fn read_result_picks_comment_id() {
        let r = read_result(&json!({"commentId": "c-1"})).unwrap();
        assert_eq!(r.comment_id, Some("c-1".to_string()));
    }

    // -------- serde --------

    #[test]
    fn author_serializes_with_type_field() {
        let a = PlanReviewContextAuthor {
            author_type: "user".to_string(),
            id: Some("u-1".to_string()),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["id"], "u-1");
    }

    #[test]
    fn limits_serialize_camel_case() {
        let v = serde_json::to_value(PLAN_REVIEW_CONTEXT_LIMITS).unwrap();
        assert_eq!(v["maxThreads"], 20);
        assert_eq!(v["maxBodyChars"], 1_200);
    }
}
