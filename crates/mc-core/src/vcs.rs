//! VCS（token 型自建 Git）领域类型 —— M8-0 anchor（`LUM-1797` / `docs/61-M8-PLAN.md` §2.1 / §5）。
//!
//! 本文件是 M8 里 **VCS connection 一侧**的领域层：provider 枚举 + `vcs_connection` /
//! `vcs_pull_request` / `vcs_commit_status` 三张表的行投影。运行时（`Provider` trait /
//! registry / forgejo 与 gitlab 两个 adapter）在 `mc-vcs`，仓储在 `mc_repos::vcs`，
//! HTTP 面在 `mc_http::routes::vcs`。**三者都引用本文件**，所以跨 crate 的枚举与投影
//! 只在这里定义一次。
//!
//! # GitHub 不在这里（结构性裁定，`docs/61` §1.6）
//!
//! GitHub App 是**安装式**凭据（App 私钥 → App JWT → installation token），且上游自己
//! 都没把它塞进 `integrations/vcs` 的 registry ⇒ GitHub 的类型在
//! [`crate::github`]，**不得**把 `VcsProviderKind::GitHub` 加进来。
//!
//! # 三个字符串口径（不要把 `provider` 的显示名与存储值混起来）
//!
//! | 口径 | 出处 | `Forgejo` 的取值 |
//! | --- | --- | --- |
//! | [`VcsProviderKind::as_str`] | `vcs_connection.provider` 的 CHECK 约束、路由前缀 | `forgejo` |
//! | [`VcsProviderKind::display_name`] | 前端展示 | `Forgejo` |
//!
//! 两个函数**都不 trim、不 fold**：上游 `Kind` 是 `string` 别名，存储值即 wire 值
//! （`internal/integrations/vcs/vcs.go` 的三个常量）⇒ 未知值一律 [`VcsProviderKind::from_str`]
//! 返回 `None`，由调用侧翻译成 `ErrUnknownProvider`。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// 自建 Git provider 的种类（上游 `integrations/vcs.Kind` 的三个常量）。
///
/// 注意 `Gitea` 与 `Forgejo` 是**两个** kind（wire 同一套、registry 也各注一份），
/// 上游刻意保留两个键（`docs/61` §1.1 的 `provider` CHECK 约束）。
/// `Ord`/`PartialOrd` 不是装饰：`Registry` 用 `BTreeMap<VcsProviderKind, _>` 保证
/// `kinds()` 的**确定序**（`docs/61` §2.1 的确定性要求）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsProviderKind {
    Forgejo,
    Gitea,
    GitLab,
}

impl VcsProviderKind {
    /// 本仓支持的全部 provider（**不含** GitHub，见模块文档）。
    pub const BUILTIN: [VcsProviderKind; 3] = [Self::Forgejo, Self::Gitea, Self::GitLab];

    /// 存储值 / wire 值（`vcs_connection.provider` 列与 registry 的键）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forgejo => "forgejo",
            Self::Gitea => "gitea",
            Self::GitLab => "gitlab",
        }
    }

    /// 前端展示名（只有大小写差异，**不是**存储值）。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Forgejo => "Forgejo",
            Self::Gitea => "Gitea",
            Self::GitLab => "GitLab",
        }
    }

    /// 从存储值解析；未知值返回 `None`（调用侧翻成 `ErrUnknownProvider`）。
    /// `from_str` 是对**存储/wire 值**的显式解析（未知值 ⇒ `None`）。
    /// 刻意**不**实现 `std::str::FromStr`：那个 trait 的错误类型会诱导调用侧用 `?`
    /// 掩盖「未知值」，而上游语义是「未知 ⇒ 不认识的 kind」，必须显式处理。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "forgejo" => Some(Self::Forgejo),
            "gitea" => Some(Self::Gitea),
            "gitlab" => Some(Self::GitLab),
            _ => None,
        }
    }
}

/// PR 状态（`vcs_pull_request.state` 的 CHECK 四值，与 GitHub 侧同形）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsPullRequestState {
    Open,
    Closed,
    Merged,
    Draft,
}

impl VcsPullRequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
            Self::Draft => "draft",
        }
    }

    /// `from_str` 是对**存储/wire 值**的显式解析（未知值 ⇒ `None`）。
    /// 刻意**不**实现 `std::str::FromStr`：那个 trait 的错误类型会诱导调用侧用 `?`
    /// 掩盖「未知值」，而上游语义是「未知 ⇒ 不认识的 kind」，必须显式处理。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "merged" => Some(Self::Merged),
            "draft" => Some(Self::Draft),
            _ => None,
        }
    }

    /// 终态（merge / close）—— 自动关闭决策在此之后必须冻结（对照 `docs/61` §4.1 的 M8-4）。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Merged)
    }
}

/// `vcs_connection` 行投影（`migrations/upstream/216_vcs_integration.up.sql`）。
///
/// ⚠️ `access_token_encrypted` / `webhook_secret_encrypted` **不在**本投影里：它们是
/// `secretbox` 密文，只经 `mc_secrets::secretbox` 出库，领域层**不持有**（`docs/61` §2.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsConnection {
    pub id: Id,
    pub workspace_id: Id,
    pub provider: VcsProviderKind,
    pub instance_url: String,
    pub account_login: String,
    pub connected_by_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `vcs_pull_request` 行投影（同一迁移）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsPullRequest {
    pub id: Id,
    pub workspace_id: Id,
    pub connection_id: Id,
    pub provider: VcsProviderKind,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    pub state: VcsPullRequestState,
    pub html_url: String,
    pub branch: Option<String>,
    pub head_sha: String,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub pr_created_at: Timestamp,
    pub pr_updated_at: Timestamp,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `vcs_commit_status` 行投影（CI 状态镜像；主键是 `(connection_id, sha, context)`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsCommitStatus {
    pub connection_id: Id,
    pub sha: String,
    pub context: String,
    pub state: VcsCommitState,
    pub target_url: Option<String>,
    pub description: Option<String>,
    pub updated_at: Timestamp,
}

/// CI 状态归一化三态（上游 `CIStatusEvent.State` 的 `passed | failed | pending`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsCommitState {
    Passed,
    Failed,
    Pending,
}

impl VcsCommitState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Pending => "pending",
        }
    }

    /// `from_str` 是对**存储/wire 值**的显式解析（未知值 ⇒ `None`）。
    /// 刻意**不**实现 `std::str::FromStr`：那个 trait 的错误类型会诱导调用侧用 `?`
    /// 掩盖「未知值」，而上游语义是「未知 ⇒ 不认识的 kind」，必须显式处理。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "passed" => Some(Self::Passed),
            "failed" => Some(Self::Failed),
            "pending" => Some(Self::Pending),
            _ => None,
        }
    }
}
