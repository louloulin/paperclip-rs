//! 归一化的 webhook 事件类型 —— 上游 `internal/integrations/vcs/vcs.go` 的逐字复刻
//! （M8-0 anchor 建文件与形状，**填充归 M8-2**）。
//!
//! 三个类型的角色（`docs/61` §1.6）：
//! - [`EventKind`]：归一化的事件类别，provider 不建模的一律映射到 [`EventKind::Other`]
//!   （**确认但忽略**，不是错误）；
//! - [`PullRequestEvent`]：provider 无关的 PR/merge request 载荷，`state` **已经**归一化到
//!   `open | closed | merged | draft`，handler 不再重新推导；
//! - [`CIStatusEvent`]：commit-status / pipeline 载荷，`state` 归一化为
//!   `passed | failed | pending`，让聚合查询与 provider 无关。
//!
//! ⚠️ 本文件的类型**只描述形状**：解析（`ParsePullRequest` / `ParseCIStatus`）在
//! `forgejo.rs` / `gitlab.rs`，由 M8-2 实现。

use mc_core::vcs::VcsProviderKind;
use serde::{Deserialize, Serialize};

/// 归一化的事件类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// provider 不建模的事件（确认后忽略）。
    Other,
    PullRequest,
    CIStatus,
}

/// provider 无关的 PR / merge request 事件（上游 `PullRequestEvent`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestEvent {
    /// provider 的**原始** action（`opened` / `closed` / `merge` / …）。handler 只需要知道
    /// 它是不是终态（见 [`PullRequestEvent::is_terminal`]）。
    pub action: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub number: i32,
    pub title: String,
    pub body: String,
    /// `open | closed | merged | draft`（已归一化）。
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub head_sha: String,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
    /// RFC3339 或空串。
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl PullRequestEvent {
    /// 本事件是否是 PR 的 merge/close 事件 —— 此后 close-intent 决策必须**冻结**。
    ///
    /// provider 对终态 action 的拼法不同（Forgejo `closed`/`merged`、GitLab
    /// `merge`/`close`），所以这套集合在这里匹配，而不是散在 handler 里。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.action.as_str(),
            "closed" | "merged" | "merge" | "close"
        )
    }
}

/// provider 无关的 commit-status / pipeline 事件（上游 `CIStatusEvent`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CIStatusEvent {
    pub sha: String,
    /// status check / pipeline 名（允许空串）。
    pub context: String,
    /// `passed | failed | pending`。
    pub state: String,
    pub target_url: Option<String>,
    pub description: Option<String>,
    /// provider 自己的事件时间戳（RFC3339 或空串）。它喂给 commit-status 的**单调守卫**，
    /// 让乱序重投递不能把状态回退；空串 = 未知，handler 退回摄入时间。
    pub updated_at: Option<String>,
}

/// `ValidateToken` 返回的最小身份（上游 `Account`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub login: String,
    /// provider 种类 —— 本仓补的字段（上游 `Account` 只有 `Login`），便于调用侧记录来源。
    pub kind: VcsProviderKind,
}
