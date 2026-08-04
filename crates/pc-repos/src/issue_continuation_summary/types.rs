//! Issue continuation summary 域类型与常量。
//!
//! 单一职责：定义 summary doc 的输入/输出类型 + 文档键常量 + 字符上限常量。
//!
//! 与 Node `server/src/services/issue-continuation-summary.ts` 的 `IssueSummaryInput` /
//! `RunSummaryInput` / `AgentSummaryInput` / `IssueContinuationSummaryDocument` 1:1 对齐。

// ============================================================================
// Constants
// ============================================================================

/// Issue continuation summary 文档键（与 Node `ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY` 1:1 对齐）。
///
/// 在 issue / document link 表中作为 `key` 字段使用；UI / 路由层用这个 key 查找对应文档。
pub const ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY: &str = "issue_continuation_summary";

/// Issue continuation summary 文档标题（与 Node `ISSUE_CONTINUATION_SUMMARY_TITLE` 1:1 对齐）。
pub const ISSUE_CONTINUATION_SUMMARY_TITLE: &str = "Continuation Summary";

/// Summary body 最大字符数（与 Node `ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS = 8_000` 1:1 对齐）。
pub const ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS: usize = 8_000;

/// 单个 markdown section 最大字符数（与 Node `SUMMARY_SECTION_MAX_CHARS = 1_200` 1:1 对齐）。
pub const SUMMARY_SECTION_MAX_CHARS: usize = 1_200;

// ============================================================================
// Input types
// ============================================================================

/// Issue summary 输入（与 Node `IssueSummaryInput` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSummaryInput {
    pub id: String,
    pub identifier: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
}

/// Run summary 输入（与 Node `RunSummaryInput` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummaryInput {
    pub id: String,
    pub status: String,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub result_json: Option<serde_json::Value>,
    pub stdout_excerpt: Option<String>,
    pub stderr_excerpt: Option<String>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Agent summary 输入（与 Node `AgentSummaryInput` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSummaryInput {
    pub id: String,
    pub name: String,
    pub adapter_type: Option<String>,
}

/// Continuation summary markdown builder 输入（与 Node `buildContinuationSummaryMarkdown` 参数 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct BuildSummaryInput {
    pub issue: IssueSummaryInput,
    pub run: RunSummaryInput,
    pub agent: AgentSummaryInput,
    pub previous_summary_body: Option<String>,
}

// ============================================================================
// Output types
// ============================================================================

/// Issue continuation summary 文档读取结果（与 Node `IssueContinuationSummaryDocument` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct IssueContinuationSummaryDocument {
    pub key: String,
    pub title: Option<String>,
    pub body: String,
    pub latest_revision_id: Option<String>,
    pub latest_revision_number: i64,
    pub source_trust: Option<serde_json::Value>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
