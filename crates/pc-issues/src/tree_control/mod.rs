//! Issue 业务子模块（原 `pc-issue-tree-control` 已下沉到 `pc-issues::tree_control`）。
//!
//! 对应 Node `server/src/services/issue-tree_control.ts`。

mod hook;
mod policy;
mod service;
mod types;

pub use hook::{
    IssueTreeControlHook, IssueTreeControlHookEvent, NoopIssueTreeControlHook,
    RecordingIssueTreeControlHook,
};
pub use policy::{
    default_release_policy, parse_mode, validate_mode, validate_release_policy,
    IssueTreeReleasePolicyStrategy, MODE_ISOLATE, MODE_PAUSE, MODE_STOP, MODE_THROTTLE,
};
pub use service::{IssueTreeControlActor, IssueTreeControlError, IssueTreeControlService};
pub use types::{
    AffectedIssue, IssueTreeAffectedCount, IssueTreeApplyResult, IssueTreeControlMode,
    IssueTreeHoldInfo, IssueTreeHoldSummary, IssueTreePreview, IssueTreePreviewWarning,
    IssueTreeReleaseResult,
};
