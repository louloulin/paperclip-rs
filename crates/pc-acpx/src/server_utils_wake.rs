//! `pc-acpx::server_utils_wake` - port of the wake-payload section of
//! `server-utils.ts` from Node `paperclip/packages/adapter-utils/src/`.
//!
//! R408 covers the sync pure helpers + prompt templates + the
//! `PaperclipWakePayload` normalizer that adapters consume when
//! composing the heartbeat / resume-delta prompt:
//!
//! - Prompt templates: `DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE`,
//!   `WATCHDOG_DEFAULT_MANDATE`
//! - Wake types: `PaperclipWakeIssue`, `PaperclipWakeRecovery`,
//!   `PaperclipWakeAgentMessage`, `PaperclipWakePayload`
//! - Normalizers: `normalize_paperclip_wake_recovery`,
//!   `normalize_paperclip_wake_agent_message`,
//!   `normalize_paperclip_wake_issue`,
//!   `normalize_paperclip_wake_payload`
//! - Stringify helper: `stringify_paperclip_wake_payload`
//! - Reason predicates: `is_paperclip_recovery_wake_payload`,
//!   `read_paperclip_issue_work_mode_from_context`,
//!   `is_assignment_shaped_paperclip_wake_reason`
//! - Task-markdown selector: `select_paperclip_task_markdown`
//!
//! The full `renderPaperclipWakePrompt` (~500 lines in Node, with deeply
//! nested template rendering for comments / annotation deltas /
//! interaction kind / execution stage / etc.) is deferred — adapters
//! that need the full prompt currently read it from Node and the pc-acpx
//! layer only needs the canonical predicate set for prompt routing.

use serde::{Deserialize, Serialize};

use crate::server_utils::{
    as_boolean, as_number, as_string, is_paperclip_runtime_env_key, parse_object,
};

// =============================================================================
// Prompt templates - mirrored 1:1 from Node literal arrays.
// =============================================================================

/// Default Paperclip agent prompt template. Mirrors Node
/// `DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE`. Adapters render this
/// before the wake payload so a fresh session sees the execution
/// contract on its first heartbeat.
pub const DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE: &str = "\
You are agent {{agent.id}} ({{agent.name}}). Continue your Paperclip work.

Execution contract:
- Start actionable work in this heartbeat; do not stop at a plan unless the issue asks for planning.
- Leave durable progress in comments, documents, or work products, then update the issue to a clear final disposition before ending the heartbeat.
- Comments, documents, screenshots, work products, and `Remaining` bullets are evidence, not valid liveness paths by themselves.
- Final disposition checklist: mark `done` when complete; use `in_review` only with a real reviewer, approval, interaction, or monitor path; use `blocked` only with first-class blockers or a named unblock owner/action; create delegated follow-up issues with blockers when another agent owns the next step; keep `in_progress` only when a live continuation path exists.
- Prefer the smallest verification that proves the change; do not default to full workspace typecheck/build/test on every heartbeat unless the task scope warrants it.
- After 2 consecutive failures of the same control-plane write, stop retrying that write for the rest of the heartbeat. Continue useful work, report the failure in the final response, and rely on the adapter/runtime status channel as the sanctioned fallback.
- Use child issues for parallel or long delegated work instead of polling agents, sessions, or processes.
- If woken by a human comment on a dependency-blocked issue, respond or triage the comment without treating the blocked deliverable work as unblocked.
- Create child issues directly when you know what needs to be done; use issue-thread interactions when the board/user must choose suggested tasks, answer structured questions, or confirm a proposal.
- Use `PAPERCLIP_SCRATCH_DIR` / `PAPERCLIP_RUN_SCRATCH_DIR` for temporary scratch files instead of ad hoc `/tmp` paths; Paperclip removes that run-owned directory after the run ends.
- To ask for that input, create an interaction on the current issue with POST /api/issues/{issueId}/interactions using kind suggest_tasks, ask_user_questions, or request_confirmation. Use continuationPolicy wake_assignee when you need to resume after a response (it wakes on acceptance and rejection alike; only expiry does not wake); use wake_assignee_on_accept when you want to resume only after acceptance.
- When you intentionally restart follow-up work on a completed assigned issue, include structured `resume: true` with the POST /api/issues/{issueId}/comments or PATCH /api/issues/{issueId} comment payload. Generic agent comments on closed issues are inert by default.
- For plan approval, update the plan document first, then create request_confirmation targeting the latest plan revision with idempotencyKey confirmation:{issueId}:plan:{revisionId}. Wait for acceptance before creating implementation subtasks, and create a fresh confirmation after superseding board/user comments if approval is still needed.
- If blocked, mark the issue blocked and name the unblock owner and action.
- Respect budget, pause/cancel, approval gates, and company boundaries.";

/// Default watchdog mandate for issue-watchdog resumes. Mirrors Node
/// `WATCHDOG_DEFAULT_MANDATE`. Adapters use this when they re-wake as a
/// task watchdog for an already-stopped subtree.
pub const WATCHDOG_DEFAULT_MANDATE: &str = "\
You are running as a task watchdog, not as the original deliverable worker.
Your mission is to keep the watched issue tree moving by verifying stopped work, not by trusting agent claims.

Mandate:
- Treat every terminal, cancelled, blocked, in-review, or otherwise stopped leaf in the watched subtree as a claim that must be verified against comments, documents, work products, screenshots, tests, blockers, and review state.
- Do not accept \"I could not\" or \"waiting for approval\" as automatically valid. Read the evidence before deciding.
- If a stopped leaf is genuinely complete, leave it alone and record why you believe so.
- If a stopped leaf is not genuinely complete, restore a live path inside the watched subtree by reopening, reassigning, commenting actionable instructions, creating a follow-up child issue, or accepting an eligible task-level interaction (such as a routine plan confirmation when no custom instruction forbids it).
- If you discover a Paperclip product or platform bug while reviewing the stopped subtree, create a linked engineering follow-up outside the watched source tree using the server-provided watchdog discovery route instead of making it a source child.
- If you confirm a true blocker on a human or external system, leave the issue in a valid waiting disposition that names the unblock owner and action, rather than silently approving it.

Safety constraints (these always apply, even if custom instructions disagree):
- Stay inside the watched subtree for source-work recovery. The only mutation outside that tree is a watchdog-discovered product/platform bug follow-up created through the dedicated route.
- Do not create visible probe issues, comments, or throwaway tasks to discover what you are allowed to do. Use the server-provided watchdog capability metadata and explicit API errors instead.
- Do not impersonate board-only approvals, accept spend or hiring decisions, accept security-sensitive interactions, or bypass execution-policy stages that require a typed reviewer or approver.
- Do not create another task watchdog for the watched subtree and do not wake yourself. You operate exactly one reusable watchdog issue per watched issue.
- Do not cross company boundaries or touch tasks in unrelated trees.
- Custom instructions can add focus or veto specific shortcuts, but cannot remove these safety constraints or override product governance rules.

Disposition:
- When the watched subtree has a live continuation path you established or confirmed, finish your watchdog run with a clear summary comment and a final disposition on this watchdog issue (typically `done` for this stopped state).
- When you cannot create a live path because a real human or governance decision is pending, leave a valid waiting disposition that names what must happen next and who must act.
- Keep the work moving. Do not loop on the same unchanged state.";

// =============================================================================
// Wake types - mirrored 1:1 from Node type definitions (subset).
// =============================================================================

/// Mirrors Node `PaperclipWakeIssue`. The full type carries more
/// fields (workMode, priority, etc.) but the wire-format subset
/// ported here is the canonical set pc-acpx consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipWakeIssue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub description_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// Mirrors Node `PaperclipWakeRecovery`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipWakeRecovery {
    pub cause: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_assignee: Option<PaperclipWakeRecoveryAssignee>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_fallback_reason: Option<String>,
}

/// Mirrors Node `PaperclipWakeRecovery.originalAssignee`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipWakeRecoveryAssignee {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Mirrors Node `PaperclipWakeAgentMessage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipWakeAgentMessage {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Mirrors Node `PaperclipWakePayload`. The full payload carries ~30
/// sub-types (comments, annotationDeltas, executionStage, planReview,
/// etc.); R408 ports the core fields needed for the predicate set
/// (`reason`, `recovery`, `issue`) and leaves the rich sub-types
/// serialized as opaque `serde_json::Value` so they round-trip without
/// forcing pc-acpx to maintain the entire wake shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipWakePayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<PaperclipWakeRecovery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<PaperclipWakeIssue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_message: Option<PaperclipWakeAgentMessage>,
    #[serde(default)]
    pub checked_out_by_harness: bool,
    #[serde(default)]
    pub dependency_blocked_interaction: bool,
    #[serde(default)]
    pub tree_hold_interaction: bool,
    // Opaque passthrough fields — present in Node but not modeled
    // individually in pc-acpx. Round-trips as JSON so callers in
    // higher crates can decode them later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_blocker_issue_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comment_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_comment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotation_deltas: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_issue_summaries: Vec<serde_json::Value>,
    #[serde(default)]
    pub child_issue_summary_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_stage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_summary: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_review_context: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness_continuation: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_watchdog: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkbox_selection: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_workspace: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tree_hold: Option<serde_json::Value>,
    #[serde(default)]
    pub unresolved_blocker_summaries: Vec<serde_json::Value>,
    #[serde(default)]
    pub requested_count: i64,
    #[serde(default)]
    pub included_count: i64,
    #[serde(default)]
    pub missing_count: i64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub fallback_fetch_needed: bool,
}

// =============================================================================
// Normalizers - pure functions mirroring Node behavior.
// =============================================================================

/// Return a `PaperclipWakeRecovery` parsed from `value`, or `None` when
/// no `cause` is present. Mirrors Node `normalizePaperclipWakeRecovery`.
#[must_use]
pub fn normalize_paperclip_wake_recovery(
    value: &serde_json::Value,
) -> Option<PaperclipWakeRecovery> {
    let recovery = parse_object(value);
    let cause = as_string(
        recovery.get("cause").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    if cause.is_empty() {
        return None;
    }
    let original_assignee_obj = parse_object(
        recovery
            .get("originalAssignee")
            .unwrap_or(&serde_json::Value::Null),
    );
    let original_assignee_id = as_string(
        original_assignee_obj
            .get("id")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let original_assignee_name = as_string(
        original_assignee_obj
            .get("name")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let original_assignee =
        if !original_assignee_id.is_empty() || !original_assignee_name.is_empty() {
            Some(PaperclipWakeRecoveryAssignee {
                id: if original_assignee_id.is_empty() {
                    None
                } else {
                    Some(original_assignee_id)
                },
                name: if original_assignee_name.is_empty() {
                    None
                } else {
                    Some(original_assignee_name)
                },
            })
        } else {
            None
        };
    let attempt_count = recovery.get("attemptCount").and_then(|v| v.as_i64());
    let max_attempts = recovery.get("maxAttempts").and_then(|v| v.as_i64());
    let failure_summary = as_string(
        recovery
            .get("failureSummary")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let next_action = as_string(
        recovery
            .get("nextAction")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let routing_fallback_reason = as_string(
        recovery
            .get("routingFallbackReason")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    Some(PaperclipWakeRecovery {
        cause,
        failure_summary: if failure_summary.is_empty() {
            None
        } else {
            Some(failure_summary)
        },
        original_assignee,
        attempt_count,
        max_attempts,
        next_action: if next_action.is_empty() {
            None
        } else {
            Some(next_action)
        },
        routing_fallback_reason: if routing_fallback_reason.is_empty() {
            None
        } else {
            Some(routing_fallback_reason)
        },
    })
}

/// Return a sanitized `PaperclipWakeAgentMessage` parsed from `value`,
/// or `None` when the text is empty after stripping control bytes.
/// Mirrors Node `normalizePaperclipWakeAgentMessage` (the
/// `[\u0000-\u0008\u000b-\u001f\u007f]` strip is mirrored as a Rust
/// regex on the same byte ranges).
#[must_use]
pub fn normalize_paperclip_wake_agent_message(
    value: &serde_json::Value,
) -> Option<PaperclipWakeAgentMessage> {
    let message = parse_object(value);
    let raw_text = as_string(message.get("text").unwrap_or(&serde_json::Value::Null), "");
    // Strip terminal-control bytes / NULs / other non-printable controls.
    let text: String = raw_text
        .chars()
        .filter(|&c| !matches!(c as u32, 0x00..=0x08 | 0x0b..=0x1f | 0x7f))
        .collect();
    if text.trim().is_empty() {
        return None;
    }
    let source = as_string(
        message.get("source").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let plugin_key = as_string(
        message.get("pluginKey").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let session_id = as_string(
        message.get("sessionId").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    Some(PaperclipWakeAgentMessage {
        text,
        source: if source.is_empty() {
            None
        } else {
            Some(source)
        },
        plugin_key: if plugin_key.is_empty() {
            None
        } else {
            Some(plugin_key)
        },
        session_id: if session_id.is_empty() {
            None
        } else {
            Some(session_id)
        },
    })
}

/// Return a `PaperclipWakeIssue` parsed from `value`, or `None` when
/// none of `id`, `identifier`, or `title` are present. Mirrors Node
/// `normalizePaperclipWakeIssue`.
#[must_use]
pub fn normalize_paperclip_wake_issue(value: &serde_json::Value) -> Option<PaperclipWakeIssue> {
    let issue = parse_object(value);
    let id = as_string(issue.get("id").unwrap_or(&serde_json::Value::Null), "")
        .trim()
        .to_string();
    let identifier = as_string(
        issue.get("identifier").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let title = as_string(issue.get("title").unwrap_or(&serde_json::Value::Null), "")
        .trim()
        .to_string();
    let work_mode = as_string(
        issue.get("workMode").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    if id.is_empty() && identifier.is_empty() && title.is_empty() && work_mode.is_empty() {
        return None;
    }
    let description_raw = issue
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let description = description_raw
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let status = as_string(issue.get("status").unwrap_or(&serde_json::Value::Null), "")
        .trim()
        .to_string();
    let priority = as_string(
        issue.get("priority").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    Some(PaperclipWakeIssue {
        id: if id.is_empty() { None } else { Some(id) },
        identifier: if identifier.is_empty() {
            None
        } else {
            Some(identifier)
        },
        title: if title.is_empty() { None } else { Some(title) },
        description,
        description_truncated: as_boolean(
            issue
                .get("descriptionTruncated")
                .unwrap_or(&serde_json::Value::Null),
            false,
        ),
        status: if status.is_empty() {
            None
        } else {
            Some(status)
        },
        work_mode: if work_mode.is_empty() {
            None
        } else {
            Some(work_mode)
        },
        priority: if priority.is_empty() {
            None
        } else {
            Some(priority)
        },
    })
}

/// Normalize an arbitrary JSON value into a `PaperclipWakePayload`.
/// Returns `None` when no useful payload content is present (mirrors
/// Node's "all-empty" short-circuit). Mirrors Node
/// `normalizePaperclipWakePayload`.
#[must_use]
pub fn normalize_paperclip_wake_payload(value: &serde_json::Value) -> Option<PaperclipWakePayload> {
    let payload = parse_object(value);
    let comment_ids_v: Vec<String> = match payload.get("commentIds") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let comments_v: Vec<serde_json::Value> = match payload.get("comments") {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        _ => Vec::new(),
    };
    let annotation_deltas_v: Vec<serde_json::Value> = match payload.get("annotationDeltas") {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        _ => Vec::new(),
    };
    let child_issue_summaries_v: Vec<serde_json::Value> = match payload.get("childIssueSummaries") {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        _ => Vec::new(),
    };
    let unresolved_blocker_issue_ids: Vec<String> = match payload.get("unresolvedBlockerIssueIds") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let unresolved_blocker_summaries: Vec<serde_json::Value> =
        match payload.get("unresolvedBlockerSummaries") {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => Vec::new(),
        };

    let recovery = normalize_paperclip_wake_recovery(
        payload.get("recovery").unwrap_or(&serde_json::Value::Null),
    );
    let issue =
        normalize_paperclip_wake_issue(payload.get("issue").unwrap_or(&serde_json::Value::Null));
    let agent_message = normalize_paperclip_wake_agent_message(
        payload
            .get("agentMessage")
            .unwrap_or(&serde_json::Value::Null),
    );
    let execution_stage = payload.get("executionStage").cloned();
    let continuation_summary = payload.get("continuationSummary").cloned();
    let plan_review_context = payload.get("planReviewContext").cloned();
    let liveness_continuation = payload.get("livenessContinuation").cloned();
    let task_watchdog = payload.get("taskWatchdog").cloned();
    let checkbox_selection = payload.get("checkboxSelection").cloned();
    let execution_workspace = payload.get("executionWorkspace").cloned();
    let active_tree_hold = payload.get("activeTreeHold").cloned();

    // Node short-circuits when every optional field is absent.
    let reason = as_string(
        payload.get("reason").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let empty = comments_v.is_empty()
        && comment_ids_v.is_empty()
        && annotation_deltas_v.is_empty()
        && child_issue_summaries_v.is_empty()
        && unresolved_blocker_issue_ids.is_empty()
        && unresolved_blocker_summaries.is_empty()
        && active_tree_hold.is_none()
        && execution_stage.is_none()
        && continuation_summary.is_none()
        && plan_review_context.is_none()
        && liveness_continuation.is_none()
        && task_watchdog.is_none()
        && checkbox_selection.is_none()
        && execution_workspace.is_none()
        && agent_message.is_none()
        && recovery.is_none()
        && issue.is_none()
        && reason.is_empty();
    if empty {
        return None;
    }

    let interaction_kind = as_string(
        payload
            .get("interactionKind")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let interaction_status = as_string(
        payload
            .get("interactionStatus")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let latest_comment_id = as_string(
        payload
            .get("latestCommentId")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    let comment_window = parse_object(
        payload
            .get("commentWindow")
            .unwrap_or(&serde_json::Value::Null),
    );
    let requested_count = as_number(
        comment_window
            .get("requestedCount")
            .unwrap_or(&serde_json::Value::Null),
        (comments_v.len() as f64).max(comment_ids_v.len() as f64),
    ) as i64;
    let included_count = as_number(
        comment_window
            .get("includedCount")
            .unwrap_or(&serde_json::Value::Null),
        comments_v.len() as f64,
    ) as i64;
    let missing_count = as_number(
        comment_window
            .get("missingCount")
            .unwrap_or(&serde_json::Value::Null),
        0.0,
    ) as i64;

    Some(PaperclipWakePayload {
        reason: if reason.is_empty() {
            None
        } else {
            Some(reason)
        },
        recovery,
        issue,
        agent_message,
        checked_out_by_harness: as_boolean(
            payload
                .get("checkedOutByHarness")
                .unwrap_or(&serde_json::Value::Null),
            false,
        ),
        dependency_blocked_interaction: as_boolean(
            payload
                .get("dependencyBlockedInteraction")
                .unwrap_or(&serde_json::Value::Null),
            false,
        ),
        tree_hold_interaction: as_boolean(
            payload
                .get("treeHoldInteraction")
                .unwrap_or(&serde_json::Value::Null),
            false,
        ),
        active_tree_hold,
        unresolved_blocker_issue_ids,
        unresolved_blocker_summaries,
        execution_stage,
        continuation_summary,
        plan_review_context,
        annotation_deltas: annotation_deltas_v,
        liveness_continuation,
        task_watchdog,
        interaction_kind: if interaction_kind.is_empty() {
            None
        } else {
            Some(interaction_kind)
        },
        interaction_status: if interaction_status.is_empty() {
            None
        } else {
            Some(interaction_status)
        },
        checkbox_selection,
        execution_workspace,
        child_issue_summaries: child_issue_summaries_v,
        child_issue_summary_truncated: as_boolean(
            payload
                .get("childIssueSummaryTruncated")
                .unwrap_or(&serde_json::Value::Null),
            false,
        ),
        comment_ids: comment_ids_v,
        latest_comment_id: if latest_comment_id.is_empty() {
            None
        } else {
            Some(latest_comment_id)
        },
        comments: comments_v,
        requested_count,
        included_count,
        missing_count,
        truncated: as_boolean(
            payload.get("truncated").unwrap_or(&serde_json::Value::Null),
            false,
        ),
        fallback_fetch_needed: as_boolean(
            payload
                .get("fallbackFetchNeeded")
                .unwrap_or(&serde_json::Value::Null),
            false,
        ),
    })
}

/// Serialize a normalized wake payload to JSON. When
/// `omit_issue_description` is set, the issue's description +
/// `descriptionTruncated` are dropped (Node semantics). Mirrors Node
/// `stringifyPaperclipWakePayload`.
#[must_use]
pub fn stringify_paperclip_wake_payload(
    value: &serde_json::Value,
    options: StringifyWakePayloadOptions,
) -> Option<String> {
    let mut normalized = normalize_paperclip_wake_payload(value)?;
    if options.omit_issue_description {
        if let Some(issue) = normalized.issue.as_mut() {
            issue.description = None;
            issue.description_truncated = false;
        }
    }
    serde_json::to_string(&normalized).ok()
}

/// Options for [`stringify_paperclip_wake_payload`].
#[derive(Default, Clone, Copy)]
pub struct StringifyWakePayloadOptions {
    pub omit_issue_description: bool,
}

/// Returns `true` when the payload is a recovery wake (carries a
/// non-null `recovery`, or its `reason` is `source_scoped_recovery_action`).
/// Mirrors Node `isPaperclipRecoveryWakePayload`.
#[must_use]
pub fn is_paperclip_recovery_wake_payload(value: &serde_json::Value) -> bool {
    match normalize_paperclip_wake_payload(value) {
        Some(p) => {
            p.recovery.is_some() || p.reason.as_deref() == Some("source_scoped_recovery_action")
        }
        None => false,
    }
}

/// Read the `workMode` from either a direct `paperclipIssue.workMode`
/// field or from the normalized wake payload's issue. Mirrors Node
/// `readPaperclipIssueWorkModeFromContext`.
#[must_use]
pub fn read_paperclip_issue_work_mode_from_context(value: &serde_json::Value) -> Option<String> {
    let context = parse_object(value);
    let issue = parse_object(
        context
            .get("paperclipIssue")
            .unwrap_or(&serde_json::Value::Null),
    );
    let direct = as_string(
        issue.get("workMode").unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    if !direct.is_empty() {
        return Some(direct);
    }
    let wake = normalize_paperclip_wake_payload(
        context
            .get("paperclipWake")
            .unwrap_or(&serde_json::Value::Null),
    );
    wake.and_then(|w| w.issue).and_then(|i| i.work_mode)
}

/// The canonical "assignment-shaped" wake reasons. Mirrors Node
/// `ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS`. These wake kinds restart
/// work on an issue where the session may not have seen the task brief
/// yet even though the adapter session itself is resuming.
pub const ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS: &[&str] = &[
    "issue_assigned",
    "issue_reopened_via_comment",
    "issue_recovery_action_restored",
    "issue_tree_restored",
];

/// Returns `true` when `reason` is one of the
/// [`ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS`]. Mirrors Node
/// `isAssignmentShapedPaperclipWakeReason`.
#[must_use]
pub fn is_assignment_shaped_paperclip_wake_reason(reason: Option<&str>) -> bool {
    match reason {
        Some(r) => ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS.contains(&r),
        None => false,
    }
}

/// Pick the task-context markdown variant for adapters. Fresh sessions
/// get `full`; resumed sessions only get `full` for
/// assignment-shaped / recovery wakes (so the brief re-appears when
/// the issue is being re-handed to the agent). Other resume deltas get
/// `compact` (description stripped). Mirrors Node
/// `selectPaperclipTaskMarkdown`.
#[must_use]
pub fn select_paperclip_task_markdown(
    context: Option<&serde_json::Value>,
    options: SelectTaskMarkdownOptions,
) -> String {
    let ctx = match context {
        Some(c) => c,
        None => return String::new(),
    };
    let full = as_string(
        ctx.get("paperclipTaskMarkdown")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    if full.is_empty() {
        return String::new();
    }
    if !options.resumed_session {
        return full;
    }
    let wake = normalize_paperclip_wake_payload(
        ctx.get("paperclipWake").unwrap_or(&serde_json::Value::Null),
    );
    let Some(wake) = wake else {
        return full;
    };
    if is_assignment_shaped_paperclip_wake_reason(wake.reason.as_deref())
        || wake.recovery.is_some()
        || wake.reason.as_deref() == Some("source_scoped_recovery_action")
    {
        return full;
    }
    let compact = as_string(
        ctx.get("paperclipTaskMarkdownCompact")
            .unwrap_or(&serde_json::Value::Null),
        "",
    )
    .trim()
    .to_string();
    if compact.is_empty() {
        full
    } else {
        compact
    }
}

/// Options for [`select_paperclip_task_markdown`].
#[derive(Default, Clone, Copy)]
pub struct SelectTaskMarkdownOptions {
    pub resumed_session: bool,
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- constants ----------

    #[test]
    fn default_agent_prompt_template_is_non_empty_and_carries_execution_contract() {
        assert!(DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE.contains("Execution contract"));
        assert!(DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE.contains("{{agent.id}}"));
        assert!(DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE.contains("{{agent.name}}"));
        assert!(DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE.contains("PAPERCLIP_SCRATCH_DIR"));
    }

    #[test]
    fn watchdog_default_mandate_carries_safety_constraints() {
        assert!(WATCHDOG_DEFAULT_MANDATE.contains("Safety constraints"));
        assert!(WATCHDOG_DEFAULT_MANDATE.contains("Disposition"));
        assert!(WATCHDOG_DEFAULT_MANDATE.contains("watched subtree"));
    }

    // ---------- normalize_paperclip_wake_recovery ----------

    #[test]
    fn recovery_requires_cause() {
        let empty = serde_json::json!({});
        assert!(normalize_paperclip_wake_recovery(&empty).is_none());
        let blank_cause = serde_json::json!({"cause": "  "});
        assert!(normalize_paperclip_wake_recovery(&blank_cause).is_none());
    }

    #[test]
    fn recovery_parses_full_payload() {
        let v = serde_json::json!({
            "cause": "process_lost",
            "failureSummary": "killed by signal",
            "originalAssignee": { "id": "agent-1", "name": "Agent One" },
            "attemptCount": 3,
            "maxAttempts": 5,
            "nextAction": "retry",
            "routingFallbackReason": "no live assignee"
        });
        let r = normalize_paperclip_wake_recovery(&v).unwrap();
        assert_eq!(r.cause, "process_lost");
        assert_eq!(r.failure_summary.as_deref(), Some("killed by signal"));
        assert_eq!(
            r.original_assignee.as_ref().unwrap().id.as_deref(),
            Some("agent-1")
        );
        assert_eq!(r.attempt_count, Some(3));
        assert_eq!(r.max_attempts, Some(5));
        assert_eq!(r.next_action.as_deref(), Some("retry"));
    }

    // ---------- normalize_paperclip_wake_agent_message ----------

    #[test]
    fn agent_message_strips_control_bytes() {
        let v = serde_json::json!({"text": "hello\u{0000}world\u{0007}!\u{007f}"});
        let m = normalize_paperclip_wake_agent_message(&v).unwrap();
        assert_eq!(m.text, "helloworld!");
    }

    #[test]
    fn agent_message_returns_none_for_empty_after_strip() {
        let v = serde_json::json!({"text": "\u{0000}\u{0001}\u{0002}"});
        assert!(normalize_paperclip_wake_agent_message(&v).is_none());
    }

    // ---------- normalize_paperclip_wake_issue ----------

    #[test]
    fn issue_returns_none_when_all_key_fields_blank() {
        let v = serde_json::json!({});
        assert!(normalize_paperclip_wake_issue(&v).is_none());
    }

    #[test]
    fn issue_parses_minimum_viable_payload() {
        let v = serde_json::json!({"identifier": "ISS-1"});
        let i = normalize_paperclip_wake_issue(&v).unwrap();
        assert_eq!(i.identifier.as_deref(), Some("ISS-1"));
        assert_eq!(i.id, None);
        assert!(!i.description_truncated);
    }

    // ---------- normalize_paperclip_wake_payload ----------

    #[test]
    fn payload_returns_none_when_all_optional_fields_missing() {
        let v = serde_json::json!({"checkedOutByHarness": true});
        assert!(normalize_paperclip_wake_payload(&v).is_none());
    }

    #[test]
    fn payload_returns_some_when_issue_present() {
        let v = serde_json::json!({
            "reason": "issue_assigned",
            "issue": { "id": "iss-1", "title": "Test issue" }
        });
        let p = normalize_paperclip_wake_payload(&v).unwrap();
        assert_eq!(p.reason.as_deref(), Some("issue_assigned"));
        assert_eq!(p.issue.as_ref().unwrap().id.as_deref(), Some("iss-1"));
    }

    // ---------- stringify_paperclip_wake_payload ----------

    #[test]
    fn stringify_returns_none_for_empty_payload() {
        let v = serde_json::json!({});
        assert!(stringify_paperclip_wake_payload(&v, Default::default()).is_none());
    }

    #[test]
    fn stringify_with_omit_issue_description_drops_description_field() {
        let v = serde_json::json!({
            "reason": "issue_assigned",
            "issue": {
                "id": "iss-1",
                "title": "Test",
                "description": "long description",
                "descriptionTruncated": true
            }
        });
        let s = stringify_paperclip_wake_payload(
            &v,
            StringifyWakePayloadOptions {
                omit_issue_description: true,
            },
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["issue"]["description"], serde_json::Value::Null);
        assert_eq!(parsed["issue"]["descriptionTruncated"], false);
    }

    // ---------- reason predicates ----------

    #[test]
    fn is_recovery_payload_when_recovery_present() {
        let v = serde_json::json!({"recovery": {"cause": "process_lost"}});
        assert!(is_paperclip_recovery_wake_payload(&v));
    }

    #[test]
    fn is_recovery_payload_when_reason_is_source_scoped_recovery_action() {
        let v = serde_json::json!({"reason": "source_scoped_recovery_action"});
        assert!(is_paperclip_recovery_wake_payload(&v));
    }

    #[test]
    fn is_not_recovery_for_other_wakes() {
        let v = serde_json::json!({"reason": "issue_assigned"});
        assert!(!is_paperclip_recovery_wake_payload(&v));
    }

    #[test]
    fn is_assignment_shaped_wake_reason_matches_canonical_set() {
        for r in ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS {
            assert!(is_assignment_shaped_paperclip_wake_reason(Some(r)));
        }
        assert!(!is_assignment_shaped_paperclip_wake_reason(Some(
            "unrelated"
        )));
        assert!(!is_assignment_shaped_paperclip_wake_reason(None));
    }

    #[test]
    fn read_work_mode_prefers_direct_field_over_wake_payload() {
        let v = serde_json::json!({
            "paperclipIssue": { "workMode": "explicit" },
            "paperclipWake": { "issue": { "workMode": "wake" } }
        });
        assert_eq!(
            read_paperclip_issue_work_mode_from_context(&v).as_deref(),
            Some("explicit")
        );
    }

    #[test]
    fn read_work_mode_falls_back_to_wake_payload() {
        let v = serde_json::json!({
            "paperclipWake": { "issue": { "workMode": "wake" } }
        });
        assert_eq!(
            read_paperclip_issue_work_mode_from_context(&v).as_deref(),
            Some("wake")
        );
    }

    // ---------- select_paperclip_task_markdown ----------

    #[test]
    fn select_task_markdown_returns_full_for_fresh_session() {
        let ctx = serde_json::json!({
            "paperclipTaskMarkdown": "# Full",
            "paperclipTaskMarkdownCompact": "# Compact"
        });
        let s = select_paperclip_task_markdown(Some(&ctx), Default::default());
        assert_eq!(s, "# Full");
    }

    #[test]
    fn select_task_markdown_returns_full_for_assignment_shaped_resume() {
        let ctx = serde_json::json!({
            "paperclipTaskMarkdown": "# Full",
            "paperclipTaskMarkdownCompact": "# Compact",
            "paperclipWake": { "reason": "issue_assigned" }
        });
        let s = select_paperclip_task_markdown(
            Some(&ctx),
            SelectTaskMarkdownOptions {
                resumed_session: true,
            },
        );
        assert_eq!(s, "# Full");
    }

    #[test]
    fn select_task_markdown_returns_compact_for_other_resume_deltas() {
        let ctx = serde_json::json!({
            "paperclipTaskMarkdown": "# Full",
            "paperclipTaskMarkdownCompact": "# Compact",
            "paperclipWake": { "reason": "issue_commented" }
        });
        let s = select_paperclip_task_markdown(
            Some(&ctx),
            SelectTaskMarkdownOptions {
                resumed_session: true,
            },
        );
        assert_eq!(s, "# Compact");
    }

    #[test]
    fn select_task_markdown_falls_back_to_full_when_compact_missing() {
        let ctx = serde_json::json!({
            "paperclipTaskMarkdown": "# Full",
            "paperclipWake": { "reason": "issue_commented" }
        });
        let s = select_paperclip_task_markdown(
            Some(&ctx),
            SelectTaskMarkdownOptions {
                resumed_session: true,
            },
        );
        assert_eq!(s, "# Full");
    }

    #[test]
    fn select_task_markdown_returns_empty_when_full_missing() {
        let ctx = serde_json::json!({});
        let s = select_paperclip_task_markdown(Some(&ctx), Default::default());
        assert_eq!(s, "");
    }
}
