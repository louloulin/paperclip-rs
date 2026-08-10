#![forbid(unsafe_code)]
//! Issue ↔ approval link business service.
mod service;
pub use pc_repos::issue_approvals::{
    ApprovalForIssueItem, IssueApprovalLinkRow, IssueForApprovalItem,
};
pub use service::{
    IssueApprovalError, IssueApprovalHook, IssueApprovalHookEvent, IssueApprovalLinkActor,
    IssueApprovalService, NoopIssueApprovalHook, RecordingIssueApprovalHook,
};
