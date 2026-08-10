#![forbid(unsafe_code)]
//! `pc-issue-liveness` — Issue graph liveness 分类器。
//!
//! 对应 Node `services/issue-liveness.ts` + `services/recovery/issue-graph-liveness.ts`
//! (~620 行)。本 crate 是纯函数分类器：
//!
//! - **类型**：`types.rs` 提供与 Node 1:1 对齐的 input/output 类型。
//! - **incident key**：`incident_key.rs` 复刻 Node `origins.ts` 的
//!   `buildIssueGraphLivenessIncidentKey` / `parseIssueGraphLivenessIncidentKey`。
//! - **分类器**：`classifier.rs` 复刻
//!   `classifyIssueGraphLiveness` 主逻辑（DFS blocker chain、owner candidate、
//!   review participant 校验、incident key 拼接）。
//! - **服务层**：`service.rs` 提供 summary / filter / dedup 等便捷 API。
//!
//! 设计原则：
//! - **高内聚**：所有 liveness 业务集中在本 crate。
//! - **低耦合**：上游 HTTP 路由只需构造 `IssueGraphLivenessInput` 并调用 `classify`。
//! - **纯函数**：核心分类逻辑不依赖 DB / 网络，易测试。
//! - **真实测试**：e2e 测试打到真实 Postgres，加载真实 issues / agents / relations 后分类。
//!
//! ## 已复刻的 Node API
//!
//! - `classifyIssueGraphLiveness` → `classifier::classify_issue_graph_liveness` / `service::classify`
//! - `buildIssueGraphLivenessIncidentKey` → `incident_key::build_issue_graph_liveness_incident_key`
//! - `parseIssueGraphLivenessIncidentKey` → `incident_key::parse_issue_graph_liveness_incident_key`
//!
//! ## Liveness states
//!
//! - `blocked_by_unassigned_issue`
//! - `blocked_by_assigned_backlog_issue`
//! - `blocked_by_uninvokable_assignee`
//! - `blocked_by_cancelled_issue`
//! - `invalid_review_participant`
//! - `in_review_without_action_path`

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
