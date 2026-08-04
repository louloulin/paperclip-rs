//! `recovery` 子模块门面：heartbeat 周期内 issue 恢复相关分类、键构造、状态判定。
//!
//! 对齐 Node `services/recovery/`：
//! - `origins` —— origin / reason / key 前缀常量 + 强类型枚举 + key 构建解析
//!
//! 后续可在此目录下追加更多恢复子模块（如 `pause_hold_guard` / `run_liveness_continuations` 等）。

pub mod model_profile_hint;
pub mod origins;
pub mod run_liveness_continuations;

pub use origins::{
    build_issue_graph_liveness_incident_key, build_issue_graph_liveness_leaf_key,
    is_stranded_issue_recovery_origin_kind, parse_issue_graph_liveness_incident_key,
    IncidentKeyInput, LeafKeyInput, ParsedIncidentKey, RecoveryKeyPrefix, RecoveryOriginKind,
    RecoveryReasonKind,
};
pub use run_liveness_continuations::{
    build_run_liveness_continuation_idempotency_key, decide_run_liveness_continuation,
    read_continuation_attempt, AgentRef, DecideRunLivenessContinuationInput, HeartbeatRunRef,
    IdempotencyKeyInput, IssueRef, RunContinuationDecision, ACTIONABLE_LIVENESS_STATES,
    CONTINUATION_ACTIVE_ISSUE_STATUSES, CONTINUATION_AGENT_STATUSES,
    DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS, IDEMPOTENT_WAKE_STATUSES,
    RUN_LIVENESS_CONTINUATION_REASON,
};
pub use model_profile_hint::{
    recovery_assignee_adapter_overrides, scrub_recovery_model_profile_hints,
    with_recovery_model_profile_hint, RecoveryAssigneeAdapterOverrides,
    RecoveryModelProfileWorkClass, RECOVERY_MODEL_PROFILE_HINT_KEYS,
    RECOVERY_MODEL_PROFILE_KEY, status_only_recovery_guard_context,
};
