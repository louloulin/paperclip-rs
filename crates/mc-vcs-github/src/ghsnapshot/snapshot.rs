//! 那一条 GraphQL 查询的解析与归一化 —— 上游 `ghsnapshot/snapshot.go`（290 行）
//! （M8-0 anchor 建桩，**实现归 M8-5**）。
//!
//! 契约（`docs/61` §4.2 的 M8-5 行）：把 `statusCheckRollup` 的 contexts 分页拿到后，
//! 归一化成扁平的逐 check 结果，并裁决 [`PrSnapshotDecision`] 三态。

use mc_core::github::PullRequestSnapshot;
use serde_json::Value;

use crate::rest::GithubError;

/// 解析 GraphQL 响应为归一化快照 —— **anchor 期是桩**，实现归 M8-5。
///
/// # Errors
///
/// 载荷形状不符合预期（缺 `data.repository.pullRequest` 等）时返回 [`GithubError`]。
pub fn parse_pr_snapshot(_graphql_response: &Value) -> Result<PullRequestSnapshot, GithubError> {
    todo!("M8-5：GraphQL 快照解析与归一化（docs/61 §4.2 / §6.5 的 M8-5 行）")
}
