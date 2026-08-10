//! 批处理 company-import writers 的行类型（原 `pc-import-write-types` 已下沉）。
//!
//! 对齐 Node `services/import-write-types.ts`：导入管线提前解析所有实体
//! （slug → id、label 重映射、状态降级、blob 校验、embedded-asset 重写），
//! 然后把完全解析后的行交给这些 writers 做 chunked multi-row INSERT。
//!
//! 所有 id 由 caller 预生成，因此 children 可以引用 parents 而无需 per-row
//! `.returning()` round-trip。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Issue 作者类型（user / agent / system）。Rust 用 String 与 pc-repos 对齐。
pub type IssueCommentAuthorType = String;

/// Issue 评论 presentation JSON。
pub type IssueCommentPresentation = Value;

/// Issue 评论 metadata JSON。
pub type IssueCommentMetadata = Value;

/// 已解析 issue 行（id 预生成）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportIssueRow {
    /// Pre-generated issue id.
    pub id: Uuid,
    /// Source slug，caller 在批处理后用此字段做关联。
    pub ref_: String,
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub assignee_agent_id: Option<Uuid>,
    /// 与 Node `IssueStatus` 等价的字符串（caller 保证合法）。
    pub status: String,
    /// 与 Node `IssuePriority` 等价的字符串（caller 保证合法）。
    pub priority: String,
    pub billing_code: Option<String>,
    pub assignee_adapter_overrides: Option<Value>,
    pub execution_workspace_settings: Option<Value>,
    pub label_ids: Vec<Uuid>,
    /// Imported monitors land un-armed；只恢复 notes / provenance。
    pub monitor_notes: Option<String>,
    pub monitor_scheduled_by: Option<String>,
}

/// 已解析 comment 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportIssueCommentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub body: String,
    pub author_type: IssueCommentAuthorType,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<String>,
    pub presentation: Option<IssueCommentPresentation>,
    pub metadata: Option<IssueCommentMetadata>,
    pub created_at: Option<DateTime<Utc>>,
}

/// 已解析 attachment（asset + link）行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportIssueAttachmentRow {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub issue_comment_id: Option<Uuid>,
    pub provider: String,
    pub object_key: String,
    pub content_type: String,
    pub byte_size: i64,
    pub sha256: String,
    pub original_filename: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

/// 已解析 issue-document 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportIssueDocumentRow {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub key: String,
    pub title: Option<String>,
    pub format: String,
    pub body: String,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub source_trust: Option<Value>,
}

/// 已解析 work-product 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportIssueWorkProductRow {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub project_id: Option<Uuid>,
    #[serde(rename = "type")]
    pub kind: String,
    pub provider: String,
    pub external_id: Option<String>,
    pub title: String,
    pub url: Option<String>,
    pub status: String,
    pub review_state: String,
    pub is_primary: bool,
    pub health_status: String,
    pub summary: Option<String>,
    pub metadata: Option<Value>,
    pub execution_workspace_id: Option<Uuid>,
    pub runtime_service_id: Option<Uuid>,
    pub created_by_run_id: Option<Uuid>,
    pub source_trust: Option<Value>,
}
