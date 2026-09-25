//! PR 镜像 —— 上游 `mirrorPullRequestForWorkspace`（`github.go` L964–L1997 的核心）
//! （M8-0 anchor 建桩，**实现归 M8-4**）。
//!
//! 一次镜像要做四件事（M8-4 的 `DoD`，`docs/61` §6.5）：写 `github_pull_request` 行 →
//! 自动关联 issue（`links.rs`）→ 自动关闭决策（`closepolicy.rs`）→ 入队快照刷新
//! （`port.rs` 的 `PrRefreshPort`）。**幂等**：同一事件重投只插 1 行。
//!
//! ⚠️ 门 ⑩ 预飞把本文件排在 600–800 行（`docs/61` §6.3）⇒ 关联/关闭逻辑必须分到
//! `links.rs` / `closepolicy.rs`，**不得**全塞这里。

use mc_core::id::Id;

use crate::payload::PullRequestWebhookPayload;
use crate::rest::GithubError;

/// 一次镜像的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorRequest {
    pub installation_id: i64,
    pub workspace_id: Id,
    pub event: PullRequestWebhookPayload,
}

/// 镜像一次 PR（写行 + 关联 + 关闭决策 + 入队快照）—— **anchor 期是桩**，实现归 M8-4。
///
/// # Errors
///
/// 库写入失败 / 载荷无法映射时返回 [`GithubError`]。
pub async fn mirror_pull_request(_request: MirrorRequest) -> Result<(), GithubError> {
    todo!("M8-4：PR 镜像（docs/61 §4.1 的 M8-4 行）")
}
