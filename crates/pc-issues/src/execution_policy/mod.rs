//! Issue 业务子模块（原 `pc-issue-execution-policy` 已下沉到 `pc-issues::execution_policy`）。
//!
//! 对应 Node `server/src/services/issue-execution_policy.ts`。


mod hook;
mod service;
mod types;

pub use hook::{
    IssueExecutionPolicyHook, IssueExecutionPolicyHookEvent, NoopIssueExecutionPolicyHook,
    RecordingIssueExecutionPolicyHook,
};
pub use service::IssueExecutionPolicyService;
pub use types::{
    ApplyTransitionOutcome, ApplyTransitionRequest, ClearMonitorRequest,
    IssueExecutionPolicyError, IssueExecutionPolicyResult,
    MonitorPatchOutcome, RequestedAssigneePatchDto, TriggerMonitorRequest,
    ExecutionPolicyActor, InitialMonitorRequest,
};

// Re-export pc-core key constants for convenience
pub use pc_core::{
    DEFAULT_MAX_REVIEW_ROUNDS, MONITOR_BOUNDS_EXHAUSTED_MESSAGE, MONITOR_INVALID_MESSAGE,
    STAGE_DECISION_COMMENT_HINT,
};
