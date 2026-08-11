//! Issue ↔ approval link business service（原 `pc-issue-approvals` 已下沉）。
mod service;
pub use pc_repos::issue_approvals::{
    ApprovalForIssueItem, IssueApprovalLinkRow, IssueForApprovalItem,
};
pub use service::{
    IssueApprovalError, IssueApprovalHook, IssueApprovalHookEvent, IssueApprovalLinkActor,
    IssueApprovalService, NoopIssueApprovalHook, RecordingIssueApprovalHook,
};
