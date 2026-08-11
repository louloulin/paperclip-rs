//! Types —— Issue continuation summary DTOs and constants.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;
use uuid::Uuid;

/// Document key 标签（与 Node `ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY` 1:1 对齐）。
pub const ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY: &str = "continuation_summary";

/// Document title（与 Node `ISSUE_CONTINUATION_SUMMARY_TITLE` 1:1 对齐）。
pub const ISSUE_CONTINUATION_SUMMARY_TITLE: &str = "Continuation Summary";

/// Summary body 最大字符数（与 Node `ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS` 1:1 对齐）。
pub const ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS: usize = 8_000;

/// 每个 markdown section 最大字符数（与 Node `SUMMARY_SECTION_MAX_CHARS` 1:1 对齐）。
pub const SUMMARY_SECTION_MAX_CHARS: usize = 1_200;

/// Path candidate regex（与 Node `PATH_CANDIDATE_RE` 1:1 对齐）。
pub static PATH_CANDIDATE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r#"(?:^|[\s`"'(])((?:server|ui|packages|doc|scripts|\.github)/[A-Za-z0-9._/-]+)"#,
    )
    .expect("valid path regex")
});

/// Waiting for review/approval regex（与 Node `WAITING_FOR_REVIEW_OR_APPROVAL_RE` 1:1 对齐）。
pub static WAITING_FOR_REVIEW_OR_APPROVAL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\bwait(?:ing)?\s+for\b.{0,160}\b(?:review(?:er)?(?: feedback)?|approval|board|human|user|operator)\b")
        .expect("valid waiting regex")
});

/// Mode 枚举（与 Node `inferMode` 返回字符串对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContinuationSummaryMode {
    Review,
    Implementation,
    Plan,
}

impl ContinuationSummaryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Implementation => "implementation",
            Self::Plan => "plan",
        }
    }
}

/// Issue 摘要输入（与 Node `IssueSummaryInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSummaryInput {
    pub id: String,
    pub identifier: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
}

/// Run 摘要输入（与 Node `RunSummaryInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummaryInput {
    pub id: String,
    pub status: String,
    pub error: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub result_json: Option<Value>,
    #[serde(default)]
    pub stdout_excerpt: Option<String>,
    #[serde(default)]
    pub stderr_excerpt: Option<String>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
}

/// Agent 摘要输入（与 Node `AgentSummaryInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummaryInput {
    pub id: String,
    pub name: String,
    pub adapter_type: Option<String>,
}

/// Continuation summary document（与 Node `IssueContinuationSummaryDocument` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueContinuationSummaryDocument {
    pub key: String,
    pub title: Option<String>,
    pub body: String,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: i32,
    pub source_trust: Option<Value>,
    pub updated_at: DateTime<Utc>,
}

/// Refresh 输入（与 Node `refreshIssueContinuationSummary` 入参 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshContinuationSummaryInput {
    pub db_company_id: Uuid,
    pub issue_id: Uuid,
    pub run: RunSummaryInput,
    pub agent: AgentSummaryInput,
}

/// Markdown builder 输入（与 Node `buildContinuationSummaryMarkdown` 入参 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildContinuationSummaryInput {
    pub issue: IssueSummaryInput,
    pub run: RunSummaryInput,
    pub agent: AgentSummaryInput,
    #[serde(default)]
    pub previous_summary_body: Option<String>,
}
