#![forbid(unsafe_code)]

//! Run liveness continuation — 1:1 port of paperclip/server/src/services/recovery/run-liveness-continuations.ts.
//!
//! Pure decision logic for whether a heartbeat run should be re-enqueued
//! after ending in a non-actionable liveness state (plan_only / empty_response).

use serde::{Deserialize, Serialize};

/// Reason constant — Node `RUN_LIVENESS_CONTINUATION_REASON`.
pub const RUN_LIVENESS_CONTINUATION_REASON: &str = "run_liveness_continuation";

/// Default max attempts — Node `DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS = 2`.
pub const DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS: u32 = 2;

/// Issue status values that allow continuation.
pub const CONTINUATION_ACTIVE_ISSUE_STATUSES: &[&str] = &["todo", "in_progress"];

/// Agent status values that allow invocation.
pub const CONTINUATION_AGENT_STATUSES: &[&str] = &["active", "idle", "running", "error"];

/// Liveness states that are actionable for continuation.
pub const ACTIONABLE_LIVENESS_STATES: &[&str] = &["plan_only", "empty_response"];

/// Idempotent wake status values that mean a previous wake exists.
pub const IDEMPOTENT_WAKE_STATUSES: &[&str] = &[
    "queued",
    "deferred_issue_execution",
    "completed",
];

/// Minimal input shape for `decide_run_liveness_continuation`.
///
/// Mirrors the Node function's input parameter shape (without DB-bound fields).
#[derive(Debug, Clone)]
pub struct ContinuationInput<'a> {
    /// Current attempt count stored on the run (parsed from any value).
    pub current_attempt: Option<u32>,
    /// Whether the issue is still in an actionable status.
    pub issue_status: Option<&'a str>,
    /// Whether the issue is blocked by execution policy.
    pub issue_execution_state: Option<&'a str>,
    /// Issue assignee agent id.
    pub issue_assignee_agent_id: Option<&'a str>,
    /// Run's agent id.
    pub run_agent_id: &'a str,
    /// Agent's status.
    pub agent_status: Option<&'a str>,
    /// Liveness state of the source run.
    pub liveness_state: Option<&'a str>,
    /// Whether the budget is hard-stopped.
    pub budget_blocked: bool,
    /// Whether a wakeup request with the same idempotency key already exists.
    pub idempotent_wake_exists: bool,
    /// Override the default max attempts.
    pub max_attempts: Option<u32>,
}

/// Decision enum mirroring Node's `RunContinuationDecision` discriminated union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RunContinuationDecision {
    /// Enqueue a new wakeup request.
    Enqueue {
        next_attempt: u32,
        idempotency_key: String,
    },
    /// Bounded continuation attempts exhausted.
    Exhausted {
        attempt: u32,
        max_attempts: u32,
        comment: String,
    },
    /// Skip continuation for a specific reason.
    Skip {
        reason: String,
    },
}

/// Parse a continuation attempt value (Node `readContinuationAttempt`).
///
/// Accepts `u32` directly or numeric strings. Returns 0 for non-positive / unparseable.
pub fn read_continuation_attempt(value: Option<u32>) -> u32 {
    match value {
        Some(n) if n > 0 => n,
        _ => 0,
    }
}

/// Build idempotency key (Node `buildRunLivenessContinuationIdempotencyKey`).
///
/// Format: `{reason}:{issueId}:{sourceRunId}:{livenessState}:{nextAttempt}`
pub fn build_run_liveness_continuation_idempotency_key(
    issue_id: &str,
    source_run_id: &str,
    liveness_state: &str,
    next_attempt: u32,
) -> String {
    format!(
        "{RUN_LIVENESS_CONTINUATION_REASON}:{issue_id}:{source_run_id}:{liveness_state}:{next_attempt}"
    )
}

/// Check whether the liveness state is actionable.
pub fn is_actionable_liveness_state(state: Option<&str>) -> bool {
    match state {
        Some(s) => ACTIONABLE_LIVENESS_STATES.contains(&s),
        None => false,
    }
}

/// Check whether the issue status allows continuation.
pub fn is_continuation_issue_status(status: Option<&str>) -> bool {
    match status {
        Some(s) => CONTINUATION_ACTIVE_ISSUE_STATUSES.contains(&s),
        None => false,
    }
}

/// Check whether the agent status allows invocation.
pub fn is_continuation_agent_status(status: Option<&str>) -> bool {
    match status {
        Some(s) => CONTINUATION_AGENT_STATUSES.contains(&s),
        None => false,
    }
}

/// Pure decision: should this run be continued?
///
/// Mirrors Node `decideRunLivenessContinuation` 1:1 with simplification of DB-bound fields.
pub fn decide_run_liveness_continuation(input: &ContinuationInput<'_>) -> RunContinuationDecision {
    if !is_actionable_liveness_state(input.liveness_state) {
        return RunContinuationDecision::Skip {
            reason: "liveness state is not actionable for continuation".to_string(),
        };
    }
    let Some(issue_status) = input.issue_status else {
        return RunContinuationDecision::Skip {
            reason: "issue not found".to_string(),
        };
    };
    let Some(agent_status) = input.agent_status else {
        return RunContinuationDecision::Skip {
            reason: "agent not found".to_string(),
        };
    };
    if input.issue_assignee_agent_id != Some(input.run_agent_id) {
        return RunContinuationDecision::Skip {
            reason: "issue is no longer assigned to the source run agent".to_string(),
        };
    }
    if !is_continuation_issue_status(Some(issue_status)) {
        return RunContinuationDecision::Skip {
            reason: format!("issue status {issue_status} is not continuable"),
        };
    }
    if input.issue_execution_state.is_some() {
        return RunContinuationDecision::Skip {
            reason: "issue is blocked by execution policy state".to_string(),
        };
    }
    if !is_continuation_agent_status(Some(agent_status)) {
        return RunContinuationDecision::Skip {
            reason: format!("agent status {agent_status} is not invokable"),
        };
    }
    if input.budget_blocked {
        return RunContinuationDecision::Skip {
            reason: "budget hard stop blocks continuation".to_string(),
        };
    }

    let max_attempts = input
        .max_attempts
        .unwrap_or(DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS);
    let current_attempt = read_continuation_attempt(input.current_attempt);

    if current_attempt >= max_attempts {
        return RunContinuationDecision::Exhausted {
            attempt: current_attempt,
            max_attempts,
            comment: format!(
                "Bounded liveness continuation exhausted\n\n\
                 - Last liveness state: `{}`\n\
                 - Attempts used: {current_attempt}/{max_attempts}\n\
                 - Next action: a human or manager should inspect the run and either \
                 clarify the task, mark it blocked, or assign a concrete follow-up.",
                input.liveness_state.unwrap_or("unknown"),
            ),
        };
    }

    let next_attempt = current_attempt + 1;
    let liveness_state = input.liveness_state.unwrap_or("unknown");
    let idempotency_key = build_run_liveness_continuation_idempotency_key(
        input.issue_assignee_agent_id.unwrap_or(""),
        input.run_agent_id,
        liveness_state,
        next_attempt,
    );

    if input.idempotent_wake_exists {
        return RunContinuationDecision::Skip {
            reason: "continuation wake already exists for this source run and attempt".to_string(),
        };
    }

    RunContinuationDecision::Enqueue {
        next_attempt,
        idempotency_key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_input() -> ContinuationInput<'static> {
        ContinuationInput {
            current_attempt: Some(0),
            issue_status: Some("todo"),
            issue_execution_state: None,
            issue_assignee_agent_id: Some("agent-1"),
            run_agent_id: "agent-1",
            agent_status: Some("active"),
            liveness_state: Some("plan_only"),
            budget_blocked: false,
            idempotent_wake_exists: false,
            max_attempts: None,
        }
    }

    #[test]
    fn read_attempt_accepts_positive() {
        assert_eq!(read_continuation_attempt(Some(0)), 0);
        assert_eq!(read_continuation_attempt(Some(1)), 1);
        assert_eq!(read_continuation_attempt(Some(2)), 2);
        assert_eq!(read_continuation_attempt(None), 0);
    }

    #[test]
    fn idempotency_key_format() {
        let key = build_run_liveness_continuation_idempotency_key(
            "issue-1",
            "run-1",
            "plan_only",
            1,
        );
        assert_eq!(key, "run_liveness_continuation:issue-1:run-1:plan_only:1");
    }

    #[test]
    fn actionable_state_only_plan_only_empty_response() {
        assert!(is_actionable_liveness_state(Some("plan_only")));
        assert!(is_actionable_liveness_state(Some("empty_response")));
        assert!(!is_actionable_liveness_state(Some("executed")));
        assert!(!is_actionable_liveness_state(None));
    }

    #[test]
    fn issue_status_continuable() {
        assert!(is_continuation_issue_status(Some("todo")));
        assert!(is_continuation_issue_status(Some("in_progress")));
        assert!(!is_continuation_issue_status(Some("done")));
        assert!(!is_continuation_issue_status(None));
    }

    #[test]
    fn agent_status_invokable() {
        assert!(is_continuation_agent_status(Some("active")));
        assert!(is_continuation_agent_status(Some("idle")));
        assert!(is_continuation_agent_status(Some("running")));
        assert!(is_continuation_agent_status(Some("error")));
        assert!(!is_continuation_agent_status(Some("disabled")));
        assert!(!is_continuation_agent_status(None));
    }

    #[test]
    fn decide_happy_path_enqueues() {
        let input = default_input();
        let decision = decide_run_liveness_continuation(&input);
        match decision {
            RunContinuationDecision::Enqueue { next_attempt, .. } => assert_eq!(next_attempt, 1),
            other => panic!("expected Enqueue, got {other:?}"),
        }
    }

    #[test]
    fn decide_skips_non_actionable_liveness() {
        let mut input = default_input();
        input.liveness_state = Some("executed");
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("not actionable")
        ));
    }

    #[test]
    fn decide_skips_unassigned_issue() {
        let mut input = default_input();
        input.issue_assignee_agent_id = Some("agent-other");
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("no longer assigned")
        ));
    }

    #[test]
    fn decide_skips_terminal_issue() {
        let mut input = default_input();
        input.issue_status = Some("done");
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("not continuable")
        ));
    }

    #[test]
    fn decide_skips_blocked_issue() {
        let mut input = default_input();
        input.issue_execution_state = Some("paused");
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("execution policy")
        ));
    }

    #[test]
    fn decide_skips_disabled_agent() {
        let mut input = default_input();
        input.agent_status = Some("disabled");
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("not invokable")
        ));
    }

    #[test]
    fn decide_skips_budget_blocked() {
        let mut input = default_input();
        input.budget_blocked = true;
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("budget")
        ));
    }

    #[test]
    fn decide_exhausts_when_attempts_reach_max() {
        let mut input = default_input();
        input.current_attempt = Some(2);
        input.max_attempts = Some(2);
        let decision = decide_run_liveness_continuation(&input);
        match decision {
            RunContinuationDecision::Exhausted { attempt, max_attempts, .. } => {
                assert_eq!(attempt, 2);
                assert_eq!(max_attempts, 2);
            }
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }

    #[test]
    fn decide_skips_when_idempotent_wake_exists() {
        let mut input = default_input();
        input.idempotent_wake_exists = true;
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("already exists")
        ));
    }

    #[test]
    fn decide_increments_attempt() {
        let mut input = default_input();
        input.current_attempt = Some(1);
        let decision = decide_run_liveness_continuation(&input);
        match decision {
            RunContinuationDecision::Enqueue { next_attempt, .. } => assert_eq!(next_attempt, 2),
            other => panic!("expected Enqueue, got {other:?}"),
        }
    }

    #[test]
    fn decide_skips_when_issue_missing() {
        let mut input = default_input();
        input.issue_status = None;
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("issue not found")
        ));
    }

    #[test]
    fn decide_skips_when_agent_missing() {
        let mut input = default_input();
        input.agent_status = None;
        let decision = decide_run_liveness_continuation(&input);
        assert!(matches!(
            decision,
            RunContinuationDecision::Skip { ref reason } if reason.contains("agent not found")
        ));
    }
}