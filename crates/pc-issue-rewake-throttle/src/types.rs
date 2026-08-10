//! Types —— Issue rewake throttle DTOs and constants.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Consecutive no-progress runs required before the cooldown engages。
pub const ISSUE_REWAKE_NO_PROGRESS_THRESHOLD: usize = 2;

/// Cooldown after the threshold streak; doubles per additional no-progress run。
pub const ISSUE_REWAKE_BASE_COOLDOWN_MS: u64 = 120_000;

/// Upper bound for the escalating cooldown (30 minutes)。
pub const ISSUE_REWAKE_MAX_COOLDOWN_MS: u64 = 30 * 60_000;

/// Only runs newer than this feed the streak; older history is ignored (6 hours)。
pub const ISSUE_REWAKE_LOOKBACK_MS: u64 = 6 * 60 * 60_000;

/// How many recent terminal runs to sample when computing the streak。
pub const ISSUE_REWAKE_RUN_SAMPLE_LIMIT: usize = 8;

/// Wake reasons that assert issue state rather than deliver a new event.
///
/// These (plus reason-less on-demand invokes) are the only wakes the throttle
/// applies to; every event-shaped reason passes through.
pub const THROTTLED_ISSUE_REWAKE_REASONS: &[&str] = &[
    "issue_assigned",
    "issue_continuation_needed",
    "issue_assignment_recovery",
    "issue_graph_liveness_backstop",
];

/// Activity actions that count as issue-visible progress when attributed to a run.
///
/// Deliberately narrower than run-liveness "concrete action evidence": tool calls
/// inside the workspace do not move the issue, so they do not reset the streak.
pub const ISSUE_PROGRESS_ACTIVITY_ACTIONS: &[&str] = &[
    "issue.updated",
    "issue.comment_added",
    "issue.created",
    "issue.child_created",
    "issue.assigned",
    "issue.released",
    "issue.blockers_updated",
    "issue.document_upserted",
    "issue.document_updated",
    "issue.document_deleted",
    "issue.document_restored",
    "issue.document_annotation_comment_added",
    "issue.document_annotation_thread_created",
    "issue.document_annotation_thread_resolved",
    "issue.work_product_created",
    "issue.work_product_updated",
    "issue.work_product_deleted",
    "issue.attachment_added",
    "issue.attachment_removed",
    "issue.thread_interaction_created",
    "issue.monitor_scheduled",
    "issue.approval_linked",
];

/// Activity on the issue that counts as new external input since the last run
/// finished — anything a waiting agent should be woken for.
pub const ISSUE_NEW_INPUT_ACTIVITY_ACTIONS: &[&str] = &[
    "issue.updated",
    "issue.comment_added",
    "issue.created",
    "issue.child_created",
    "issue.assigned",
    "issue.released",
    "issue.blockers_updated",
    "issue.document_upserted",
    "issue.document_updated",
    "issue.document_deleted",
    "issue.document_restored",
    "issue.document_annotation_comment_added",
    "issue.document_annotation_thread_created",
    "issue.document_annotation_thread_resolved",
    "issue.work_product_created",
    "issue.work_product_updated",
    "issue.work_product_deleted",
    "issue.attachment_added",
    "issue.attachment_removed",
    "issue.thread_interaction_created",
    "issue.monitor_scheduled",
    "issue.approval_linked",
    "issue.thread_interaction_accepted",
    "issue.thread_interaction_answered",
    "issue.thread_interaction_item_verdicts_submitted",
    "issue.blockers_resolved_wake_emitted",
];

/// Wake candidate input。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRewakeCandidateInput {
    pub reason: Option<String>,
    pub wake_comment_id: Option<String>,
    pub force_fresh_session: bool,
    pub has_explicit_resume: bool,
}

/// Recent issue run sample (for streak calculation)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentIssueRunSample {
    pub id: String,
    pub status: String,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Throttle decision input。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRewakeThrottleInput {
    pub now: DateTime<Utc>,
    /// Terminal runs for the same (agent, issue), newest finish first.
    pub recent_terminal_runs: Vec<RecentIssueRunSample>,
    /// Runs among the sample that produced issue-visible progress.
    pub run_ids_with_issue_progress: HashSet<String>,
    /// New issue input landed after the newest run finished.
    pub has_new_issue_input_since_last_run: bool,
}

/// Throttle decision output。
///
/// - `Allowed` —— 不节流
/// - `Blocked` —— 节流到指定时间
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "blocked", rename_all = "camelCase")]
pub enum IssueRewakeThrottleDecision {
    #[serde(rename = "false")]
    Allowed { no_progress_streak: usize },
    #[serde(rename = "true")]
    Blocked {
        no_progress_streak: usize,
        cooldown_ms: u64,
        last_run_finished_at: DateTime<Utc>,
        next_allowed_at: DateTime<Utc>,
    },
}
