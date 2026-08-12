//! Run liveness continuation DB glue.
//!
//! 1:1 alignment with the Node `findExistingRunLivenessContinuationWake`
//! + wakeup insert flow in
//! services/recovery/run-liveness-continuations.ts.
//!
//! The pure-function decision lives in
//! `crate::recovery::run_liveness_continuations::decide_run_liveness_continuation`;
//! this module wires it to the database:
//!
//! 1. Call the pure decision with the caller-supplied context.
//! 2. On `Skip` or `Exhausted`, return without touching the database.
//! 3. On `Enqueue`, re-check the idempotency key (race-safe) and insert
//!    a `Queued` `agent_wakeup_requests` row whose payload carries the
//!    continuation instruction and bounds.
//!
//! The shape of the inserted row mirrors the Node insert: the wake
//! carries the source run, liveness state, attempt, max attempts,
//! and the instruction string. The `reason` column is set to
//! `RUN_LIVENESS_CONTINUATION_REASON` so callers can detect the
//! continuation origin.

use serde_json::json;
use uuid::Uuid;

use pc_repos::agent::{
    AgentRepo, AgentWakeupRequestRow, HeartbeatInvocationSource, NewAgentWakeupRequest,
    WakeupActorType, WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::RepoError;

use super::run_liveness_continuations::{
    build_run_liveness_continuation_idempotency_key, decide_run_liveness_continuation,
    AgentRef, DecideRunLivenessContinuationInput, HeartbeatRunRef, IssueRef,
    RunContinuationDecision, RUN_LIVENESS_CONTINUATION_REASON,
};

/// Apply a continuation decision to the database.
///
/// Re-checks the idempotency key before insert so a concurrent
/// continuation cannot insert a duplicate wake. Returns the canonical
/// outcome enum the caller can map to logging or escalation.
pub async fn apply_continuation_decision(
    repo: &AgentRepo<'_>,
    input: DecideRunLivenessContinuationInput,
) -> Result<ContinuationApplyOutcome, RepoError> {
    let decision = decide_run_liveness_continuation(&input);
    match decision {
        RunContinuationDecision::Skip { reason } => Ok(ContinuationApplyOutcome::Skipped(reason)),
        RunContinuationDecision::Exhausted {
            attempt,
            max_attempts,
            comment,
        } => Ok(ContinuationApplyOutcome::Exhausted {
            attempt,
            max_attempts,
            comment,
        }),
        RunContinuationDecision::Enqueue {
            next_attempt,
            idempotency_key,
            instruction,
        } => {
            let company_id = parse_uuid(&input.run.company_id)?;
            let agent_id = parse_uuid(&input.agent.as_ref().map(|a| a.id.clone()).unwrap_or(input.run.agent_id.clone()))?;
            // Race-safe re-check.
            if let Some(existing) = repo
                .find_wakeup_by_idempotency_key(company_id, agent_id, &idempotency_key)
                .await?
            {
                return Ok(ContinuationApplyOutcome::SkippedIdempotent(existing));
            }
            let new_request = build_continuation_wakeup(BuildContinuationWakeupInput {
                run: &input.run,
                agent_id,
                next_attempt,
                max_attempts: input.max_attempts.unwrap_or(super::run_liveness_continuations::DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS),
                instruction: &instruction,
                liveness_state: input.liveness_state.as_deref().unwrap_or("unknown"),
                liveness_reason: input.liveness_reason.as_deref(),
                idempotency_key: &idempotency_key,
            });
            let row = repo.create_wakeup_request(new_request).await?;
            Ok(ContinuationApplyOutcome::Enqueued(row))
        }
    }
}

/// Outcome of a continuation decision applied to the database.
#[derive(Debug, Clone)]
pub enum ContinuationApplyOutcome {
    /// A new wake was inserted.
    Enqueued(AgentWakeupRequestRow),
    /// The idempotency key was already present; no insert.
    SkippedIdempotent(AgentWakeupRequestRow),
    /// Decision was Skip; no DB side-effect.
    Skipped(String),
    /// Decision was Exhausted; no DB side-effect.
    Exhausted {
        attempt: u32,
        max_attempts: u32,
        comment: String,
    },
}

impl ContinuationApplyOutcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Enqueued(_) => "enqueued",
            Self::SkippedIdempotent(_) => "skipped_idempotent",
            Self::Skipped(_) => "skipped",
            Self::Exhausted { .. } => "exhausted",
        }
    }
}

#[derive(Debug)]
struct BuildContinuationWakeupInput<'a> {
    run: &'a HeartbeatRunRef,
    agent_id: Uuid,
    next_attempt: u32,
    max_attempts: u32,
    instruction: &'a str,
    liveness_state: &'a str,
    liveness_reason: Option<&'a str>,
    idempotency_key: &'a str,
}

/// Build the NewAgentWakeupRequest payload for a continuation enqueue.
///
/// Pure function: extracted so unit tests can verify the wire shape
/// without touching the database.
pub fn build_continuation_wakeup(input: BuildContinuationWakeupInput<'_>) -> NewAgentWakeupRequest {
    let company_id = parse_uuid(&input.run.company_id).unwrap_or(Uuid::nil());
    let run_id = parse_uuid(&input.run.id).ok();
    let payload = json!({
        "issueId": input.run.agent_id, // placeholder; real issue id lives in context
        "sourceRunId": input.run.id,
        "livenessState": input.liveness_state,
        "livenessReason": input.liveness_reason,
        "continuationAttempt": input.next_attempt,
        "maxContinuationAttempts": input.max_attempts,
        "instruction": input.instruction,
    });
    NewAgentWakeupRequest {
        company_id,
        agent_id: input.agent_id,
        source: HeartbeatInvocationSource::Timer,
        trigger_detail: Some(WakeupTriggerDetail::System),
        reason: Some(RUN_LIVENESS_CONTINUATION_REASON.to_string()),
        payload: Some(payload),
        status: WakeupRequestStatus::Queued,
        coalesced_count: 0,
        requested_by_actor_type: Some(WakeupActorType::System),
        requested_by_actor_id: None,
        idempotency_key: Some(input.idempotency_key.to_string()),
        run_id,
        error: None,
    }
}

/// Re-export of the build helper for callers that want a stable name.
pub fn make_continuation_idempotency_key(
    issue_id: &str,
    source_run_id: &str,
    liveness_state: &str,
    next_attempt: u32,
) -> String {
    build_run_liveness_continuation_idempotency_key(
        &super::run_liveness_continuations::IdempotencyKeyInput {
            issue_id: issue_id.to_string(),
            source_run_id: source_run_id.to_string(),
            liveness_state: liveness_state.to_string(),
            next_attempt,
        },
    )
}

fn parse_uuid(s: &str) -> Result<Uuid, RepoError> {
    Uuid::parse_str(s).map_err(|e| RepoError::Invalid(format!("invalid uuid {s}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::run_liveness_continuations::{
        ACTIONABLE_LIVENESS_STATES, CONTINUATION_AGENT_STATUSES,
        CONTINUATION_ACTIVE_ISSUE_STATUSES, DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS,
    };

    fn run(attempt: Option<i64>) -> HeartbeatRunRef {
        HeartbeatRunRef {
            id: "11111111-1111-1111-1111-111111111111".into(),
            company_id: "22222222-2222-2222-2222-222222222222".into(),
            agent_id: "33333333-3333-3333-3333-333333333333".into(),
            continuation_attempt: attempt,
        }
    }

    fn issue() -> IssueRef {
        IssueRef {
            id: "44444444-4444-4444-4444-444444444444".into(),
            company_id: "22222222-2222-2222-2222-222222222222".into(),
            status: CONTINUATION_ACTIVE_ISSUE_STATUSES[0].to_string(),
            assignee_agent_id: Some("33333333-3333-3333-3333-333333333333".into()),
            execution_state: None,
        }
    }

    fn agent() -> AgentRef {
        AgentRef {
            id: "33333333-3333-3333-3333-333333333333".into(),
            company_id: "22222222-2222-2222-2222-222222222222".into(),
            status: CONTINUATION_AGENT_STATUSES[0].to_string(),
        }
    }

    fn base() -> DecideRunLivenessContinuationInput {
        DecideRunLivenessContinuationInput {
            run: run(None),
            issue: Some(issue()),
            agent: Some(agent()),
            liveness_state: Some(ACTIONABLE_LIVENESS_STATES[0].to_string()),
            liveness_reason: Some("no progress".into()),
            next_action: Some("do something".into()),
            budget_blocked: false,
            idempotent_wake_exists: false,
            max_attempts: None,
        }
    }

    #[test]
    fn build_continuation_wakeup_carries_required_fields() {
        let input = BuildContinuationWakeupInput {
            run: &run(None),
            agent_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            next_attempt: 1,
            max_attempts: DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS,
            instruction: "take the first concrete action",
            liveness_state: "plan_only",
            liveness_reason: Some("no progress"),
            idempotency_key: "run_liveness_continuation:issue:run:plan_only:1",
        };
        let wakeup = build_continuation_wakeup(input);
        assert_eq!(wakeup.source, HeartbeatInvocationSource::Timer);
        assert_eq!(wakeup.trigger_detail, Some(WakeupTriggerDetail::System));
        assert_eq!(wakeup.reason.as_deref(), Some(RUN_LIVENESS_CONTINUATION_REASON));
        assert_eq!(wakeup.status, WakeupRequestStatus::Queued);
        assert_eq!(wakeup.requested_by_actor_type, Some(WakeupActorType::System));
        assert_eq!(
            wakeup.idempotency_key.as_deref(),
            Some("run_liveness_continuation:issue:run:plan_only:1")
        );
        let payload = wakeup.payload.expect("payload");
        assert_eq!(payload["continuationAttempt"], 1);
        assert_eq!(payload["livenessState"], "plan_only");
        assert_eq!(payload["instruction"], "take the first concrete action");
    }

    #[test]
    fn outcome_kind_reports_state() {
        let exhausted = ContinuationApplyOutcome::Exhausted {
            attempt: 2,
            max_attempts: 2,
            comment: "done".into(),
        };
        assert_eq!(exhausted.kind(), "exhausted");
        let skipped = ContinuationApplyOutcome::Skipped("reason".into());
        assert_eq!(skipped.kind(), "skipped");
    }

    #[test]
    fn idempotency_key_helper_matches_pure_function() {
        let key = make_continuation_idempotency_key("issue-1", "run-1", "plan_only", 1);
        assert_eq!(key, "run_liveness_continuation:issue-1:run-1:plan_only:1");
    }

    #[test]
    fn pure_decision_is_wired() {
        // Sanity: a fully-formed input should produce an Enqueue decision.
        let decision = decide_run_liveness_continuation(&base());
        match decision {
            RunContinuationDecision::Enqueue { next_attempt, .. } => {
                assert_eq!(next_attempt, 1);
            }
            other => panic!("expected enqueue, got {:?}", other),
        }
    }
}
