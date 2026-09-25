//! GitHub webhook 载荷的分类与解码 —— 上游 `handler/github.go` 的事件分派
//! （M8-0 anchor 建桩，**实现归 M8-4**）。
//!
//! GitHub 用 `X-GitHub-Event` 头做事件分类（`installation` / `pull_request` /
//! `check_suite`），本文件把三族收成一个枚举；`pull_request` 与 `check_suite` 的
//! **原始载荷**解码也在这里（`webhook.rs` 只做验签与分发）。
//!
//! ⚠️ anchor 期是桩（`todo!()`）：这是平台 wire，**不得**在 anchor 里实现
//! （`docs/61` §5 的硬边界）。

use serde::{Deserialize, Serialize};

/// `X-GitHub-Event` 的三族 + 其它（确认但忽略，**不是**错误）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubEventKind {
    Installation,
    PullRequest,
    CheckSuite,
    /// 上游不建模的事件（`ping` / `push` / …）—— 确认后忽略。
    Other,
}

impl GithubEventKind {
    /// 从 `X-GitHub-Event` 头值分类 —— **anchor 期是桩**，实现归 M8-4。
    pub fn classify(_event_name: &str) -> Self {
        todo!("M8-4：X-GitHub-Event 分类（docs/61 §4.1 的 M8-4 行）")
    }
}

/// `pull_request` 事件的最小载荷（上游 `parseGitHubPullRequestWebhook` 的输入）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestWebhookPayload {
    pub action: String,
    pub number: i32,
    /// `open | closed | merged | draft`（解码后即归一化）。
    pub state: String,
    pub title: String,
    pub html_url: String,
    pub head_sha: String,
    pub repo_owner: String,
    pub repo_name: String,
}

/// `check_suite` 事件的最小载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSuiteWebhookPayload {
    pub action: String,
    pub suite_id: i64,
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
}

/// `installation` 事件的最小载荷（安装被创建 / 删除）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationWebhookPayload {
    pub action: String,
    pub installation_id: i64,
    pub account_login: String,
    pub account_type: String,
}
