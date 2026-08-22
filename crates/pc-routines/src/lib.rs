#![forbid(unsafe_code)]

//! Routines domain service layer.
//!
//! Provides [`RoutineService`] — a high-level facade over [`pc_repos::routine::RoutineRepo`]
//! that:
//!
//! * Validates inputs (non-empty title, allowed priority, allowed status, ...)
//! * Routes writes through a [`RoutineHook`] chain so callers can layer
//!   activity / realtime / plugin side-effects without touching SQL.
//! * Translates repo `sqlx::Error` into [`pc_errors::Error`] so HTTP / CLI layers
//!   only need to handle one error type.
//!
//! Routines are reusable "playbooks" the agent runtime can fire manually,
//! on a cron schedule, or via a public webhook.

pub mod activity_gate;
pub mod activity_gate_pure;
pub mod scheduler;
pub mod worktree_eligibility;
pub mod attention;
pub mod dashboard;
pub mod pure;
pub mod webhook_signature_pure;
pub mod dashboard_pure;
pub mod routines_validation_pure;
pub mod summary_slots;
mod service;
pub mod session_cwd;

pub use dashboard::{
    bucket_agents, bucket_tasks_v2, build_run_activity, format_utc_date_key,
    get_recent_utc_date_keys, get_utc_month_start, AgentCounts, BudgetSummary, CostSummary,
    DashboardError, DashboardResult, DashboardService, DashboardSummary, RunActivityBucket,
    DASHBOARD_RUN_ACTIVITY_DAYS,
};
pub use service::{
    CreateRoutine, CreateRoutineTrigger, NoopRoutineHook, RecordingRoutineHook, RoutineHook,
    RoutineHookEvent, RoutinePatch, RoutineService, UpdateRoutineTrigger,
};
pub use scheduler::{
    next_cron_tick, compute_catch_up, record_skipped_run, tick_scheduled_triggers,
    verify_webhook_signature, SchedulerTickOutcome, RoutineSchedulerContext,
    MAX_CATCH_UP_RUNS, SUPPRESS_REASON_PAUSED,
    SUPPRESS_REASON_WORKTREE_CUTOFF, SUPPRESS_REASON_NO_EXTERNAL_ACTIVITY,
};
pub use session_cwd::{is_unsafe_session_workspace_cwd, normalize_cwd, SESSION_CWD_SYSTEM_ROOTS};
pub use worktree_eligibility::{
    evaluate_automatic_dispatch_eligibility, is_truthy_runtime_env_value,
    resolve_automatic_dispatch_eligibility, runtime_instance_id,
    AutomaticRoutineDispatchEligibility, AutomaticRoutineSuppressionReason,
};
pub use summary_slots::{
    failure_reason_for_terminal_issue, finalize_summary_slots_for_terminal_issue,
    build_finalization_patch, build_scope_snapshot_pure, finalization_scope,
    generation_issue_description, generation_issue_idempotency_key, generation_issue_title,
    generation_version_label, is_issue_active, is_terminal_issue_status,
    recent_done_since, resolve_generation_target_project, resolve_selector, scope_issue_filter_project_id,
    scope_label, urlencoding, assert_target_visible_preconditions, DocumentFormat,
    FinalizationError, FinalizationPatch, FinalizationPlan, FinalizationResult, FinalizationScope,
    GenerationTarget, GetSummarySlotResponse, IssueSnapshotRow, IssueStatus,
    ListSummarySlotRevisionsResponse, ResolvedSelector, ScopeSnapshotInputs, SummaryGenerateActor,
    SummarySlot, SummarySlotDocument, SummarySlotError, SummarySlotIssueRef, SummarySlotKey,
    SummarySlotRevision, SummarySlotResult, SummarySlotScopeKind, SummarySlotSelectorInput,
    SummarySlotService, SummarySlotStatus, SummaryWriteActor, TerminalGenerationIssue,
    WriteSummarySlotRequest, WriteSummarySlotResponse, DEFAULT_SUMMARY_FORMAT,
    GenerateSummarySlotResponse, SUMMARY_SLOT_REVISION_LIMIT,
    SUMMARIZER_BUILT_IN_KEY,
    SUMMARY_SNAPSHOT_GROUP_LIMIT, SUMMARY_SNAPSHOT_INITIAL_LOOKBACK_MS,
    TERMINAL_ISSUE_STATUSES, summary_slot_service,
};
