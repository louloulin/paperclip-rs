//! Issue reference business types — strict alignment with Node shared types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_core::Timestamp;

pub const SOURCE_KIND_TITLE: &str = "title";
pub const SOURCE_KIND_DESCRIPTION: &str = "description";
pub const SOURCE_KIND_DOCUMENT: &str = "document";
pub const SOURCE_KIND_COMMENT: &str = "comment";

/// Source kind enum — 与 Node `IssueReferenceSourceKind` 严格对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueReferenceSourceKind {
    Title,
    Description,
    Document,
    Comment,
}

impl IssueReferenceSourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Title => SOURCE_KIND_TITLE,
            Self::Description => SOURCE_KIND_DESCRIPTION,
            Self::Document => SOURCE_KIND_DOCUMENT,
            Self::Comment => SOURCE_KIND_COMMENT,
        }
    }
}

/// 单条 source 描述 — 用于 relatedWorkForIssue 输出的 sources 数组。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReferenceSource {
    pub kind: String,
    pub source_record_id: Option<Uuid>,
    pub document_key: Option<String>,
    pub label: String,
}

/// 出站 / 入站相关 issue 摘要 — 与 Node `IssueRelationIssueSummary` 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceRelatedIssueSummary {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
}

/// 单条 related work 项 — 与 Node `IssueRelatedWorkItem` 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedWorkItem {
    pub issue: ReferenceRelatedIssueSummary,
    pub mention_count: i64,
    pub sources: Vec<IssueReferenceSource>,
}

/// Issue reference mention 行（对外视图）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReferenceMentionView {
    pub id: Uuid,
    pub source_issue_id: Uuid,
    pub target_issue_id: Uuid,
    pub source_kind: String,
    pub source_record_id: Option<Uuid>,
    pub document_key: Option<String>,
    pub matched_text: Option<String>,
    pub created_at: Timestamp,
}
