//! GitHub 面的响应 DTO（上游 `githubInstallationToResponse` / `githubPullRequestToResponse`，
//! `github.go` L163–L330）。
//!
//! ⚠️ 本文件的写者是 **M8-1**，**M8-4 只读**（`docs/61` §1.2 的写者/读者两列）：两片共用
//! 这一份响应映射，不得各造一份。anchor 落的是**完整形状**（切片只填函数体）。
//!
//! # write-only / 脱敏
//!
//! DTO 里**不得**出现 installation token、App 私钥、webhook secret
//! （`docs/61` §2.4 的四条判据）。`installation_id` 是 GitHub 的公开数字标识，可以出。

use serde::{Deserialize, Serialize};

/// `GET /api/workspaces/{id}/github/installations` 的单条（上游 `githubInstallationToResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubInstallationResponse {
    pub id: String,
    pub installation_id: i64,
    pub account_login: String,
    pub account_type: String,
    pub account_avatar_url: Option<String>,
    pub connected_by_id: Option<String>,
    pub created_at: String,
}

/// `GET /api/workspaces/{id}/github/connect` 的响应（含「未配置」语义的两格，`docs/61` §2.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubConnectResponse {
    /// 可直接跳转的安装引导 URL；未配置时为 `None`。
    pub install_url: Option<String>,
    /// 是否配置了 App（前端据此隐藏按钮）—— **200 + `configured:false`**，不是 503。
    pub configured: bool,
}

/// `GET .../installations/{installationId}/repositories` 的单条。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubRepositoryResponse {
    pub id: i64,
    pub full_name: String,
    pub name: String,
    pub owner: String,
    pub private: bool,
    pub default_branch: Option<String>,
    pub html_url: String,
}

/// `GET /api/issues/{id}/pull-requests` 的单条（上游 `githubPullRequestToResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubPullRequestResponse {
    pub id: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 分页参数解析的结果（上游 `parseGitHubPageParam` 的边界，M8-1 的 `DoD` 点名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GithubPageParam {
    /// 已归一化的页号（≥1）。
    pub page: u32,
    /// 已归一化的每页条数（≤100）。
    pub per_page: u32,
}

impl GithubPageParam {
    /// GitHub 的每页上限（上游硬限 100）。
    pub const MAX_PER_PAGE: u32 = 100;

    /// 从 query 的两个可选字符串解析 —— **anchor 期是桩**，实现归 M8-1。
    ///
    /// 契约：非法 / 缺失 / 越界都要归一到安全值，**不得** panic，**不得**透传 `0`。
    pub fn parse(_page: Option<&str>, _per_page: Option<&str>) -> Self {
        todo!("M8-1：parseGitHubPageParam 的边界（docs/61 §4.1 的 M8-1 行）")
    }
}
