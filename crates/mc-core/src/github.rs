//! GitHub App 领域类型 —— M8-0 anchor（`LUM-1797` / `docs/61-M8-PLAN.md` §2.1 / §5）。
//!
//! 本文件是 M8 里 **GitHub 一侧**的领域层：`github_installation` / `github_pull_request` /
//! `github_pull_request_check_suite` / `issue_pull_request` 四张表的行投影，外加 ghsnapshot
//! 管道的归一化快照（[`PullRequestSnapshot`]）。运行时（App JWT / token 缓存 / webhook 验签 /
//! PR 镜像）在 `mc-vcs-github`，仓储在 `mc_repos::github`，HTTP 面在
//! `mc_http::routes::github`。
//!
//! # 与 `mc_core::vcs` 的关系：**两套并列，不合并**（`docs/61` §1.6）
//!
//! GitHub 是**安装式**凭据链，VCS 是**每连接 PAT**；两者的表、事件族、授权层都不同。
//! 唯一的共享投影是「PR 行的形状」，但它们的来源列不同（`github_pull_request` 没有
//! `connection_id` / `additions`，而有 `installation_id`）⇒ **不共用一个 struct**，
//! 也不把 `VcsProviderKind` 扩成含 GitHub 的枚举。
//!
//! # 凭据纪律
//!
//! App 私钥（PEM）与 installation token **都不在本文件**：它们是 `mc-vcs-github::app`
//! 的 `secrecy` 风格不透明值（`docs/61` §2.4 的四条 redaction 判据）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;
use super::vcs::VcsPullRequestState;

/// `github_installation.account_type` 的 CHECK 两值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitHubAccountType {
    User,
    Organization,
}

impl GitHubAccountType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Organization => "Organization",
        }
    }

    /// 上游存的是 `User` / `Organization`（**首字母大写**，CHECK 约束逐字）。
    /// `from_str` 是对**存储/wire 值**的显式解析（未知值 ⇒ `None`）。
    /// 刻意**不**实现 `std::str::FromStr`：那个 trait 的错误类型会诱导调用侧用 `?`
    /// 掩盖「未知值」，而上游语义是「未知 ⇒ 不认识的 kind」，必须显式处理。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "User" => Some(Self::User),
            "Organization" => Some(Self::Organization),
            _ => None,
        }
    }
}

/// `github_installation` 行投影（`migrations/upstream/079_github_integration.up.sql`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubInstallation {
    pub id: Id,
    pub workspace_id: Id,
    /// GitHub 的 installation id（`BIGINT` ⇒ `i64`，**不是**本仓的 `Id`）。
    pub installation_id: i64,
    pub account_login: String,
    pub account_type: GitHubAccountType,
    pub account_avatar_url: Option<String>,
    pub connected_by_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `github_pull_request` 行投影（同一迁移）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequest {
    pub id: Id,
    pub workspace_id: Id,
    pub installation_id: i64,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    pub state: VcsPullRequestState,
    pub html_url: String,
    pub branch: Option<String>,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub pr_created_at: Timestamp,
    pub pr_updated_at: Timestamp,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `github_pull_request_check_suite` 行投影（`migrations/upstream/091_pr_ci_conflict.up.sql`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubCheckSuite {
    pub pr_id: Id,
    pub suite_id: i64,
    pub head_sha: String,
    pub app_id: i64,
    pub conclusion: Option<String>,
    pub status: String,
    pub updated_at: Timestamp,
}

/// `issue_pull_request` 行投影 —— issue ↔ PR 的关联账（`docs/61` §1.2 的 M8-4 面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuePrLink {
    pub issue_id: Id,
    pub pull_request_id: Id,
    pub linked_by_type: Option<String>,
    pub linked_by_id: Option<Id>,
    pub linked_at: Timestamp,
}

/// ghsnapshot 管道的**归一化**快照（上游 `ghsnapshot.PRSnapshot` 的扁平化结果）。
///
/// 这是 PR 卡片的**唯一真值**（`docs/61` §4.2 的 M8-5）：webhook 与页面访问只触发
/// **刷新**，没有任何一处从 webhook 载荷**增量推断**状态。anchor 只落形状。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestSnapshot {
    pub workspace_id: Id,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub head_sha: String,
    /// 合并可行性 / CI 状态的三态裁决（上游 `Decided`）。
    pub decision: PrSnapshotDecision,
    /// 逐 check 的扁平结果（`statusCheckRollup` 的 contexts）。
    pub checks: Vec<PrCheckContext>,
    /// 快照未决时的原因（限流 / 错误）；`Decided` 时为空。
    pub pending_reason: Option<String>,
    pub fetched_at: Timestamp,
}

/// 快照裁决三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrSnapshotDecision {
    /// 已决：CI 与可合并性都拿到了。
    Decided,
    /// 未决：GraphQL 配额耗尽 ⇒ 暂停（`RateLimitError`）。
    RateLimited,
    /// 未决：上游返回错误，等退避重试。
    Error,
}

/// 单个 check context 的扁平结果（`statusCheckRollup.contexts` 的一项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCheckContext {
    pub name: String,
    pub conclusion: Option<String>,
    pub status: String,
    pub details_url: Option<String>,
}
