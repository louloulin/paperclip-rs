//! Types —— IssueDependencyWakeup DTOs and constants.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Wake reason 标签（与 Node `ISSUE_BLOCKERS_RESOLVED_WAKE_REASON` 1:1 对齐）。
pub const ISSUE_BLOCKERS_RESOLVED_WAKE_REASON: &str = "issue_blockers_resolved";

/// `agent_wakeup_requests.status` 视为「已发送」的 status 集合（与 Node `IDEMPOTENT_DEPENDENCY_WAKE_STATUSES` 1:1 对齐）。
///
/// - `queued` —— 等待执行
/// - `deferred_issue_execution` —— 延迟执行
/// - `claimed` —— 已认领
/// - `completed` —— 已完成
pub const IDEMPOTENT_DEPENDENCY_WAKE_STATUSES: &[&str] = &[
    "queued",
    "deferred_issue_execution",
    "claimed",
    "completed",
];

/// 现有的 wakeup 记录（与 Node `findExistingIssueBlockersResolvedWake` 返回类型 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExistingIssueBlockersResolvedWake {
    pub id: Uuid,
    pub status: String,
    /// 仅多-key 查询时设置：命中的 idempotency key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Idempotency key 输入（与 Node `buildIssueBlockersResolvedWakeIdempotencyKey` 入参 1:1）。
#[derive(Debug, Clone)]
pub struct BuildIdempotencyKeyInput {
    pub dependent_issue_id: Uuid,
    pub resolved_blocker_issue_id: Uuid,
}

/// Find existing wake 输入（与 Node `findExistingIssueBlockersResolvedWake` 入参 1:1）。
#[derive(Debug, Clone)]
pub struct FindExistingWakeInput {
    pub company_id: Uuid,
    pub idempotency_key: String,
}

/// Find existing wake for any key 输入。
#[derive(Debug, Clone)]
pub struct FindExistingWakeForAnyKeyInput {
    pub company_id: Uuid,
    pub idempotency_keys: Vec<String>,
}
