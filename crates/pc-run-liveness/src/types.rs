//! Types —— Run liveness DTOs and constants.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

/// Liveness state（与 Node `RunLivenessState` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLivenessState {
    /// Run 成功且有进展
    Advanced,
    /// Run 失败
    Failed,
    /// 需要后续跟进
    NeedsFollowup,
    /// 仅规划无实际执行
    PlanOnly,
    /// 空响应（无输出无证据）
    EmptyResponse,
    /// 已完成
    Completed,
    /// 阻塞中
    Blocked,
}

impl RunLivenessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advanced => "advanced",
            Self::Failed => "failed",
            Self::NeedsFollowup => "needs_followup",
            Self::PlanOnly => "plan_only",
            Self::EmptyResponse => "empty_response",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }
}

/// Actionability（与 Node `RunLivenessActionability` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLivenessActionability {
    Runnable,
    ManagerReview,
    BlockedExternal,
    ApprovalRequired,
    Unknown,
}

impl RunLivenessActionability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runnable => "runnable",
            Self::ManagerReview => "manager_review",
            Self::BlockedExternal => "blocked_external",
            Self::ApprovalRequired => "approval_required",
            Self::Unknown => "unknown",
        }
    }
}

/// Issue input（与 Node `RunLivenessIssueInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLivenessIssueInput {
    pub status: String,
    pub title: String,
    pub description: Option<String>,
}

/// Evidence input（与 Node `RunLivenessEvidenceInput` 1:1 对齐）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLivenessEvidenceInput {
    pub issue_comments_created: i64,
    pub document_revisions_created: i64,
    pub plan_document_revisions_created: i64,
    pub work_products_created: i64,
    pub workspace_operations_created: i64,
    pub activity_events_created: i64,
    pub tool_or_action_events_created: i64,
    pub latest_evidence_at: Option<DateTime<Utc>>,
}

/// Classification input（与 Node `RunLivenessClassificationInput` 1:1 对齐）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLivenessClassificationInput {
    pub run_status: String,
    pub issue: Option<RunLivenessIssueInput>,
    pub result_json: Option<Value>,
    pub issue_comment_bodies: Option<Vec<String>>,
    pub continuation_summary_body: Option<String>,
    pub stdout_excerpt: Option<String>,
    pub stderr_excerpt: Option<String>,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub continuation_attempt: Option<i64>,
    pub evidence: Option<RunLivenessEvidenceInput>,
}

/// Classification output（与 Node `RunLivenessClassification` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunLivenessClassification {
    pub liveness_state: RunLivenessState,
    pub liveness_reason: String,
    pub continuation_attempt: i64,
    pub last_useful_action_at: Option<DateTime<Utc>>,
    pub next_action: Option<String>,
    pub actionability: RunLivenessActionability,
}

/// Unmanaged background task stop reason constant。
pub const UNMANAGED_BACKGROUND_TASK_STOP_REASON: &str = "unmanaged_background_task_stopped";

/// Unmanaged background task liveness reason constant。
pub const UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON: &str =
    "unmanaged background task stopped; no durable live path";

// ============================================================================
// Regex 模式（与 Node 1:1 对齐）
// ============================================================================

/// Planning-only re: "I'll first inspect ..." / "next: do X" 等。
pub static PLANNING_ONLY_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:i(?:'ll| will| am going to|'m going to)|let me|i need to|next(?:,| i will| i'll)?|my next step is|the next step is)\s+(?:first\s+)?(?:inspect|check|review|look|investigate|analy[sz]e|open|read|start|begin|work on|implement|fix|test|update|create|add)\b"
    ).expect("valid planning regex")
});

/// Next steps prefix: "Next steps:" / "Plan:".
pub static NEXT_STEPS_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?im)^\s*(?:next steps?|plan)\s*:")
        .expect("valid next steps regex")
});

/// Generic blocker re.
pub static BLOCKER_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:blocked|can't proceed|cannot proceed|unable to proceed|waiting on|need(?:s|ed)? .{0,80}\b(?:approval|access|credential|credentials|secret|api key|token|input|clarification)|requires? .{0,80}\b(?:approval|access|credential|credentials|secret|api key|token|input|clarification))\b"
    ).expect("valid blocker regex")
});

/// Negated blocker re: "not blocked" / "no blockers".
pub static NEGATED_BLOCKER_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:not blocked|no blocker|no blockers|unblocked)\b")
        .expect("valid negated blocker regex")
});

/// Approval required re.
pub static APPROVAL_REQUIRED_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:approval required|requires? .{0,80}\bapproval|need(?:s|ed)? .{0,80}\bapproval|waiting on .{0,80}\bapproval|pending approval|board approval|human approval|user approval|operator approval)\b"
    ).expect("valid approval regex")
});

/// External blocker re (need access/secret/etc).
pub static EXTERNAL_BLOCKER_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:can't proceed|cannot proceed|unable to proceed|waiting on|blocked by|blocked on|need(?:s|ed)?|requires?) .{0,120}\b(?:access|credential|credentials|secret|secrets|api key|token|password|login|account|permission|permissions|input|clarification)\b"
    ).expect("valid external blocker regex")
});

/// Manager review re.
pub static MANAGER_REVIEW_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:manager review|human review|manual review|security review|escalate|production deploy|deploy(?:ing)? to production|deploy(?:ing)? to prod|prod deploy|production access|rotate .{0,40}\b(?:secret|key|token)|delete .{0,40}\bproduction|security-sensitive|credentialed operation|budget-sensitive|cost approval|spend approval)\b"
    ).expect("valid manager review regex")
});

/// Runnable re (commands / verbs to continue).
pub static RUNNABLE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:(?:run|rerun|execute)\s+(?:pnpm|npm|yarn|bun|vitest|jest|pytest|cargo|go test|curl|tests?|typecheck|build|lint|package|verification)|(?:inspect|check|review|look|investigate|analy[sz]e|open|read|start|begin|continue|implement|fix|test|update|create|add|write|verify|validate|report)\b)"
    ).expect("valid runnable regex")
});

/// Plan task title re.
pub static PLAN_TASK_TITLE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:plan|planning|analysis|investigation|research|report|proposal|design doc|write-?up)\b")
        .expect("valid plan task title regex")
});

/// Plan task description re.
pub static PLAN_TASK_DESCRIPTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:create|write|produce|draft|update|revise|prepare)\s+(?:a\s+|the\s+)?(?:plan|analysis|investigation|research report|report|proposal|design doc|write-?up)\b")
        .expect("valid plan task desc regex")
});
