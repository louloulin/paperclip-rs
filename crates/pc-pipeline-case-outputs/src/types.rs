#![forbid(unsafe_code)]
//! Pipeline case outputs 类型契约（与 Node @paperclipai/shared 1:1）。
//!
//! 对应 Node \`server/src/services/pipeline-case-outputs.ts\` 引用的类型。

use serde::{Deserialize, Serialize};

/// 源码信任元数据（来自 source-trust）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTrustMetadata {
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub trust_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCaseOutputItem {
    pub id: String,
    pub kind: PipelineCaseOutputItemKind,
    pub title: String,
    pub source_issue_id: String,
    pub source_issue_identifier: Option<String>,
    pub source_issue_path: String,
    pub source_issue_title: String,
    pub source_issue_status: String,
    pub source_role: String,
    #[serde(default)]
    pub source_trust: Option<SourceTrustMetadata>,
    #[serde(default)]
    pub source_run_id: Option<String>,
    #[serde(default)]
    pub source_agent_id: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub document_id: Option<String>,
    #[serde(default)]
    pub document_key: Option<String>,
    #[serde(default)]
    pub document_title: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub latest_revision_id: Option<String>,
    #[serde(default)]
    pub latest_revision_number: Option<i32>,
    #[serde(default)]
    pub document_path: Option<String>,
    #[serde(default)]
    pub work_product_id: Option<String>,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub review_state: Option<String>,
    #[serde(default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub content_path: Option<String>,
    #[serde(default)]
    pub download_path: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineCaseOutputItemKind {
    Document,
    WorkProduct,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCaseOutputsResponse {
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub case_id: Option<String>,
    pub generated_at: String,
    pub items: Vec<PipelineCaseOutputItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCaseOutputContextSummaryItem {
    pub id: String,
    pub kind: PipelineCaseOutputItemKind,
    pub title: String,
    pub key: Option<String>,
    #[serde(default)]
    pub revision_id: Option<String>,
    #[serde(default)]
    pub revision_number: Option<i32>,
    pub source_issue: PipelineCaseOutputContextSourceIssue,
    #[serde(default)]
    pub source_run_id: Option<String>,
    #[serde(default)]
    pub source_agent_id: Option<String>,
    #[serde(default)]
    pub source_trust: Option<SourceTrustMetadata>,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub excerpt_truncated: bool,
    #[serde(default)]
    pub fetch_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCaseOutputContextSourceIssue {
    pub id: String,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub path: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCaseOutputContextSummary {
    pub generated_at: String,
    pub item_count: usize,
    pub total_item_count: usize,
    pub omitted_item_count: usize,
    pub excerpt_max_chars: usize,
    pub redaction_note: String,
    pub items: Vec<PipelineCaseOutputContextSummaryItem>,
}
