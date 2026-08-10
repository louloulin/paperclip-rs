#![forbid(unsafe_code)]
//! `pc-issue-tree-control` — Issue tree hold / pause / stop / throttle / isolate
//! business service.
//!
//! 对应 Node `services/issue-tree-control.ts` (1212 行)。本 crate 在
//! `pc-repos::issue_tree_hold` 与 `pc-repos::issue` 之上提供业务级 API：
//!
//! - **预览** `preview(company_id, root_issue_id, mode, reason)`：递归
//!   列出 root issue 子树，统计 total/active/cancelled 与 hold 命中情况。
//! - **应用** `apply(company_id, root_issue_id, mode, reason, release_policy, actor)`：
//!   事务内创建 `issue_tree_holds` 行 + 遍历写入 `issue_tree_hold_members` 快照。
//! - **释放** `release(company_id, root_issue_id, hold_id, reason, actor)`：
//!   释放 active hold，幂等更新 release 元数据。
//! - **列出** `list_holds(company_id, include_released)` /
//!   `list_holds_for_root(root_issue_id)` /
//!   `find_active_for_root(root_issue_id)`。
//! - **计数** `count_active_holds(company_id)` /
//!   `count_active_holds_for_root(root_issue_id)`。
//! - **影响范围** `affected_issues(hold_id)` /
//!   `is_issue_paused(company_id, issue_id)`。
//! - **校验** `validate_mode(mode)` / `validate_release_policy(policy)`。
//!
//! 设计原则：
//! - 高内聚：所有 tree-control 业务集中在本 crate。
//! - 低耦合：上游 HTTP 路由只需调本 service 方法。
//! - 严格分层：service → repo → db，不跨层调用。
//! - Hook 副作用：通过 `IssueTreeControlHook` trait 抽象（async）。
//! - 业务校验：mode ∈ {pause, stop, throttle, isolate}；
//!   release_policy 是 `{ strategy, ... }` 形式 JSON。
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
    IssueTreeReleasePolicyStrategy, MODE_PAUSE, MODE_STOP, MODE_THROTTLE, MODE_ISOLATE,
};
pub use service::{IssueTreeControlActor, IssueTreeControlError, IssueTreeControlService};
pub use types::{
    AffectedIssue, IssueTreeAffectedCount, IssueTreeApplyResult, IssueTreeControlMode,
    IssueTreeHoldInfo, IssueTreeHoldSummary, IssueTreePreview, IssueTreePreviewWarning,
    IssueTreeReleaseResult,
};
