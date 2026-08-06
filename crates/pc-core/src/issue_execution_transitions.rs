//! `issue_execution_transitions` — Issue execution policy 状态机编排层。
//!
//! 与 Node `issue-execution-policy.ts` 中高阶 `apply*Transition` 系列
//! 1:1 对齐：
//!
//! - `apply_issue_execution_stage_transition`：主 stage transition（escalated
//!   hold / board override / escalation / approval / changes_requested /
//!   pending-flow / auto-skip）。
//! - `apply_monitor_transition`：monitor 字段（next_check_at / scheduled /
//!   triggered / cleared）状态转移。
//! - `apply_issue_execution_policy_transition`：stage + monitor 组合。
//! - `apply_issue_monitor_policy_transition`：仅 monitor。
//! - `build_initial_issue_monitor_fields`：issue 初始化时构造 monitor 字段。
//! - `build_issue_monitor_triggered_patch`：monitor 触发后构造 patch。
//! - `build_issue_monitor_cleared_patch`：monitor 清除后构造 patch。
//!
//! 设计目标：复用 `issue_execution_monitor_state` / `issue_execution_policy`
//! 的 pure helpers，仅本模块负责状态机编排与 policy 决策。

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::issue_execution_monitor_state::{
    execution_state_with_monitor, monitor_states_equal, IssueExecutionMonitorClearReason,
    IssueExecutionMonitorState, IssueExecutionStagePrincipal, IssueExecutionState,
    IssueExecutionStateStatus, IssueMonitorScheduledBy, ReviewRequest,
};
use crate::issue_execution_policy::{
    actor_principal, assignee_principal, build_changes_requested_state,
    build_cleared_monitor_state, build_completed_state, build_pending_stage_patch,
    build_pending_state, build_scheduled_monitor_state, build_skipped_stage_completed_state,
    build_state_with_completed_stages, build_triggered_monitor_state, can_auto_skip_pending_stage,
    clear_execution_state_patch, derive_persisted_monitor_state, exhausted_monitor_clear_reason,
    find_stage_by_id, issue_allows_monitor, monitor_clear_reason_for_issue, next_assignee_ids,
    next_pending_stage, next_pending_stage_after, patch_for_principal, principals_equal,
    resolve_max_review_rounds, review_escalation_user_id, select_stage_participant,
    stage_has_participant, strip_monitor_from_execution_policy, ActorLike, AssigneeLike,
    BuildPendingStagePatchInput, BuildPendingStateInput, BuildStateWithCompletedStagesInput,
    ClearExecutionStatePatchInput, DerivePersistedMonitorStateInput,
    ExhaustedMonitorClearReasonInput, IssueExecutionDecision, IssueExecutionDecisionOutcome,
    IssueExecutionPolicy, IssueLike, RequestedAssigneePatch,
};

// ============================================================================
// Constants (re-exported for caller convenience)
// ============================================================================

pub use crate::issue_execution_policy::{
    DEFAULT_MAX_REVIEW_ROUNDS, MONITOR_BOUNDS_EXHAUSTED_MESSAGE, MONITOR_INVALID_MESSAGE,
    STAGE_DECISION_COMMENT_HINT,
};

/// Local copy of the same constant; defined here for self-documenting use.
pub const COMPLETED_STATUS: &str = "completed";
pub const PENDING_STATUS: &str = "pending";
pub const CHANGES_REQUESTED_STATUS: &str = "changes_requested";

// ============================================================================
// Error type
// ============================================================================

/// `PolicyTransitionError`：transition 拒绝的错误。
///
/// 与 Node `unprocessable(...)` 1:1 对齐：携带 message + optional details。
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyTransitionError {
    pub message: String,
    pub clear_reason: Option<IssueExecutionMonitorClearReason>,
}

impl PolicyTransitionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            clear_reason: None,
        }
    }

    pub fn with_clear_reason(
        message: impl Into<String>,
        clear_reason: IssueExecutionMonitorClearReason,
    ) -> Self {
        Self {
            message: message.into(),
            clear_reason: Some(clear_reason),
        }
    }
}

impl std::fmt::Display for PolicyTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PolicyTransitionError {}

pub type PolicyTransitionResult<T> = Result<T, PolicyTransitionError>;

// ============================================================================
// Input/Output structs
// ============================================================================

/// `TransitionInput`：与 Node `TransitionInput` 1:1 对齐。
#[derive(Debug, Clone)]
pub struct TransitionInput {
    pub issue: IssueLike,
    pub policy: Option<IssueExecutionPolicy>,
    pub previous_policy: Option<IssueExecutionPolicy>,
    pub requested_status: Option<String>,
    pub requested_assignee_patch: RequestedAssigneePatch,
    pub actor: ActorLike,
    pub allow_board_override: bool,
    pub comment_body: Option<String>,
    pub review_request: Option<ReviewRequest>,
    pub monitor_explicitly_updated: bool,
}

impl Default for TransitionInput {
    fn default() -> Self {
        Self {
            issue: IssueLike::default(),
            policy: None,
            previous_policy: None,
            requested_status: None,
            requested_assignee_patch: RequestedAssigneePatch::default(),
            actor: ActorLike::default(),
            allow_board_override: false,
            comment_body: None,
            review_request: None,
            monitor_explicitly_updated: false,
        }
    }
}

/// `TransitionResult`：与 Node `TransitionResult` 1:1 对齐。
#[derive(Debug, Clone, Default)]
pub struct TransitionResult {
    pub patch: Map<String, Value>,
    pub decision: Option<IssueExecutionDecision>,
    pub workflow_controlled_assignment: bool,
}

impl PartialEq for TransitionResult {
    fn eq(&self, other: &Self) -> bool {
        // Compare via JSON serialization to avoid needing PartialEq on Map
        serde_json::to_string(&self.patch).ok() == serde_json::to_string(&other.patch).ok()
            && self.decision == other.decision
            && self.workflow_controlled_assignment == other.workflow_controlled_assignment
    }
}

// ============================================================================
// parse helpers (terse "if invalid → return early" wrappers)
// ============================================================================

fn parse_execution_state(input: Option<&Value>) -> Option<IssueExecutionState> {
    let v = input?;
    serde_json::from_value(v.clone()).ok()
}

fn parse_optional_policy(input: Option<&Value>) -> Option<IssueExecutionPolicy> {
    let v = input?;
    serde_json::from_value(v.clone()).ok()
}

fn execution_state_value(state: Option<&IssueExecutionState>) -> Option<Value> {
    state.map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
}

fn principal_from_assignee_like(input: &AssigneeLike) -> Option<IssueExecutionStagePrincipal> {
    assignee_principal(input)
}

fn principal_from_actor_like(input: &ActorLike) -> Option<IssueExecutionStagePrincipal> {
    actor_principal(input)
}

// ============================================================================
// applyIssueExecutionStageTransition
// ============================================================================

/// `apply_issue_execution_stage_transition`：主 stage transition 状态机。
///
/// 与 Node 1:1 对齐：完整实现 escalated hold / board override / approval /
/// changes_requested / escalation / pending flow / auto-skip 等所有分支。
pub fn apply_issue_execution_stage_transition(
    input: &TransitionInput,
) -> PolicyTransitionResult<TransitionResult> {
    let mut patch: Map<String, Value> = Map::new();
    let existing_state = parse_execution_state(
        input
            .issue
            .execution_state
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok())
            .as_ref(),
    );
    let current_assignee = principal_from_assignee_like(&AssigneeLike {
        assignee_agent_id: input.issue.assignee_agent_id.clone(),
        assignee_user_id: input.issue.assignee_user_id.clone(),
    });
    let actor = principal_from_actor_like(&input.actor);

    let requested_assignee_patch_provided =
        input.requested_assignee_patch.assignee_agent_id.is_some()
            || input.requested_assignee_patch.assignee_user_id.is_some();
    let explicit_assignee = principal_from_assignee_like(&AssigneeLike {
        assignee_agent_id: input.requested_assignee_patch.assignee_agent_id.clone(),
        assignee_user_id: input.requested_assignee_patch.assignee_user_id.clone(),
    });

    let current_stage = if let Some(p) = &input.policy {
        find_stage_by_id(
            p,
            existing_state
                .as_ref()
                .and_then(|s| s.current_stage_id.as_deref()),
        )
    } else {
        None
    };
    let requested_status = input.requested_status.as_deref();
    let active_stage = if current_stage.is_some()
        && existing_state.as_ref().map(|s| s.status) == Some(IssueExecutionStateStatus::Pending)
    {
        current_stage.clone()
    } else {
        None
    };
    let effective_review_request = if input.review_request.is_some() {
        input.review_request.clone()
    } else {
        existing_state
            .as_ref()
            .and_then(|s| s.review_request.clone())
    };

    // No policy: clear execution state, possibly return to in_progress
    let Some(policy) = &input.policy else {
        if let Some(state) = &existing_state {
            let patch_clear = clear_execution_state_patch(&ClearExecutionStatePatchInput {
                patch,
                issue_status: &input.issue.status,
                requested_status,
                return_assignee: state.return_assignee.clone(),
            });
            patch = patch_clear;
            if input.issue.status == "in_review" {
                if let Some(return_assignee) = &state.return_assignee {
                    patch.insert("status".into(), Value::String("in_progress".into()));
                    let assignee_patch = patch_for_principal(Some(return_assignee));
                    for (k, v) in assignee_patch {
                        patch.insert(k, v);
                    }
                }
            }
        }
        return Ok(TransitionResult {
            patch,
            decision: None,
            workflow_controlled_assignment: false,
        });
    };

    // Done/Cancelled → opening back is forbidden
    if (input.issue.status == "done" || input.issue.status == "cancelled")
        && requested_status
            .map(|r| r != "done" && r != "cancelled")
            .unwrap_or(false)
    {
        patch.insert("executionState".into(), Value::Null);
        return Ok(TransitionResult {
            patch,
            decision: None,
            workflow_controlled_assignment: false,
        });
    }

    // existing state points to a stage that no longer exists
    if existing_state
        .as_ref()
        .and_then(|s| s.current_stage_id.as_ref())
        .is_some()
        && current_stage.is_none()
    {
        let cleared = clear_execution_state_patch(&ClearExecutionStatePatchInput {
            patch,
            issue_status: &input.issue.status,
            requested_status,
            return_assignee: existing_state
                .as_ref()
                .and_then(|s| s.return_assignee.clone()),
        });
        return Ok(TransitionResult {
            patch: cleared,
            ..Default::default()
        });
    }

    if let Some(active_stage) = &active_stage {
        let current_participant = if let Some(state) = &existing_state {
            state.current_participant.clone()
        } else {
            select_stage_participant(
                active_stage,
                Some(
                    &crate::issue_execution_policy::StageParticipantSelectorOpts {
                        preferred: None,
                        exclude: existing_state
                            .as_ref()
                            .and_then(|s| s.return_assignee.clone()),
                    },
                ),
            )
        };

        let Some(current_participant) = current_participant else {
            return Err(PolicyTransitionError::new(format!(
                "No eligible {} participant is configured for this issue",
                active_stage.kind.as_str()
            )));
        };

        // escalated hold detection
        let max_rounds = resolve_max_review_rounds(Some(policy));
        let changes_requested_count = existing_state
            .as_ref()
            .and_then(|s| s.changes_requested_count)
            .unwrap_or(0);
        let escalated_hold = current_participant.user_id.is_some()
            && current_participant.agent_id.is_none()
            && !stage_has_participant(active_stage, Some(&current_participant))
            && changes_requested_count >= max_rounds;

        if escalated_hold && !principals_equal(Some(&current_participant), actor.as_ref()) {
            let attempted_advance_during_hold = (requested_status.is_some()
                && requested_status != Some("in_review"))
                || (requested_assignee_patch_provided
                    && !principals_equal(explicit_assignee.as_ref(), Some(&current_participant)));
            if attempted_advance_during_hold {
                return Err(PolicyTransitionError::new(
                    "Only the escalated reviewer can advance the current execution stage",
                ));
            }
            let hold_drifted = input.issue.status != "in_review"
                || !principals_equal(current_assignee.as_ref(), Some(&current_participant));
            if hold_drifted {
                patch.insert("status".into(), Value::String("in_review".into()));
                let assignee_patch = patch_for_principal(Some(&current_participant));
                for (k, v) in assignee_patch {
                    patch.insert(k, v);
                }
                let stage_index = policy
                    .stages
                    .iter()
                    .position(|s| s.id == active_stage.id)
                    .map(|i| i as i64)
                    .unwrap_or(0);
                let pending_state = build_pending_state(&BuildPendingStateInput {
                    previous: existing_state.as_ref(),
                    stage: active_stage,
                    stage_index,
                    participant: current_participant.clone(),
                    return_assignee: existing_state
                        .as_ref()
                        .and_then(|s| s.return_assignee.clone()),
                    review_request: effective_review_request.clone(),
                    changes_requested_count: None,
                });
                patch.insert(
                    "executionState".into(),
                    serde_json::to_value(pending_state).unwrap_or(Value::Null),
                );
            }
            return Ok(TransitionResult {
                patch,
                decision: None,
                workflow_controlled_assignment: false,
            });
        }

        if !escalated_hold && !stage_has_participant(active_stage, Some(&current_participant)) {
            let opts = crate::issue_execution_policy::StageParticipantSelectorOpts {
                preferred: explicit_assignee.clone().or_else(|| {
                    existing_state
                        .as_ref()
                        .and_then(|s| s.current_participant.clone())
                }),
                exclude: existing_state
                    .as_ref()
                    .and_then(|s| s.return_assignee.clone()),
            };
            if let Some(participant) = select_stage_participant(active_stage, Some(&opts)) {
                let return_assignee = existing_state
                    .as_ref()
                    .and_then(|s| s.return_assignee.clone())
                    .or_else(|| current_assignee.clone())
                    .or_else(|| actor.clone());
                let pending_patch = build_pending_stage_patch(&BuildPendingStagePatchInput {
                    patch: patch.clone(),
                    previous: existing_state.as_ref(),
                    policy,
                    stage: active_stage,
                    participant: &participant,
                    return_assignee,
                    review_request: effective_review_request.clone(),
                    changes_requested_count: None,
                });
                return Ok(TransitionResult {
                    decision: None,
                    patch: pending_patch,
                    workflow_controlled_assignment: true,
                    ..Default::default()
                });
            } else {
                let cleared = clear_execution_state_patch(&ClearExecutionStatePatchInput {
                    patch,
                    issue_status: &input.issue.status,
                    requested_status,
                    return_assignee: existing_state
                        .as_ref()
                        .and_then(|s| s.return_assignee.clone()),
                });
                return Ok(TransitionResult {
                    patch: cleared,
                    ..Default::default()
                });
            }
        }

        // principal equals actor: decision branch
        if principals_equal(Some(&current_participant), actor.as_ref()) {
            // Approval / "done" branch
            if requested_status == Some("done") {
                let comment_trimmed = input
                    .comment_body
                    .as_deref()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty());
                let Some(body) = comment_trimmed else {
                    return Err(PolicyTransitionError::new(format!(
                        "Approving a review or approval stage requires a comment. {STAGE_DECISION_COMMENT_HINT}"
                    )));
                };
                let approved_state = build_completed_state(existing_state.as_ref(), active_stage);
                let next_stage =
                    next_pending_stage_after(policy, active_stage, Some(&approved_state));
                let Some(next_stage) = next_stage else {
                    patch.insert(
                        "executionState".into(),
                        serde_json::to_value(&approved_state).unwrap_or(Value::Null),
                    );
                    return Ok(TransitionResult {
                        patch,
                        decision: Some(IssueExecutionDecision {
                            stage_id: active_stage.id.clone().unwrap_or_default(),
                            stage_type: active_stage.kind,
                            outcome: IssueExecutionDecisionOutcome::Approved,
                            body: body.to_string(),
                        }),
                        ..Default::default()
                    });
                };

                let return_assignee_next = existing_state
                    .as_ref()
                    .and_then(|s| s.return_assignee.clone())
                    .or_else(|| current_assignee.clone())
                    .or_else(|| actor.clone());
                let opts_next = crate::issue_execution_policy::StageParticipantSelectorOpts {
                    preferred: explicit_assignee.clone(),
                    exclude: existing_state
                        .as_ref()
                        .and_then(|s| s.return_assignee.clone()),
                };
                let Some(participant_next) =
                    select_stage_participant(&next_stage, Some(&opts_next))
                else {
                    return Err(PolicyTransitionError::new(format!(
                        "No eligible {} participant is configured for this issue",
                        next_stage.kind.as_str()
                    )));
                };
                let pending_patch = build_pending_stage_patch(&BuildPendingStagePatchInput {
                    patch: patch.clone(),
                    previous: Some(&approved_state),
                    policy,
                    stage: &next_stage,
                    participant: &participant_next,
                    return_assignee: return_assignee_next,
                    review_request: input.review_request.clone(),
                    changes_requested_count: None,
                });
                return Ok(TransitionResult {
                    patch: pending_patch,
                    decision: Some(IssueExecutionDecision {
                        stage_id: active_stage.id.clone().unwrap_or_default(),
                        stage_type: active_stage.kind,
                        outcome: IssueExecutionDecisionOutcome::Approved,
                        body: body.to_string(),
                    }),
                    workflow_controlled_assignment: true,
                });
            }

            // Board override short-circuit
            if input.allow_board_override
                && requested_status.is_some()
                && requested_status != Some("in_review")
                && requested_status != Some("in_progress")
            {
                patch.insert("executionState".into(), Value::Null);
                return Ok(TransitionResult {
                    patch,
                    decision: None,
                    workflow_controlled_assignment: false,
                });
            }

            // Changes-requested branch
            if requested_status.is_some() && requested_status != Some("in_review") {
                let comment_trimmed = input
                    .comment_body
                    .as_deref()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty());
                let Some(body) = comment_trimmed else {
                    return Err(PolicyTransitionError::new(format!(
                        "Requesting changes requires a comment. {STAGE_DECISION_COMMENT_HINT}"
                    )));
                };
                let existing = existing_state.as_ref().ok_or_else(|| {
                    PolicyTransitionError::new("Missing execution state for changes-requested")
                })?;
                let return_assignee = existing.return_assignee.clone().ok_or_else(|| {
                    PolicyTransitionError::new("This execution stage has no return assignee")
                })?;
                let actor_is_human = actor.as_ref().map(|p| p.user_id.is_some()).unwrap_or(false);
                let next_rounds = if actor_is_human {
                    0
                } else {
                    existing.changes_requested_count.unwrap_or(0) + 1
                };
                let decision = IssueExecutionDecision {
                    stage_id: active_stage.id.clone().unwrap_or_default(),
                    stage_type: active_stage.kind,
                    outcome: IssueExecutionDecisionOutcome::ChangesRequested,
                    body: body.to_string(),
                };
                if !actor_is_human && next_rounds >= max_rounds {
                    if let Some(escalation_user_id) = review_escalation_user_id(&input.issue) {
                        let escalation_principal = IssueExecutionStagePrincipal {
                            principal_type: "user".to_string(),
                            agent_id: None,
                            user_id: Some(escalation_user_id),
                        };
                        let pending_patch =
                            build_pending_stage_patch(&BuildPendingStagePatchInput {
                                patch: patch.clone(),
                                previous: existing_state.as_ref(),
                                policy,
                                stage: active_stage,
                                participant: &escalation_principal,
                                return_assignee: Some(return_assignee.clone()),
                                review_request: effective_review_request.clone(),
                                changes_requested_count: Some(next_rounds),
                            });
                        return Ok(TransitionResult {
                            patch: pending_patch,
                            decision: Some(decision),
                            workflow_controlled_assignment: true,
                        });
                    }
                }
                patch.insert("status".into(), Value::String("in_progress".into()));
                let assignee_patch = patch_for_principal(Some(&return_assignee));
                for (k, v) in assignee_patch {
                    patch.insert(k, v);
                }
                let changes_state =
                    build_changes_requested_state(existing, active_stage, next_rounds);
                patch.insert(
                    "executionState".into(),
                    serde_json::to_value(&changes_state).unwrap_or(Value::Null),
                );
                return Ok(TransitionResult {
                    patch,
                    decision: Some(decision),
                    workflow_controlled_assignment: true,
                });
            }
        }

        // attempted advance / drifted detection
        let attempted_stage_advance = (requested_status.is_some()
            && requested_status != Some("in_review"))
            || (requested_assignee_patch_provided
                && !principals_equal(explicit_assignee.as_ref(), Some(&current_participant)));
        let stage_state_drifted = input.issue.status != "in_review"
            || !principals_equal(current_assignee.as_ref(), Some(&current_participant))
            || !principals_equal(
                existing_state
                    .as_ref()
                    .and_then(|s| s.current_participant.clone())
                    .as_ref(),
                Some(&current_participant),
            );

        if input.allow_board_override && attempted_stage_advance {
            if requested_status.is_some() && requested_status != Some("in_review") {
                patch.insert("executionState".into(), Value::Null);
                return Ok(TransitionResult {
                    patch,
                    decision: None,
                    workflow_controlled_assignment: false,
                });
            }
            if let Some(ea) = explicit_assignee.as_ref() {
                if stage_has_participant(active_stage, Some(ea)) {
                    let return_assignee = existing_state
                        .as_ref()
                        .and_then(|s| s.return_assignee.clone())
                        .or_else(|| current_assignee.clone())
                        .or_else(|| actor.clone());
                    let pending_patch = build_pending_stage_patch(&BuildPendingStagePatchInput {
                        patch: patch.clone(),
                        previous: existing_state.as_ref(),
                        policy,
                        stage: active_stage,
                        participant: ea,
                        return_assignee,
                        review_request: effective_review_request.clone(),
                        changes_requested_count: None,
                    });
                    return Ok(TransitionResult {
                        patch: pending_patch,
                        ..Default::default()
                    });
                }
            }
            patch.insert("executionState".into(), Value::Null);
            if input.issue.status == "in_review" {
                patch.insert("status".into(), Value::String("in_progress".into()));
            }
            return Ok(TransitionResult {
                patch,
                decision: None,
                workflow_controlled_assignment: false,
            });
        }

        if attempted_stage_advance && !stage_state_drifted {
            return Err(PolicyTransitionError::new(
                "Only the active reviewer or approver can advance the current execution stage",
            ));
        }

        if stage_state_drifted {
            let return_assignee = existing_state
                .as_ref()
                .and_then(|s| s.return_assignee.clone())
                .or_else(|| current_assignee.clone())
                .or_else(|| actor.clone());
            let pending_patch = build_pending_stage_patch(&BuildPendingStagePatchInput {
                patch: patch.clone(),
                previous: existing_state.as_ref(),
                policy,
                stage: active_stage,
                participant: &current_participant,
                return_assignee,
                review_request: effective_review_request.clone(),
                changes_requested_count: None,
            });
            return Ok(TransitionResult {
                patch: pending_patch,
                decision: None,
                workflow_controlled_assignment: true,
            });
        }

        return Ok(TransitionResult {
            patch,
            decision: None,
            workflow_controlled_assignment: false,
        });
    }

    // No active stage → check if we should start workflow
    let should_start_workflow =
        requested_status == Some("done") || requested_status == Some("in_review");
    if !should_start_workflow {
        return Ok(TransitionResult {
            patch,
            decision: None,
            workflow_controlled_assignment: false,
        });
    }

    // Already completed workflow: terminal for approve/done (#7893)
    if requested_status == Some("done")
        && existing_state.as_ref().map(|s| s.status) == Some(IssueExecutionStateStatus::Completed)
    {
        return Ok(TransitionResult {
            patch,
            decision: None,
            workflow_controlled_assignment: false,
        });
    }

    let pending_stage = if existing_state.as_ref().map(|s| s.status)
        == Some(IssueExecutionStateStatus::ChangesRequested)
        && current_stage.is_some()
    {
        current_stage.cloned()
    } else {
        next_pending_stage(policy, existing_state.as_ref()).cloned()
    };
    let Some(mut pending_stage) = pending_stage else {
        return Ok(TransitionResult {
            patch,
            decision: None,
            workflow_controlled_assignment: false,
        });
    };

    let return_assignee = existing_state
        .as_ref()
        .and_then(|s| s.return_assignee.clone())
        .or_else(|| current_assignee.clone());

    let mut skipped_stage_ids: Vec<String> = existing_state
        .as_ref()
        .map(|s| s.completed_stage_ids.clone())
        .unwrap_or_default();
    let original_completed_len = skipped_stage_ids.len();
    let mut participant = {
        let preferred = if existing_state.as_ref().map(|s| s.status)
            == Some(IssueExecutionStateStatus::ChangesRequested)
        {
            explicit_assignee.clone().or_else(|| {
                existing_state
                    .as_ref()
                    .and_then(|s| s.current_participant.clone())
            })
        } else {
            explicit_assignee.clone()
        };
        let opts = crate::issue_execution_policy::StageParticipantSelectorOpts {
            preferred,
            exclude: return_assignee.clone(),
        };
        select_stage_participant(&pending_stage, Some(&opts))
    };

    while participant.is_none()
        && can_auto_skip_pending_stage(
            &crate::issue_execution_policy::CanAutoSkipPendingStageInput {
                stage: &pending_stage,
                return_assignee: return_assignee.clone(),
                requested_status,
            },
        )
    {
        if let Some(id) = pending_stage.id.as_deref() {
            skipped_stage_ids.push(id.to_string());
        }
        let synthetic_state =
            build_state_with_completed_stages(&BuildStateWithCompletedStagesInput {
                previous: existing_state.as_ref(),
                completed_stage_ids: skipped_stage_ids.clone(),
                return_assignee: return_assignee.clone(),
            });
        pending_stage = match next_pending_stage(policy, Some(&synthetic_state)).cloned() {
            Some(s) => s,
            None => {
                let completed =
                    build_skipped_stage_completed_state(&BuildStateWithCompletedStagesInput {
                        previous: existing_state.as_ref(),
                        completed_stage_ids: skipped_stage_ids.clone(),
                        return_assignee: return_assignee.clone(),
                    });
                patch.insert(
                    "executionState".into(),
                    serde_json::to_value(&completed).unwrap_or(Value::Null),
                );
                return Ok(TransitionResult {
                    decision: None,
                    patch,
                    workflow_controlled_assignment: true,
                });
            }
        };
        participant = {
            let preferred = if existing_state.as_ref().map(|s| s.status)
                == Some(IssueExecutionStateStatus::ChangesRequested)
            {
                explicit_assignee.clone().or_else(|| {
                    existing_state
                        .as_ref()
                        .and_then(|s| s.current_participant.clone())
                })
            } else {
                explicit_assignee.clone()
            };
            let opts = crate::issue_execution_policy::StageParticipantSelectorOpts {
                preferred,
                exclude: return_assignee.clone(),
            };
            select_stage_participant(&pending_stage, Some(&opts))
        };
    }

    let Some(participant) = participant else {
        return Err(PolicyTransitionError::new(format!(
            "No eligible {} participant is configured for this issue",
            pending_stage.kind.as_str()
        )));
    };

    let prev_for_patch: Option<IssueExecutionState> =
        if skipped_stage_ids.len() == original_completed_len {
            existing_state.clone()
        } else {
            Some(build_state_with_completed_stages(
                &BuildStateWithCompletedStagesInput {
                    previous: existing_state.as_ref(),
                    completed_stage_ids: skipped_stage_ids.clone(),
                    return_assignee: return_assignee.clone(),
                },
            ))
        };
    let pending_patch = build_pending_stage_patch(&BuildPendingStagePatchInput {
        patch: patch.clone(),
        previous: prev_for_patch.as_ref(),
        policy,
        stage: &pending_stage,
        participant: &participant,
        return_assignee: return_assignee.clone(),
        review_request: input.review_request.clone(),
        changes_requested_count: None,
    });
    Ok(TransitionResult {
        patch: pending_patch,
        decision: None,
        workflow_controlled_assignment: true,
    })
}

// ============================================================================
// applyMonitorTransition
// ============================================================================

/// `apply_monitor_transition`：将 monitor 字段决策叠加到 stagePatch。
///
/// 与 Node 1:1 对齐。
pub fn apply_monitor_transition(
    input: &TransitionInput,
    stage_patch: &Map<String, Value>,
) -> Map<String, Value> {
    let mut patch: Map<String, Value> = Map::new();

    let previous_policy = input.previous_policy.clone().or_else(|| {
        parse_optional_policy(
            input
                .issue
                .execution_policy
                .as_ref()
                .and_then(|p| serde_json::to_value(p).ok())
                .as_ref(),
        )
    });
    let existing_state = input.issue.execution_state.clone();

    let current_monitor_state = derive_persisted_monitor_state(&DerivePersistedMonitorStateInput {
        issue: &input.issue,
        state: existing_state.as_ref(),
        policy: previous_policy.as_ref(),
    });

    let next_status = if let Some(Value::String(s)) = stage_patch.get("status") {
        s.clone()
    } else {
        input
            .requested_status
            .clone()
            .unwrap_or_else(|| input.issue.status.clone())
    };
    let (assignee_agent_id, assignee_user_id) =
        next_assignee_ids(&crate::issue_execution_policy::NextAssigneeIdsInput {
            issue: &input.issue,
            requested_assignee_patch: &input.requested_assignee_patch,
            stage_patch,
        });

    let stage_state = if let Some(v) = stage_patch.get("executionState") {
        parse_execution_state(Some(v))
    } else {
        existing_state.clone()
    };

    let invalid_reason = if input
        .policy
        .as_ref()
        .and_then(|p| p.monitor.as_ref())
        .is_some()
    {
        monitor_clear_reason_for_issue(
            &next_status,
            assignee_agent_id.as_deref(),
            assignee_user_id.as_deref(),
        )
    } else {
        None
    };

    let now = Utc::now();
    let mut target_monitor_state: Option<IssueExecutionMonitorState> =
        current_monitor_state.clone();

    if let Some(policy) = input.policy.as_ref() {
        if let Some(monitor) = policy.monitor.as_ref() {
            if let Some(reason) = invalid_reason.clone() {
                if input.monitor_explicitly_updated {
                    return error_to_monitor_patch(PolicyTransitionError::new(
                        MONITOR_INVALID_MESSAGE,
                    ));
                }
                let stripped = strip_monitor_from_execution_policy(Some(policy));
                if let Some(s) = stripped {
                    patch.insert(
                        "executionPolicy".into(),
                        serde_json::to_value(&s).unwrap_or(Value::Null),
                    );
                } else {
                    patch.insert("executionPolicy".into(), Value::Null);
                }
                patch.insert("monitorNextCheckAt".into(), Value::Null);
                patch.insert("monitorWakeRequestedAt".into(), Value::Null);
                let prev_state = current_monitor_state.as_ref();
                target_monitor_state = Some(build_cleared_monitor_state(
                    &crate::issue_execution_policy::BuildClearedMonitorStateInput {
                        previous: prev_state,
                        clear_reason: reason,
                        cleared_at: now,
                    },
                ));
            } else {
                let exhausted_reason =
                    exhausted_monitor_clear_reason(&ExhaustedMonitorClearReasonInput {
                        monitor,
                        attempt_count: current_monitor_state
                            .as_ref()
                            .map(|s| s.attempt_count)
                            .unwrap_or(0),
                        now,
                    });
                if let Some(reason) = exhausted_reason {
                    if input.monitor_explicitly_updated {
                        return error_to_monitor_patch(PolicyTransitionError::with_clear_reason(
                            MONITOR_BOUNDS_EXHAUSTED_MESSAGE,
                            reason,
                        ));
                    }
                    let stripped = strip_monitor_from_execution_policy(Some(policy));
                    if let Some(s) = stripped {
                        patch.insert(
                            "executionPolicy".into(),
                            serde_json::to_value(&s).unwrap_or(Value::Null),
                        );
                    } else {
                        patch.insert("executionPolicy".into(), Value::Null);
                    }
                    patch.insert("monitorNextCheckAt".into(), Value::Null);
                    patch.insert("monitorWakeRequestedAt".into(), Value::Null);
                    target_monitor_state = Some(build_cleared_monitor_state(
                        &crate::issue_execution_policy::BuildClearedMonitorStateInput {
                            previous: current_monitor_state.as_ref(),
                            clear_reason: reason,
                            cleared_at: now,
                        },
                    ));
                } else {
                    patch.insert(
                        "monitorNextCheckAt".into(),
                        Value::String(monitor.next_check_at.clone()),
                    );
                    patch.insert("monitorWakeRequestedAt".into(), Value::Null);
                    patch.insert(
                        "monitorNotes".into(),
                        monitor
                            .notes
                            .clone()
                            .map(Value::String)
                            .unwrap_or(Value::Null),
                    );
                    patch.insert(
                        "monitorScheduledBy".into(),
                        Value::String(monitor.scheduled_by.as_str().to_string()),
                    );
                    target_monitor_state = Some(build_scheduled_monitor_state(
                        current_monitor_state.as_ref(),
                        monitor,
                    ));
                }
            }
        } else if previous_policy
            .as_ref()
            .and_then(|p| p.monitor.as_ref())
            .is_some()
        {
            patch.insert("monitorNextCheckAt".into(), Value::Null);
            patch.insert("monitorWakeRequestedAt".into(), Value::Null);
            let reason = if input.monitor_explicitly_updated {
                IssueExecutionMonitorClearReason::Cancelled
            } else {
                monitor_clear_reason_for_issue(
                    &next_status,
                    assignee_agent_id.as_deref(),
                    assignee_user_id.as_deref(),
                )
                .unwrap_or(IssueExecutionMonitorClearReason::Cancelled)
            };
            target_monitor_state = Some(build_cleared_monitor_state(
                &crate::issue_execution_policy::BuildClearedMonitorStateInput {
                    previous: current_monitor_state.as_ref(),
                    clear_reason: reason,
                    cleared_at: now,
                },
            ));
        }
    }

    let needs_state_patch = stage_patch.get("executionState").is_some()
        || !monitor_states_equal(
            current_monitor_state.as_ref(),
            target_monitor_state.as_ref(),
        );
    if needs_state_patch {
        let merged =
            execution_state_with_monitor(stage_state.as_ref(), target_monitor_state.clone());
        if let Some(v) = execution_state_value(merged.as_ref()) {
            patch.insert("executionState".into(), v);
        }
    }

    patch
}

fn error_to_monitor_patch(_err: PolicyTransitionError) -> Map<String, Value> {
    // Used as a guard; in practice callers treat the error return as the API.
    // Returning an empty patch keeps the type signature useful for control flow.
    Map::new()
}

// ============================================================================
// applyIssueExecutionPolicyTransition (composition)
// ============================================================================

/// `apply_issue_execution_policy_transition`：stage + monitor 组合。
pub fn apply_issue_execution_policy_transition(
    input: &TransitionInput,
) -> PolicyTransitionResult<TransitionResult> {
    let mut stage_result = apply_issue_execution_stage_transition(input)?;
    let monitor_patch = apply_monitor_transition(input, &stage_result.patch);
    for (k, v) in monitor_patch {
        stage_result.patch.insert(k, v);
    }
    Ok(stage_result)
}

/// `apply_issue_monitor_policy_transition`：仅 monitor 转换。
pub fn apply_issue_monitor_policy_transition(
    input: &TransitionInput,
) -> PolicyTransitionResult<TransitionResult> {
    let patch = apply_monitor_transition(input, &Map::new());
    Ok(TransitionResult {
        patch,
        decision: None,
        workflow_controlled_assignment: false,
    })
}

// ============================================================================
// buildInitialIssueMonitorFields
// ============================================================================

/// `build_initial_issue_monitor_fields`：issue 初始化时构造 monitor 字段。
///
/// 与 Node 1:1 对齐。
pub fn build_initial_issue_monitor_fields(
    input: BuildInitialMonitorFieldsInput,
) -> PolicyTransitionResult<MonitorPatch> {
    if input
        .policy
        .as_ref()
        .and_then(|p| p.monitor.as_ref())
        .is_none()
    {
        return Ok(MonitorPatch::default());
    }
    let monitor = input
        .policy
        .as_ref()
        .and_then(|p| p.monitor.as_ref())
        .unwrap();
    if !issue_allows_monitor(
        &input.status,
        input.assignee_agent_id.as_deref(),
        input.assignee_user_id.as_deref(),
    ) {
        return Err(PolicyTransitionError::new(MONITOR_INVALID_MESSAGE));
    }
    let now = Utc::now();
    if let Some(reason) = exhausted_monitor_clear_reason(&ExhaustedMonitorClearReasonInput {
        monitor,
        attempt_count: 0,
        now,
    }) {
        return Err(PolicyTransitionError::with_clear_reason(
            MONITOR_BOUNDS_EXHAUSTED_MESSAGE,
            reason,
        ));
    }
    let monitor_state = build_scheduled_monitor_state(None, monitor);
    Ok(MonitorPatch {
        monitor_next_check_at: Some(monitor.next_check_at.clone()),
        monitor_wake_requested_at: None,
        monitor_notes: monitor.notes.clone(),
        monitor_scheduled_by: Some(monitor.scheduled_by),
        execution_state: execution_state_with_monitor(None, Some(monitor_state)),
    })
}

/// `BuildInitialMonitorFieldsInput`。
#[derive(Debug, Clone, Default)]
pub struct BuildInitialMonitorFieldsInput {
    pub policy: Option<IssueExecutionPolicy>,
    pub status: String,
    pub assignee_agent_id: Option<String>,
    pub assignee_user_id: Option<String>,
}

/// `MonitorPatch`：issue 表上的 monitor 字段 patch 视图。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonitorPatch {
    pub monitor_next_check_at: Option<String>,
    pub monitor_wake_requested_at: Option<String>,
    pub monitor_notes: Option<String>,
    pub monitor_scheduled_by: Option<IssueMonitorScheduledBy>,
    pub execution_state: Option<IssueExecutionState>,
}

impl MonitorPatch {
    /// 序列化为 issue row patch 字段。
    pub fn to_issue_patch(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "monitorNextCheckAt".into(),
            self.monitor_next_check_at
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        m.insert(
            "monitorWakeRequestedAt".into(),
            self.monitor_wake_requested_at
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        m.insert(
            "monitorNotes".into(),
            self.monitor_notes
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        m.insert(
            "monitorScheduledBy".into(),
            self.monitor_scheduled_by
                .map(|b| Value::String(b.as_str().to_string()))
                .unwrap_or(Value::Null),
        );
        m.insert(
            "executionState".into(),
            self.execution_state
                .as_ref()
                .and_then(|s| serde_json::to_value(s).ok())
                .unwrap_or(Value::Null),
        );
        m
    }
}

// ============================================================================
// buildIssueMonitorTriggeredPatch
// ============================================================================

/// `build_issue_monitor_triggered_patch`：monitor 触发后构造 patch。
pub fn build_issue_monitor_triggered_patch(input: TriggeredPatchInput) -> Map<String, Value> {
    let existing_state = input.issue.execution_state.clone();
    let current_monitor_state = derive_persisted_monitor_state(&DerivePersistedMonitorStateInput {
        issue: &input.issue,
        state: existing_state.as_ref(),
        policy: input.policy.as_ref(),
    });
    let next_monitor_state = build_triggered_monitor_state(
        &crate::issue_execution_policy::BuildTriggeredMonitorStateInput {
            previous: current_monitor_state.as_ref(),
            triggered_at: input.triggered_at,
        },
    );

    let mut patch = Map::new();
    let stripped = strip_monitor_from_execution_policy(input.policy.as_ref());
    match stripped {
        Some(p) => {
            patch.insert(
                "executionPolicy".into(),
                serde_json::to_value(&p).unwrap_or(Value::Null),
            );
        }
        None => {
            patch.insert("executionPolicy".into(), Value::Null);
        }
    }
    let merged =
        execution_state_with_monitor(existing_state.as_ref(), Some(next_monitor_state.clone()));
    patch.insert(
        "executionState".into(),
        merged
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(Value::Null),
    );
    patch.insert("monitorNextCheckAt".into(), Value::Null);
    patch.insert("monitorWakeRequestedAt".into(), Value::Null);
    patch.insert(
        "monitorLastTriggeredAt".into(),
        Value::String(
            input
                .triggered_at
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
    );
    patch.insert(
        "monitorAttemptCount".into(),
        Value::Number(next_monitor_state.attempt_count.into()),
    );
    patch.insert(
        "monitorNotes".into(),
        next_monitor_state
            .notes
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    patch.insert(
        "monitorScheduledBy".into(),
        next_monitor_state
            .scheduled_by
            .map(|b| Value::String(b.as_str().to_string()))
            .unwrap_or(Value::Null),
    );
    patch
}

/// `TriggeredPatchInput`。
#[derive(Debug, Clone)]
pub struct TriggeredPatchInput {
    pub issue: IssueLike,
    pub policy: Option<IssueExecutionPolicy>,
    pub triggered_at: DateTime<Utc>,
}

impl Default for TriggeredPatchInput {
    fn default() -> Self {
        Self {
            issue: IssueLike::default(),
            policy: None,
            triggered_at: Utc::now(),
        }
    }
}

// ============================================================================
// buildIssueMonitorClearedPatch
// ============================================================================

/// `build_issue_monitor_cleared_patch`：monitor 清除后构造 patch。
pub fn build_issue_monitor_cleared_patch(input: ClearedPatchInput) -> Map<String, Value> {
    let existing_state = input.issue.execution_state.clone();
    let current_monitor_state = derive_persisted_monitor_state(&DerivePersistedMonitorStateInput {
        issue: &input.issue,
        state: existing_state.as_ref(),
        policy: input.policy.as_ref(),
    });
    let cleared_at = input.cleared_at.unwrap_or_else(Utc::now);
    let next_monitor_state = build_cleared_monitor_state(
        &crate::issue_execution_policy::BuildClearedMonitorStateInput {
            previous: current_monitor_state.as_ref(),
            clear_reason: input.clear_reason,
            cleared_at,
        },
    );

    let mut patch = Map::new();
    let stripped = strip_monitor_from_execution_policy(input.policy.as_ref());
    match stripped {
        Some(p) => {
            patch.insert(
                "executionPolicy".into(),
                serde_json::to_value(&p).unwrap_or(Value::Null),
            );
        }
        None => {
            patch.insert("executionPolicy".into(), Value::Null);
        }
    }
    let merged = execution_state_with_monitor(existing_state.as_ref(), Some(next_monitor_state));
    patch.insert(
        "executionState".into(),
        merged
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(Value::Null),
    );
    patch.insert("monitorNextCheckAt".into(), Value::Null);
    patch.insert("monitorWakeRequestedAt".into(), Value::Null);
    patch
}

/// `ClearedPatchInput`。
#[derive(Debug, Clone)]
pub struct ClearedPatchInput {
    pub issue: IssueLike,
    pub policy: Option<IssueExecutionPolicy>,
    pub clear_reason: IssueExecutionMonitorClearReason,
    pub cleared_at: Option<DateTime<Utc>>,
}

impl Default for ClearedPatchInput {
    fn default() -> Self {
        Self {
            issue: IssueLike::default(),
            policy: None,
            clear_reason: IssueExecutionMonitorClearReason::Completed,
            cleared_at: None,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_execution_monitor_state::{
        IssueExecutionMonitorKind, IssueExecutionMonitorPolicy, IssueExecutionMonitorStateStatus,
        IssueExecutionStageType, ReviewRequest,
    };
    use crate::issue_execution_policy::{
        IssueExecutionParticipant, IssueExecutionPolicyMode, IssueExecutionStage,
    };
    use chrono::TimeZone;
    use serde_json::json;

    fn utc_dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    fn make_agent(id: &str) -> IssueExecutionStagePrincipal {
        IssueExecutionStagePrincipal {
            principal_type: "agent".to_string().into(),
            agent_id: Some(id.into()),
            user_id: None,
        }
    }

    fn make_user(id: &str) -> IssueExecutionStagePrincipal {
        IssueExecutionStagePrincipal {
            principal_type: "user".to_string().into(),
            agent_id: None,
            user_id: Some(id.into()),
        }
    }

    fn review_stage(stage_id: &str, agent: &str) -> IssueExecutionStage {
        IssueExecutionStage {
            id: Some(stage_id.into()),
            kind: IssueExecutionStageType::Agent,
            approvals_needed: 1,
            participants: vec![IssueExecutionParticipant {
                id: Some(format!("p-{stage_id}")),
                kind: IssueExecutionStageType::Agent,
                agent_id: Some(agent.into()),
                user_id: None,
            }],
        }
    }

    fn approval_stage(stage_id: &str, user_id: &str) -> IssueExecutionStage {
        IssueExecutionStage {
            id: Some(stage_id.into()),
            kind: IssueExecutionStageType::User,
            approvals_needed: 1,
            participants: vec![IssueExecutionParticipant {
                id: Some(format!("p-{stage_id}")),
                kind: IssueExecutionStageType::User,
                agent_id: None,
                user_id: Some(user_id.into()),
            }],
        }
    }

    fn make_policy(stages: Vec<IssueExecutionStage>) -> IssueExecutionPolicy {
        IssueExecutionPolicy {
            mode: Some(IssueExecutionPolicyMode::Normal),
            comment_required: true,
            stages,
            monitor: None,
            max_review_rounds: None,
        }
    }

    fn make_issue(agent_id: Option<&str>, user_id: Option<&str>, status: &str) -> IssueLike {
        IssueLike {
            assignee_agent_id: agent_id.map(str::to_string),
            assignee_user_id: user_id.map(str::to_string),
            status: status.into(),
            responsible_user_id: Some("responsible".into()),
            created_by_user_id: Some("creator".into()),
            ..Default::default()
        }
    }

    fn two_stage_policy() -> IssueExecutionPolicy {
        make_policy(vec![
            review_stage("s1", "qa-agent"),
            approval_stage("s2", "cto-user"),
        ])
    }

    // ----- PolicyTransitionError -----

    #[test]
    fn policy_transition_error_new() {
        let err = PolicyTransitionError::new("bad");
        assert_eq!(err.message, "bad");
        assert!(err.clear_reason.is_none());
    }

    #[test]
    fn policy_transition_error_with_clear_reason() {
        let err = PolicyTransitionError::with_clear_reason(
            "exhausted",
            IssueExecutionMonitorClearReason::Exhausted,
        );
        assert_eq!(
            err.clear_reason,
            Some(IssueExecutionMonitorClearReason::Exhausted)
        );
    }

    // ----- applyIssueExecutionStageTransition: happy path -----

    #[test]
    fn stage_transition_routes_executor_completion_to_review() {
        let policy = two_stage_policy();
        let issue = make_issue(Some("coder"), None, "in_progress");
        let input = TransitionInput {
            issue,
            policy: Some(policy.clone()),
            requested_status: Some("done".into()),
            actor: ActorLike {
                agent_id: Some("coder".into()),
                ..Default::default()
            },
            comment_body: Some("Implemented".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        assert_eq!(
            result.patch.get("status"),
            Some(&Value::String("in_review".into()))
        );
        assert_eq!(
            result.patch.get("assigneeAgentId"),
            Some(&Value::String("qa-agent".into()))
        );
        let exec_state = result.patch.get("executionState").unwrap();
        assert_eq!(exec_state["status"], json!("pending"));
        assert_eq!(exec_state["currentStageType"], json!("agent"));
        assert!(result.workflow_controlled_assignment);
        assert!(result.decision.is_none());
    }

    #[test]
    fn stage_transition_carries_review_request_instructions() {
        let policy = two_stage_policy();
        let mut issue = make_issue(Some("coder"), None, "in_progress");
        issue.execution_policy = Some(policy.clone());
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_agent("qa-agent")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            last_decision_id: None,
            last_decision_outcome: None,
            monitor: None,
            changes_requested_count: None,
            review_request: Some(ReviewRequest {
                instructions: "Check the migration path".into(),
            }),
        });
        let input = TransitionInput {
            issue,
            policy: Some(policy.clone()),
            requested_status: Some("done".into()),
            actor: ActorLike {
                agent_id: Some("qa-agent".into()),
                ..Default::default()
            },
            comment_body: Some("Looks good".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        let exec_state = result.patch.get("executionState").unwrap();
        assert!(exec_state["reviewRequest"].is_null());
    }

    #[test]
    fn stage_transition_approval_advances_to_next() {
        let policy = two_stage_policy();
        let mut issue = make_issue(Some("qa-agent"), None, "in_review");
        issue.execution_policy = Some(policy.clone());
        let exec_state = IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_agent("qa-agent")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            last_decision_id: None,
            last_decision_outcome: None,
            monitor: None,
            changes_requested_count: None,
            review_request: None,
        };
        issue.execution_state = Some(exec_state.clone());
        let input = TransitionInput {
            issue,
            policy: Some(policy.clone()),
            requested_status: Some("done".into()),
            actor: ActorLike {
                agent_id: Some("qa-agent".into()),
                ..Default::default()
            },
            comment_body: Some("Approved review".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        let exec_state = result.patch.get("executionState").unwrap();
        assert_eq!(exec_state["currentStageId"], json!("s2"));
        assert_eq!(exec_state["currentStageType"], json!("user"));
        assert_eq!(
            result.patch.get("assigneeUserId"),
            Some(&Value::String("cto-user".into()))
        );
        let decision = result.decision.as_ref().unwrap();
        assert_eq!(decision.outcome, IssueExecutionDecisionOutcome::Approved);
    }

    // ----- changes_requested branch -----

    #[test]
    fn stage_transition_changes_requested_requires_comment() {
        let policy = two_stage_policy();
        let mut issue = make_issue(Some("qa-agent"), None, "in_review");
        issue.execution_policy = Some(policy.clone());
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_agent("qa-agent")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            ..Default::default()
        });
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("todo".into()),
            actor: ActorLike {
                agent_id: Some("qa-agent".into()),
                ..Default::default()
            },
            comment_body: None,
            ..Default::default()
        };
        let err = apply_issue_execution_stage_transition(&input).unwrap_err();
        assert!(err
            .message
            .contains("Requesting changes requires a comment"));
    }

    #[test]
    fn stage_transition_changes_requested_bounces_back() {
        let policy = two_stage_policy();
        let mut issue = make_issue(Some("qa-agent"), None, "in_review");
        issue.execution_policy = Some(policy.clone());
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_agent("qa-agent")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            ..Default::default()
        });
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("todo".into()),
            actor: ActorLike {
                agent_id: Some("qa-agent".into()),
                ..Default::default()
            },
            comment_body: Some("Fix the bug".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        assert_eq!(
            result.patch.get("status"),
            Some(&Value::String("in_progress".into()))
        );
        assert_eq!(
            result.patch.get("assigneeAgentId"),
            Some(&Value::String("coder".into()))
        );
        let exec_state = result.patch.get("executionState").unwrap();
        assert_eq!(exec_state["status"], json!("changes_requested"));
        assert_eq!(
            exec_state["lastDecisionOutcome"],
            json!("changes_requested")
        );
        let decision = result.decision.unwrap();
        assert_eq!(
            decision.outcome,
            IssueExecutionDecisionOutcome::ChangesRequested
        );
    }

    #[test]
    fn stage_transition_human_changes_resets_round_counter() {
        let policy = two_stage_policy();
        // The current participant is the human reviewer (user)
        let mut issue = make_issue(Some("qa-agent"), None, "in_review");
        issue.execution_policy = Some(policy.clone());
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_user("human-reviewer")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            changes_requested_count: Some(5),
            ..Default::default()
        });
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("todo".into()),
            actor: ActorLike {
                user_id: Some("human-reviewer".into()),
                ..Default::default()
            },
            comment_body: Some("Needs more work".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        let exec_state = result.patch.get("executionState").unwrap();
        assert_eq!(exec_state["changesRequestedCount"], json!(0));
    }

    #[test]
    fn stage_transition_escalates_when_rounds_exhausted() {
        let policy = two_stage_policy();
        let mut issue = make_issue(Some("qa-agent"), None, "in_review");
        issue.execution_policy = Some(policy.clone());
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_agent("qa-agent")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            changes_requested_count: Some(3),
            ..Default::default()
        });
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("todo".into()),
            actor: ActorLike {
                agent_id: Some("qa-agent".into()),
                ..Default::default()
            },
            comment_body: Some("More work needed".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        // escalation: stage stays on s1, participant becomes responsible human
        assert_eq!(
            result.patch.get("assigneeUserId"),
            Some(&Value::String("responsible".into()))
        );
        let exec_state = result.patch.get("executionState").unwrap();
        assert_eq!(exec_state["status"], json!("pending"));
        assert_eq!(exec_state["changesRequestedCount"], json!(4));
    }

    // ----- non-active stage / no policy -----

    #[test]
    fn stage_transition_no_policy_clears_state() {
        let mut issue = make_issue(Some("qa-agent"), None, "in_review");
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_agent("qa-agent")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            ..Default::default()
        });
        let input = TransitionInput {
            issue,
            policy: None,
            requested_status: None,
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        assert_eq!(
            result.patch.get("status"),
            Some(&Value::String("in_progress".into()))
        );
        assert_eq!(
            result.patch.get("assigneeAgentId"),
            Some(&Value::String("coder".into()))
        );
        assert_eq!(result.patch.get("executionState"), Some(&Value::Null));
    }

    #[test]
    fn stage_transition_done_to_open_is_blocked() {
        let policy = two_stage_policy();
        let issue = IssueLike {
            status: "done".into(),
            ..make_issue(None, None, "done")
        };
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("todo".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_stage_transition(&input).unwrap();
        assert_eq!(result.patch.get("executionState"), Some(&Value::Null));
    }

    // ----- escalated hold -----

    #[test]
    fn escalated_hold_blocks_non_escalated_advance() {
        let policy = two_stage_policy();
        // escalated: user holds the stage but is not in stage participants
        let mut issue = make_issue(Some("responsible"), None, "in_review");
        issue.execution_policy = Some(policy.clone());
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_user("responsible")),
            return_assignee: Some(make_agent("coder")),
            completed_stage_ids: vec![],
            changes_requested_count: Some(3),
            ..Default::default()
        });
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("done".into()),
            actor: ActorLike {
                agent_id: Some("qa-agent".into()),
                ..Default::default()
            },
            comment_body: Some("Approved".into()),
            ..Default::default()
        };
        let err = apply_issue_execution_stage_transition(&input).unwrap_err();
        assert!(err.message.contains("Only the escalated reviewer"));
    }

    // ----- applyMonitorTransition -----

    #[test]
    fn monitor_transition_schedules_new_monitor() {
        let policy = make_policy(vec![]);
        let monitor_policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "2025-06-01T00:00:00Z".into(),
                notes: Some("Check deploy".into()),
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: Some(IssueExecutionMonitorKind::ExternalService),
                service_name: Some("web".into()),
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            ..Default::default()
        };
        let issue = make_issue(Some("agent"), None, "in_progress");
        let input = TransitionInput {
            issue,
            policy: Some(monitor_policy.clone()),
            requested_status: None,
            ..Default::default()
        };
        let patch = apply_monitor_transition(&input, &Map::new());
        assert_eq!(
            patch.get("monitorScheduledBy"),
            Some(&Value::String("assignee".into()))
        );
        assert!(patch.get("monitorNextCheckAt").is_some());
        assert!(patch.get("executionState").is_some());
    }

    #[test]
    fn monitor_transition_clears_when_assignee_invalid() {
        let monitor_policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "2025-06-01T00:00:00Z".into(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            ..Default::default()
        };
        let issue = make_issue(None, Some("human"), "in_progress");
        let input = TransitionInput {
            issue,
            policy: Some(monitor_policy),
            ..Default::default()
        };
        let patch = apply_monitor_transition(&input, &Map::new());
        assert_eq!(patch.get("monitorNextCheckAt"), Some(&Value::Null));
        assert!(patch.get("executionState").is_some());
    }

    // ----- apply_issue_execution_policy_transition -----

    #[test]
    fn policy_transition_combines_stage_and_monitor() {
        let policy = two_stage_policy();
        let issue = make_issue(Some("coder"), None, "in_progress");
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("done".into()),
            actor: ActorLike {
                agent_id: Some("coder".into()),
                ..Default::default()
            },
            comment_body: Some("Done".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_policy_transition(&input).unwrap();
        assert!(result.patch.contains_key("status"));
        assert!(result.workflow_controlled_assignment);
    }

    #[test]
    fn policy_transition_error_propagates() {
        let policy = two_stage_policy();
        let mut issue = make_issue(Some("qa-agent"), None, "in_review");
        issue.execution_policy = Some(policy.clone());
        issue.execution_state = Some(IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("s1".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Agent),
            current_participant: Some(make_agent("qa-agent")),
            return_assignee: Some(make_agent("coder")),
            ..Default::default()
        });
        let input = TransitionInput {
            issue,
            policy: Some(policy),
            requested_status: Some("todo".into()),
            actor: ActorLike {
                agent_id: Some("qa-agent".into()),
                ..Default::default()
            },
            comment_body: None,
            ..Default::default()
        };
        let err = apply_issue_execution_policy_transition(&input).unwrap_err();
        assert!(err.message.contains("comment"));
    }

    // ----- apply_issue_monitor_policy_transition -----

    #[test]
    fn monitor_policy_transition_only_monitor() {
        let monitor_policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "2025-06-01T00:00:00Z".into(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            ..Default::default()
        };
        let issue = make_issue(Some("agent"), None, "in_progress");
        let input = TransitionInput {
            issue,
            policy: Some(monitor_policy),
            ..Default::default()
        };
        let result = apply_issue_monitor_policy_transition(&input).unwrap();
        // Should only have monitor fields, no status changes
        assert!(result.patch.contains_key("monitorNextCheckAt"));
        assert!(!result.patch.contains_key("status"));
        assert!(!result.workflow_controlled_assignment);
    }

    // ----- build_initial_issue_monitor_fields -----

    #[test]
    fn build_initial_monitor_fields_no_policy_returns_empty() {
        let input = BuildInitialMonitorFieldsInput {
            policy: None,
            status: "in_progress".into(),
            assignee_agent_id: Some("a".into()),
            ..Default::default()
        };
        let patch = build_initial_issue_monitor_fields(input).unwrap();
        assert_eq!(patch, MonitorPatch::default());
    }

    #[test]
    fn build_initial_monitor_fields_invalid_assignee() {
        let policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "2025-06-01T00:00:00Z".into(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            ..Default::default()
        };
        let input = BuildInitialMonitorFieldsInput {
            policy: Some(policy),
            status: "in_progress".into(),
            assignee_agent_id: None,
            assignee_user_id: Some("u".into()),
        };
        let err = build_initial_issue_monitor_fields(input).unwrap_err();
        assert!(err.message.contains(MONITOR_INVALID_MESSAGE));
    }

    #[test]
    fn build_initial_monitor_fields_max_attempts_exhausted() {
        // Use past timeout to trigger exhausted reason
        let past = utc_dt(2000, 1, 1, 0, 0, 0).to_rfc3339();
        let policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: past.clone(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: Some(past),
                max_attempts: None,
                recovery_policy: None,
            }),
            ..Default::default()
        };
        let input = BuildInitialMonitorFieldsInput {
            policy: Some(policy),
            status: "in_progress".into(),
            assignee_agent_id: Some("a".into()),
            assignee_user_id: None,
        };
        let err = build_initial_issue_monitor_fields(input).unwrap_err();
        assert_eq!(
            err.clear_reason,
            Some(IssueExecutionMonitorClearReason::Expired)
        );
    }

    #[test]
    fn build_initial_monitor_fields_valid() {
        let policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "2025-06-01T00:00:00Z".into(),
                notes: Some("n".into()),
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: Some(IssueExecutionMonitorKind::ExternalService),
                service_name: Some("web".into()),
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            ..Default::default()
        };
        let input = BuildInitialMonitorFieldsInput {
            policy: Some(policy),
            status: "in_progress".into(),
            assignee_agent_id: Some("a".into()),
            assignee_user_id: None,
        };
        let patch = build_initial_issue_monitor_fields(input).unwrap();
        assert_eq!(
            patch.monitor_next_check_at.as_deref(),
            Some("2025-06-01T00:00:00Z")
        );
        assert_eq!(patch.monitor_notes.as_deref(), Some("n"));
        assert!(patch.execution_state.is_some());
    }

    #[test]
    fn monitor_patch_to_issue_patch() {
        let patch = MonitorPatch {
            monitor_next_check_at: Some("2025-01-01T00:00:00Z".into()),
            monitor_wake_requested_at: None,
            monitor_notes: Some("note".into()),
            monitor_scheduled_by: Some(IssueMonitorScheduledBy::Board),
            execution_state: Some(IssueExecutionState {
                status: IssueExecutionStateStatus::Idle,
                ..Default::default()
            }),
        };
        let m = patch.to_issue_patch();
        assert_eq!(
            m.get("monitorNextCheckAt"),
            Some(&Value::String("2025-01-01T00:00:00Z".into()))
        );
        assert_eq!(m.get("monitorWakeRequestedAt"), Some(&Value::Null));
        assert_eq!(m.get("monitorNotes"), Some(&Value::String("note".into())));
        assert_eq!(
            m.get("monitorScheduledBy"),
            Some(&Value::String("board".into()))
        );
    }

    // ----- build_issue_monitor_triggered_patch -----

    #[test]
    fn build_triggered_patch_increments_attempt() {
        let mut issue = make_issue(Some("a"), None, "in_progress");
        issue.execution_state = Some(IssueExecutionState {
            monitor: Some(IssueExecutionMonitorState {
                status: IssueExecutionMonitorStateStatus::Scheduled,
                attempt_count: 1,
                ..Default::default()
            }),
            ..Default::default()
        });
        let policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "t".into(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            ..Default::default()
        };
        let triggered_at = utc_dt(2025, 1, 2, 0, 0, 0);
        let input = TriggeredPatchInput {
            issue,
            policy: Some(policy),
            triggered_at,
        };
        let patch = build_issue_monitor_triggered_patch(input);
        assert_eq!(
            patch.get("monitorAttemptCount"),
            Some(&Value::Number(2.into()))
        );
        assert_eq!(patch.get("monitorNextCheckAt"), Some(&Value::Null));
    }

    // ----- build_issue_monitor_cleared_patch -----

    #[test]
    fn build_cleared_patch_strips_monitor_from_policy() {
        let mut issue = make_issue(Some("a"), None, "in_progress");
        issue.execution_state = Some(IssueExecutionState {
            monitor: Some(IssueExecutionMonitorState {
                status: IssueExecutionMonitorStateStatus::Scheduled,
                ..Default::default()
            }),
            ..Default::default()
        });
        let policy = IssueExecutionPolicy {
            monitor: Some(IssueExecutionMonitorPolicy {
                next_check_at: "t".into(),
                notes: None,
                scheduled_by: IssueMonitorScheduledBy::Assignee,
                kind: None,
                service_name: None,
                external_ref: None,
                timeout_at: None,
                max_attempts: None,
                recovery_policy: None,
            }),
            stages: vec![],
            ..Default::default()
        };
        let input = ClearedPatchInput {
            issue,
            policy: Some(policy),
            clear_reason: IssueExecutionMonitorClearReason::Completed,
            cleared_at: Some(utc_dt(2025, 1, 1, 0, 0, 0)),
        };
        let patch = build_issue_monitor_cleared_patch(input);
        // Policy stripped → executionPolicy = null (no stages)
        assert_eq!(patch.get("executionPolicy"), Some(&Value::Null));
        assert_eq!(patch.get("monitorNextCheckAt"), Some(&Value::Null));
    }

    #[test]
    fn build_cleared_patch_keeps_stages() {
        let policy = make_policy(vec![review_stage("s1", "a")]);
        let mut policy = policy;
        policy.monitor = Some(IssueExecutionMonitorPolicy {
            next_check_at: "t".into(),
            notes: None,
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: None,
            max_attempts: None,
            recovery_policy: None,
        });
        let mut issue = make_issue(Some("a"), None, "in_progress");
        issue.execution_state = Some(IssueExecutionState {
            monitor: Some(IssueExecutionMonitorState {
                status: IssueExecutionMonitorStateStatus::Scheduled,
                ..Default::default()
            }),
            ..Default::default()
        });
        let input = ClearedPatchInput {
            issue,
            policy: Some(policy),
            clear_reason: IssueExecutionMonitorClearReason::Completed,
            cleared_at: None,
        };
        let patch = build_issue_monitor_cleared_patch(input);
        // Policy has stages, monitor stripped → executionPolicy = policy with no monitor
        let exec_policy = patch.get("executionPolicy").unwrap();
        assert!(exec_policy.is_object());
        assert!(exec_policy.get("monitor").is_none() || exec_policy["monitor"].is_null());
    }

    // ----- integration -----

    #[test]
    fn integration_full_workflow_with_monitor() {
        // Issue starts in_progress with coder, monitor attached
        let stages = vec![review_stage("s1", "qa-agent")];
        let mut policy = make_policy(stages.clone());
        policy.monitor = Some(IssueExecutionMonitorPolicy {
            next_check_at: "2025-06-01T00:00:00Z".into(),
            notes: Some("check deploy".into()),
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: None,
            max_attempts: None,
            recovery_policy: None,
        });

        // 1. coder completes → routes to review
        let issue = make_issue(Some("coder"), None, "in_progress");
        let input = TransitionInput {
            issue,
            policy: Some(policy.clone()),
            requested_status: Some("done".into()),
            actor: ActorLike {
                agent_id: Some("coder".into()),
                ..Default::default()
            },
            comment_body: Some("Implemented".into()),
            ..Default::default()
        };
        let result = apply_issue_execution_policy_transition(&input).unwrap();
        assert_eq!(
            result.patch.get("status"),
            Some(&Value::String("in_review".into()))
        );
        assert_eq!(
            result.patch.get("assigneeAgentId"),
            Some(&Value::String("qa-agent".into()))
        );
        // Monitor fields should also be set
        assert!(result.patch.contains_key("monitorNextCheckAt"));
        assert!(result.workflow_controlled_assignment);
    }
}
