//! `recovery` 子模块门面：heartbeat 周期内 issue 恢复相关分类、键构造、状态判定。
//!
//! 对齐 Node `services/recovery/`：
//! - `origins` —— origin / reason / key 前缀常量 + 强类型枚举 + key 构建解析
//! - `run_liveness_continuations` —— 心跳 run liveness 续跑决策
//! - `successful_run_handoff` —— 成功 run 的 handoff 决策
//! - `issue_graph_liveness` —— issue 关系图 liveness 分类器
//! - `environment_run_orchestrator` —— 环境运行编排（错误归一化 + 步骤编排）
//! - `model_profile_hint` —— 模型 profile hint 注入与 scrub

pub mod adapter_failure_classification;
pub mod continuation_retry_summary;
pub mod environment_run_orchestrator;
pub mod escalate;
pub mod escalate_db;
pub mod escalation_creation;
pub mod heartbeat_ticker;
pub mod issue_graph_liveness;
pub mod issue_graph_liveness_db;
pub mod liveness_pipeline;
pub mod model_profile_hint;
pub mod orchestrator;
pub mod origins;
pub mod pause_hold_guard;
pub mod run_liveness_continuations;
pub mod scan_silent_active_runs_db;
pub mod scheduler;
pub mod scheduler_db;
pub mod source_scoped_recovery_action;
pub mod stale_issue_lock_sweep;
pub mod successful_run_handoff;
pub mod watchdog_decision_recording;

pub mod resolved_dependency_wake_backstop;

pub mod collect_issue_graph_liveness_findings;

pub mod liveness_dependency_cleanup;

pub mod build_issue_graph_liveness_auto_recovery_preview;

pub mod stale_run_auto_dismiss;

pub mod run_finished_recovery_cleanup;

pub mod continuation_observation;

pub mod enqueue_stranded_issue_recovery;

pub mod enqueue_wakeup_for_evaluation_issue;

pub mod provider_quota_recovery_monitor;
pub mod recovery_timer_interval;
pub mod schedule_provider_quota_recovery_monitor;

pub mod continuation_waiting_on_review;

pub mod persist_adapter_failure_recovery_classification;

pub mod summarize_run_failure;

pub mod stranded_issue_recovery_queries;

pub mod build_stranded_issue_recovery_description;
pub mod resolve_recovery_owner_agent;
pub mod resolve_stale_run_owner_agent;

pub mod ensure_stranded_issue_recovery_issue;

pub mod build_recovery_issue_in_place_escalation_comment;

pub mod build_execution_review_participant_recovery_comment;

pub mod build_configuration_incomplete_comment;

pub mod build_execution_review_participant_unavailable_comment;
pub mod build_recovery_comment_display;

pub mod build_liveness_escalation_description;
pub mod build_liveness_original_issue_comment;

pub mod build_stale_run_evaluation_description;

pub mod ensure_source_issue_commented_for_stale_evaluation;

pub mod create_or_update_stale_run_evaluation_full;

pub mod is_recovery_origin_issue;

pub mod is_terminal_issue_status;

pub mod latest_same_run_source_terminal_evidence;

pub mod append_recovery_run_event;

pub mod finalize_agent_after_source_resolved_run;

pub mod collect_stale_run_evidence;

pub mod resolve_active_recovery_action_after_source_resolved;

pub mod cleanup_source_resolved_run_process;

pub mod redact_watchdog_evidence_text;

pub mod load_watchdog_redaction_options;

pub mod get_company_issue_prefix;

pub mod load_latest_heartbeat_run_for_issue;

pub mod reconcile_stranded_assigned_issues;

pub mod issue_change_receipt;

pub mod reconcile_issue_graph_liveness;

pub use adapter_failure_classification::{
    classify_adapter_failure, classify_continuation_failure, AdapterFailureRecoveryClassification,
    ContinuationRetryClassification, ContinuationRetryKind,
    PROVIDER_QUOTA_RECOVERY_DEFAULT_BACKOFF_MS,
};
pub use continuation_retry_summary::{
    load_continuation_retry_summary, should_escalate_due_to_retry_limit,
    should_skip_due_to_backoff, summarize_continuation_retries_from_rows, ContinuationRetrySummary,
    ContinuationRunRow, CONTINUATION_RECOVERY_DEFAULT_MAX_ATTEMPTS,
    CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS, CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS,
    INTERACTION_CONTINUATION_REQUEUE_MAX_ATTEMPTS, ISSUE_CONTINUATION_NEEDED_RETRY_REASON,
    UNSUCCESSFUL_HEARTBEAT_RUN_TERMINAL_STATUSES,
};
pub use environment_run_orchestrator::{
    build_lease_context, first_non_empty_line, format_provision_failure_detail,
    lease_needs_release, plan_acquire_for_run, plan_realize_for_run, select_environment_id,
    validate_acquire_input, AcquireForRunInput, AcquireStep, EnvironmentAcquisitionResult,
    EnvironmentErrorCode, EnvironmentLease, EnvironmentLeaseContext, EnvironmentRealizationResult,
    EnvironmentRef, EnvironmentReleaseError, EnvironmentReleaseResult, EnvironmentRunError,
    EnvironmentRunErrorDetails, ProvisionFailureDetailInput, RealizeForRunInput, RealizeStep,
    ReleaseForRunInput,
};
pub use escalate::{
    decide_escalation, is_terminal_or_hidden, should_attempt_source_escalation, EscalationDecision,
    IssueSnapshot, RecoveryInPlacePlan, SkipReason, SourceEscalationPlan,
};
pub use escalate_db::{
    escalate_stranded_assigned_issue, escalate_stranded_recovery_issue_in_place, EscalateDbInput,
    EscalateDbResult, EscalateOutcome,
};
pub use issue_graph_liveness::{
    classify_issue_graph_liveness, IssueGraphLivenessInput, IssueLivenessAgentInput,
    IssueLivenessDependencyPathEntry, IssueLivenessExecutionPathInput, IssueLivenessFinding,
    IssueLivenessIssueInput, IssueLivenessOwnerCandidate, IssueLivenessOwnerCandidateReason,
    IssueLivenessRelationInput, IssueLivenessSeverity, IssueLivenessState,
    IssueLivenessWaitingPathInput,
};
pub use liveness_pipeline::{
    classify_pipeline_severity, evaluate_liveness_pipeline, plan_liveness_pipeline,
    should_page_oncall, summary_pipeline_output, IssueGraphInput, LivenessPipelineInput,
    LivenessPipelineOutput, LivenessPipelineStep, RunLivenessContinuationsInput,
    SuccessfulRunHandoffsInput, PIPELINE_STEPS,
};
pub use model_profile_hint::{
    recovery_assignee_adapter_overrides, scrub_recovery_model_profile_hints,
    status_only_recovery_guard_context, with_recovery_model_profile_hint,
    RecoveryAssigneeAdapterOverrides, RecoveryModelProfileWorkClass,
    RECOVERY_MODEL_PROFILE_HINT_KEYS, RECOVERY_MODEL_PROFILE_KEY,
};
pub use orchestrator::{
    ensure_source_scoped_recovery_action, persist_recovery_wake,
    persist_source_scoped_recovery_action, recovery_action_wake_input, PersistedRecoveryAction,
    RecoveryDispatchIntent, RecoveryOrchestrationResult,
};
pub use origins::{
    build_issue_graph_liveness_incident_key, build_issue_graph_liveness_leaf_key,
    is_stranded_issue_recovery_origin_kind, parse_issue_graph_liveness_incident_key,
    IncidentKeyInput, LeafKeyInput, ParsedIncidentKey, RecoveryKeyPrefix, RecoveryOriginKind,
    RecoveryReasonKind,
};
pub use pause_hold_guard::{
    is_automatic_recovery_suppressed_by_pause_hold, walk_pause_hold_chain, PauseHoldGateHit,
    MAX_PAUSE_HOLD_ANCESTOR_DEPTH,
};
pub use run_liveness_continuations::{
    build_run_liveness_continuation_idempotency_key, decide_run_liveness_continuation,
    read_continuation_attempt, AgentRef, DecideRunLivenessContinuationInput, HeartbeatRunRef,
    IdempotencyKeyInput, IssueRef, RunContinuationDecision, ACTIONABLE_LIVENESS_STATES,
    CONTINUATION_ACTIVE_ISSUE_STATUSES, CONTINUATION_AGENT_STATUSES,
    DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS, IDEMPOTENT_WAKE_STATUSES,
    RUN_LIVENESS_CONTINUATION_REASON,
};
pub use scheduler::{
    build_routing_for_cause, decide_recovery_cause, decide_recovery_scheduler_plan,
    is_provider_quota_recovery, read_context_retry_reason, read_recovery_run_error_family,
    read_workspace_validation_fingerprint, read_workspace_validation_payload,
    read_workspace_validation_reason, SchedulerCandidate, SchedulerContext, SchedulerDispatchKind,
    SchedulerRoutingHints, SchedulerRunInput,
    STRANDED_RECOVERY_EXECUTION_REVIEW_PARTICIPANT_REASON,
    STRANDED_RECOVERY_GIT_WORKTREE_INCOHERENCE_REASON, STRANDED_RECOVERY_PROVIDER_QUOTA_FAMILY,
    STRANDED_RECOVERY_RUN_ERROR_FAMILY_KEY, STRANDED_RECOVERY_SUCCESSFUL_RUN_MISSING_STATE_REASON,
    STRANDED_RECOVERY_WORKSPACE_VALIDATION_FINGERPRINT_KEY,
    STRANDED_RECOVERY_WORKSPACE_VALIDATION_KEY, STRANDED_RECOVERY_WORKSPACE_VALIDATION_REASON_KEY,
};
pub use scheduler_db::{
    dispatch_intent_label, dispatch_wake_for_recovery_action,
    ensure_source_scoped_recovery_action_for_issue,
    persist_source_scoped_recovery_action_for_issue, reconcile_and_escalate_stranded_for_company,
    reconcile_stranded_assigned_issues_for_company, wake_input_for, ReconcileAndEscalateOutcome,
    ReconcileAndEscalateSweepResult, ReconcileSweepOutcome, ReconcileSweepResult, SchedulerDbInput,
    SchedulerDbResult,
};
pub use source_scoped_recovery_action::{
    build_source_scoped_recovery_action_plan, plan_to_upsert_recovery_action,
    SourceScopedRecoveryActionPlan, StrandedRecoveryCause,
};
pub use successful_run_handoff::{
    build_successful_run_handoff_context_snapshot, build_successful_run_handoff_idempotency_key,
    build_successful_run_handoff_instruction, build_successful_run_handoff_payload,
    decide_successful_run_handoff, is_comment_driven_wake, is_corrective_handoff_run,
    is_idempotent_finish_successful_run_handoff_wake_status, is_issue_monitor_maintenance_run,
    is_productive_successful_run, is_successful_run_handoff_valid_path_skip,
    AgentRef as SuccessfulRunAgentRef, BuildInstructionInput, DecideSuccessfulRunHandoffInput,
    HeartbeatRunRef as SuccessfulRunHeartbeatRunRef, IssueRef as SuccessfulRunIssueRef,
    RunLivenessState, SuccessfulRunHandoffDecision, DEFAULT_MAX_SUCCESSFUL_RUN_HANDOFF_ATTEMPTS,
    FINISH_SUCCESSFUL_RUN_HANDOFF_REASON, IDEMPOTENT_HANDOFF_WAKE_STATUSES,
    NON_INVOKABLE_AGENT_STATUSES, PRODUCTIVE_SUCCESS_LIVENESS_STATES,
    SUCCESSFUL_RUN_HANDOFF_OPTIONS, SUCCESSFUL_RUN_HANDOFF_VALID_PATH_SKIP_REASONS,
    SUCCESSFUL_RUN_MISSING_STATE_REASON,
};

pub use stale_issue_lock_sweep::{
    is_terminal_run_status, sweep_stale_issue_locks, SweepStaleIssueLocksResult,
    TERMINAL_HEARTBEAT_RUN_STATUSES,
};

pub use heartbeat_ticker::{
    list_active_companies, run_heartbeat_tick, run_sweeps_for_company, HeartbeatTickResult,
    HeartbeatTicker, HeartbeatTickerConfig, StrandedSweepOutcome,
};

pub use watchdog_decision_recording::{
    record_watchdog_decision, WatchdogDecisionActor, WatchdogDecisionError, WatchdogDecisionInput,
    ACTIVE_RUN_OUTPUT_CONTINUE_REARM_MS, STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND,
};

pub use issue_graph_liveness_db::{
    ensure_issue_blocked_by_escalation, existing_blocker_issue_ids,
    existing_unresolved_blocker_issue_ids, find_open_liveness_escalation,
    find_open_liveness_recovery_by_parsed_leaf, find_open_liveness_recovery_issue_for_fingerprint,
    find_recent_completed_liveness_recovery_issue, list_issue_dependency_readiness_map,
    EnsureBlockedByEscalationInput, IssueSummaryRow, ISSUE_GRAPH_LIVENESS_ESCALATION_ORIGIN_KIND,
};

pub use scan_silent_active_runs_db::{
    create_or_update_stale_run_evaluation, extract_issue_id_from_context,
    find_closed_stale_run_evaluation, find_open_stale_run_evaluation,
    has_dismissed_false_positive_decision, scan_silent_active_runs, ScanSilentRunsOptions,
    ScanSilentRunsResult, SilentRunCandidate, StaleRunEvaluationOutcome, StaleRunEvaluationRow,
};

pub use escalation_creation::{
    create_issue_graph_liveness_escalation, CreateEscalationInput, EscalationOutcome,
};

pub use resolved_dependency_wake_backstop::{
    build_issue_blockers_resolved_wake_idempotency_key,
    find_existing_issue_blockers_resolved_wake_for_any_key,
    reconcile_resolved_dependency_wake_backstop, BackstopWakeOutcome,
    ResolvedDependencyWakeBackstopOptions, ResolvedDependencyWakeBackstopResult,
    ISSUE_BLOCKERS_RESOLVED_WAKE_REASON, RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT,
};

pub use collect_issue_graph_liveness_findings::{
    collect_issue_graph_liveness_findings, CollectFindingsOptions,
};

pub use liveness_dependency_cleanup::{
    is_finding_inside_auto_recovery_lookback, latest_dependency_updated_at_for_finding,
    liveness_dependency_issue_key, load_liveness_dependency_updated_at_by_issue,
    normalize_lookback_hours, retire_done_liveness_recovery_blockers,
    retire_obsolete_liveness_recovery_issues, RetireDoneBlockersResult, RetireObsoleteResult,
    DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS,
    MAX_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS,
    MIN_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS,
};

pub use reconcile_issue_graph_liveness::{
    reconcile_issue_graph_liveness, ReconcileIssueGraphLivenessOptions,
    ReconcileIssueGraphLivenessResult,
};

pub use build_issue_graph_liveness_auto_recovery_preview::{
    build_issue_graph_liveness_auto_recovery_preview, AutoRecoveryPreviewOptions,
    IssueGraphLivenessAutoRecoveryPreview, IssueGraphLivenessAutoRecoveryPreviewItem,
};

pub use stale_run_auto_dismiss::{
    auto_dismiss_closed_evaluation, fold_source_resolved_stale_run,
    AutoDismissClosedEvaluationInput, AutoDismissClosedEvaluationOutcome, AutoDismissSkipReason,
    FoldSourceResolvedInput, FoldSourceResolvedOutcome, FoldSourceResolvedSkipReason,
};

pub use run_finished_recovery_cleanup::{
    outcome_from_status_str, outcome_to_status_str, resolve_recovery_action_on_run_finished,
    RunFinishedCleanupResult, RunFinishedOutcome, RunFinishedSkipReason,
};

pub use reconcile_stranded_assigned_issues::{
    parse_issue_execution_state, reconcile_stranded_assigned_issues, ExecutionStateParticipant,
    ParsedExecutionState, ReconcileStrandedOptions, StrandedCandidate, StrandedEarlyDecision,
    StrandedReconcileResult, StrandedSkipReason,
};

pub use continuation_observation::{
    get_latest_accepted_continuation_interaction, has_successful_run_since,
    AcceptedContinuationInteraction,
};

pub use enqueue_stranded_issue_recovery::{
    enqueue_initial_assigned_todo_dispatch, enqueue_stranded_issue_recovery,
    EnqueueInitialDispatchInput, EnqueueStrandedRecoveryInput, EnqueueStrandedRecoveryResult,
    EnqueueStrandedSkipReason,
};

pub use enqueue_wakeup_for_evaluation_issue::{
    enqueue_wakeup_for_evaluation_issue, EnqueueEvaluationWakeInput, EnqueueEvaluationWakeResult,
    EnqueueEvaluationWakeSkipReason,
};

pub use issue_change_receipt::{
    build_issue_changes, FieldChange, IdArrayChange, IssueChanges, RelationChangeInput,
    ISSUE_CHANGE_TEXT_BUDGET,
};

pub use provider_quota_recovery_monitor::{
    ensure_provider_quota_wait_recovery_monitor, EnsureProviderQuotaMonitorInput,
    ProviderQuotaMonitorResult, DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS,
    PROVIDER_QUOTA_RETRY_NOT_BEFORE_KEY,
};
pub use recovery_timer_interval::read_recovery_timer_interval_ms;
pub use schedule_provider_quota_recovery_monitor::{
    persist_provider_quota_recovery_classification, schedule_provider_quota_recovery_monitor,
    ProviderQuotaRecoveryMonitorResult, ScheduleProviderQuotaRecoveryMonitorInput,
    PROVIDER_QUOTA_MONITOR_SERVICE_NAME,
};

pub use continuation_waiting_on_review::{
    add_recovery_system_comment, build_waiting_on_review_comment_body, list_open_children_ids,
    log_recovery_issue_activity, resolve_continuation_waiting_on_review, set_blocked_by_issue_ids,
    set_issue_status_blocked,
};

pub use persist_adapter_failure_recovery_classification::{
    build_classified_result_json, error_code_for_classification, error_family_for_classification,
    persist_adapter_failure_recovery_classification,
};

pub use summarize_run_failure::{summarize_run_failure_for_issue_comment, RunFailureView};

pub use stranded_issue_recovery_queries::{
    find_open_stranded_issue_recovery_issue, is_stranded_issue_recovery_issue,
    is_unique_stranded_issue_recovery_conflict, STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
};

pub use resolve_recovery_owner_agent::{
    collect_stranded_recovery_candidate_ids, fetch_agent_org_row, list_company_executive_agents,
    resolve_invokable_recovery_agent_id, resolve_stranded_issue_recovery_owner_agent_id,
};

pub use resolve_stale_run_owner_agent::{
    resolve_stale_run_owner_agent_id, ResolveStaleRunOwnerAgentInput,
};

pub use build_stranded_issue_recovery_description::{
    build_stranded_issue_recovery_description, AgentShortView,
    BuildStrandedIssueRecoveryDescriptionInput, LatestRunView,
};

pub use ensure_stranded_issue_recovery_issue::{
    ensure_stranded_issue_recovery_issue, EnsureStrandedIssueRecoveryInput,
};

pub use build_recovery_issue_in_place_escalation_comment::{
    build_recovery_issue_in_place_escalation_comment,
    BuildRecoveryIssueInPlaceEscalationCommentInput, EscalationRunView,
};

pub use build_execution_review_participant_recovery_comment::build_execution_review_participant_recovery_comment;

pub use build_execution_review_participant_unavailable_comment::build_execution_review_participant_unavailable_comment;

pub use build_liveness_escalation_description::{
    build_liveness_escalation_description, format_dependency_path,
};
pub use build_liveness_original_issue_comment::{
    build_liveness_original_issue_comment, OriginalIssueCommentContext,
};

pub use build_stale_run_evaluation_description::{
    build_stale_run_evaluation_description, format_duration,
    BuildStaleRunEvaluationDescriptionInput, StaleAgentView, StaleEvaluationLevel,
    StaleIssueLinkView, StaleRunEventView, StaleRunEvidenceView, StaleRunView,
    StaleSourceIssueView, ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS,
    ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS,
};

pub use ensure_source_issue_commented_for_stale_evaluation::{
    ensure_source_issue_commented_for_stale_evaluation, EvaluationIssueRef, SourceIssueView,
    StaleEscalationCommentContext,
};

pub use create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};

pub use is_recovery_origin_issue::{
    is_recovery_origin_issue_str, log_recovery_recursion_refused_activity,
    LogRecursionRefusedInput, RECOVERY_ORIGIN_KINDS,
};

pub use is_terminal_issue_status::{is_terminal_issue_status_str, is_terminal_issue_status_string};

pub use latest_same_run_source_terminal_evidence::{
    latest_same_run_source_terminal_evidence, LatestSameRunSourceTerminalEvidence,
};

pub use append_recovery_run_event::{append_recovery_run_event, AppendRecoveryRunEventInput};

pub use finalize_agent_after_source_resolved_run::{
    finalize_agent_after_source_resolved_run, FinalizeAgentInput,
};

pub use collect_stale_run_evidence::{
    collect_stale_run_evidence, CollectStaleRunEvidenceInput, CollectedStaleRunEvidence,
};

pub use get_company_issue_prefix::{get_company_issue_prefix, DEFAULT_COMPANY_ISSUE_PREFIX};

pub use load_latest_heartbeat_run_for_issue::load_latest_heartbeat_run_for_issue;
