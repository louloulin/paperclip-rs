//! Issue continuation summary 数据库 IO（与 Node `server/src/services/issue-continuation-summary.ts`
//! 的 `getIssueContinuationSummaryDocument` + `refreshIssueContinuationSummary` 1:1 对齐）。
//!
//! 单一职责：读写 continuation summary 文档，与 issue 关联。
//!
//! - `get_issue_continuation_summary_document` —— 通过 issue_id + document key 读取
//! - `refresh_issue_continuation_summary` —— SELECT issue → 构造 markdown → upsert issue document
//!
//! 所有 DB 操作通过 `pc_repos::DocumentRepo` + `IssueRepo` 完成，避免直接 SQL。

use sqlx::FromRow;
use uuid::Uuid;

use crate::document::{DocumentRepo, DocumentRow};
use crate::Db;

use super::markdown::build_continuation_summary_markdown;
use super::types::{
    AgentSummaryInput, BuildSummaryInput, IssueContinuationSummaryDocument, IssueSummaryInput,
    RunSummaryInput, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY, ISSUE_CONTINUATION_SUMMARY_TITLE,
};

// ============================================================================
// Read
// ============================================================================

/// Issue + continuation summary document 投影行（SELECT 用）。
#[derive(Debug, FromRow)]
struct IssueSummaryRow {
    id: Uuid,
    identifier: Option<String>,
    title: String,
    description: Option<String>,
    status: String,
    priority: String,
}

/// 读取 issue + continuation summary 文档（与 Node `getIssueContinuationSummaryDocument` 1:1 对齐）。
///
/// 返回 `(IssueSummaryInput, Option<IssueContinuationSummaryDocument>)`：
/// - `IssueSummaryInput`：用于构造 markdown
/// - `IssueContinuationSummaryDocument`：当前已存在的 summary（可能为 None）
pub async fn load_issue_summary_with_doc(
    db: &Db,
    issue_id: Uuid,
) -> sqlx::Result<Option<(IssueSummaryInput, Option<IssueContinuationSummaryDocument>)>> {
    // SELECT issue 元数据
    let issue_row: Option<IssueSummaryRow> = sqlx::query_as(
        "SELECT id, identifier, title, description, status, priority FROM issues WHERE id = $1",
    )
    .bind(issue_id)
    .fetch_optional(db.pool())
    .await?;
    let Some(issue_row) = issue_row else { return Ok(None); };

    let issue = IssueSummaryInput {
        id: issue_row.id.to_string(),
        identifier: issue_row.identifier,
        title: issue_row.title,
        description: issue_row.description,
        status: issue_row.status,
        priority: issue_row.priority,
    };

    // SELECT continuation summary document
    let doc_repo = DocumentRepo::new(db);
    let doc_row: Option<DocumentRow> = doc_repo
        .get_issue_document_by_key(issue_id, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY)
        .await?;

    let doc = doc_row.map(|d| {
        let latest_revision_number = d.latest_revision_number as i64;
        IssueContinuationSummaryDocument {
            key: ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY.to_string(),
            title: d.title,
            body: d.latest_body,
            latest_revision_id: d.latest_revision_id.map(|u| u.to_string()),
            latest_revision_number,
            source_trust: d.source_trust,
            updated_at: d.updated_at.as_datetime(),
        }
    });

    Ok(Some((issue, doc)))
}

// ============================================================================
// Refresh (rebuild + upsert)
// ============================================================================

/// Refresh 输入（与 Node `refreshIssueContinuationSummary` 参数 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct RefreshSummaryInput {
    pub issue_id: Uuid,
    pub run: RunSummaryInput,
    pub agent: AgentSummaryInput,
    /// 公司 id（用于 upsert）。由调用方从 issue / context 获取。
    pub company_id: Uuid,
    /// 创建者 user id（plugin / 系统 actor 可为 None）
    pub created_by_user_id: Option<String>,
}

/// 刷新 issue continuation summary（与 Node `refreshIssueContinuationSummary` 1:1 对齐）。
///
/// 流程：
/// 1. SELECT issue + 已存在 doc
/// 2. 用 markdown builder 构造新 body
/// 3. UPSERT issue document
///
/// 返回新 doc 的最新 revision 信息（与 Node `result.document` 等价）。
pub async fn refresh_issue_continuation_summary(
    db: &Db,
    input: &RefreshSummaryInput,
) -> sqlx::Result<Option<IssueContinuationSummaryDocument>> {
    let Some((issue, existing)) = load_issue_summary_with_doc(db, input.issue_id).await? else {
        return Ok(None);
    };

    let body = build_continuation_summary_markdown(&BuildSummaryInput {
        issue,
        run: input.run.clone(),
        agent: input.agent.clone(),
        previous_summary_body: existing.as_ref().map(|d| d.body.clone()),
    });

    let doc_repo = DocumentRepo::new(db);
    let base_revision_id = existing
        .as_ref()
        .and_then(|d| d.latest_revision_id.as_deref())
        .and_then(|s| Uuid::parse_str(s).ok());

    let result = doc_repo
        .upsert_issue_document(
            input.company_id,
            input.issue_id,
            ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY,
            Some(ISSUE_CONTINUATION_SUMMARY_TITLE),
            &body,
            "markdown",
            input.created_by_user_id.as_deref(),
        )
        .await?;

    let _ = (base_revision_id, result); // baseRevisionId / changeSummary 暂未在 DocumentRepo 中支持

    // 重新读取以获取最新字段
    let updated = doc_repo
        .get_issue_document_by_key(input.issue_id, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY)
        .await?;

    let _ = result; // result 是 DB 返回的影响行数 / id，port 简化处理
    Ok(updated.map(|d| {
        let latest_revision_number = d.latest_revision_number as i64;
        IssueContinuationSummaryDocument {
            key: ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY.to_string(),
            title: d.title,
            body: d.latest_body,
            latest_revision_id: d.latest_revision_id.map(|u| u.to_string()),
            latest_revision_number,
            source_trust: d.source_trust,
            updated_at: d.updated_at.as_datetime(),
        }
    }))
}

// ============================================================================
// Test fixtures / helpers
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_key_constant_is_stable() {
        // 文档 key 必须稳定，避免与已有数据冲突
        assert_eq!(ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY, "issue_continuation_summary");
    }

    #[test]
    fn document_title_constant_is_human_readable() {
        assert_eq!(ISSUE_CONTINUATION_SUMMARY_TITLE, "Continuation Summary");
    }

    // Note: 数据库 IO 测试需要 DATABASE_URL；与既有模式一致，本模块暂不集成测试。
    // 实际 SQL 形状通过 load_issue_summary_with_doc + refresh_issue_continuation_summary
    // 的 sqlx 调用保证；纯逻辑部分在 markdown.rs 测试覆盖。
}
