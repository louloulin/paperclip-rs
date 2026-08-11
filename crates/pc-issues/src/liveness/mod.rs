//! Issue 业务子模块（原 `pc-issue-liveness` 已下沉到 `pc-issues::liveness`）。
//!
//! 对应 Node `server/src/services/issue-liveness.ts`。


mod classifier;
mod incident_key;
mod service;
mod types;

pub use classifier::classify_issue_graph_liveness;
pub use incident_key::{
    build_issue_graph_liveness_incident_key, parse_issue_graph_liveness_incident_key,
    IncidentKeyInput, ParsedIncidentKey, ISSUE_GRAPH_LIVENESS_INCIDENT_PREFIX,
};
pub use service::{
    build_incident_key, classify, dedup_by_incident_key, filter_by_company, filter_by_issue,
    filter_by_state, make_issue_input, owner_reason_str, parse_incident_key, summarize,
    IssueLivenessError, IssueLivenessResult, IssueLivenessSummary,
};
pub use types::{
    IssueGraphLivenessInput, IssueLivenessAgentInput, IssueLivenessDependencyPathEntry,
    IssueLivenessExecutionPathInput, IssueLivenessFinding, IssueLivenessIssueInput,
    IssueLivenessOwnerCandidate, IssueLivenessOwnerCandidateReason, IssueLivenessRelationInput,
    IssueLivenessSeverity, IssueLivenessState, IssueLivenessWaitingPathInput,
};
