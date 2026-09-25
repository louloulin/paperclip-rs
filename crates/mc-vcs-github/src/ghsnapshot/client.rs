//! installation-token 认证的 GitHub API 客户端 —— 上游 `ghsnapshot/client.go`（295 行）
//! （M8-0 anchor 落形状与 `api_base` 接缝，**实现归 M8-1**）。
//!
//! # `nil *Client` 是合法的「功能关闭」值
//!
//! 上游逐字：没有 App 私钥的部署必须**干净退化**（acceptance criterion 4）⇒ 本仓对应物是
//! [`Client::disabled`]（[`Client::enabled`] 返回 `false`），**不是**一个会 panic 的半成品。
//!
//! # `api_base` 是离线替身的接缝（R-M8-1）
//!
//! 上游 `client.go:40` 的 `defaultAPIBase = "https://api.github.com"` 与
//! `client.go:64-65` 的 `apiBase` 字段 —— 本地 GraphQL 替身按此注入。

/// GitHub GraphQL/REST 客户端（App 凭据链 + token 缓存）。
///
/// ⚠️ anchor 期只落字段与构造；`fetch_pr_snapshot` 等方法是桩（实现归 M8-1 / M8-5）。
pub struct Client {
    /// `GITHUB_APP_ID`（JWT 的 `iss`）；未配置为 `None`。
    pub(crate) app_id: Option<String>,
    /// App 私钥 PEM（**绝不**进 `Debug`）；未配置为 `None`。
    pub(crate) private_key_pem: Option<String>,
    /// REST / GraphQL base（替身接缝）。
    pub(crate) api_base: String,
}

impl Client {
    /// 默认 base（上游 `defaultAPIBase`）。
    pub const DEFAULT_API_BASE: &'static str = "https://api.github.com";

    /// 从 App 配置构造（`api_base` 取默认）。
    pub fn new(app_id: Option<String>, private_key_pem: Option<String>) -> Self {
        Self {
            app_id,
            private_key_pem,
            api_base: Self::DEFAULT_API_BASE.to_string(),
        }
    }

    /// 「功能关闭」值（上游 `nil *Client` 的语义）：所有方法容忍它。
    pub fn disabled() -> Self {
        Self::new(None, None)
    }

    /// 自定义 base（**测试/替身唯一入口**）。
    #[must_use]
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    /// App 凭据是否配置齐（上游 `Enabled()`：nil 或私钥缺失 ⇒ `false`）。
    pub fn enabled(&self) -> bool {
        self.app_id.is_some() && self.private_key_pem.is_some()
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// 拉一次 PR 快照（GraphQL）—— **anchor 期是桩**，实现归 M8-1 / M8-5。
    pub async fn fetch_pr_snapshot(
        &self,
        _repo_owner: &str,
        _repo_name: &str,
        _pr_number: i32,
    ) -> Result<mc_core::github::PullRequestSnapshot, crate::rest::GithubError> {
        todo!("M8-1/M8-5：GraphQL 快照查询（docs/61 §4.2 的 M8-5 行）")
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ghsnapshot::Client")
            .field("app_id", &self.app_id)
            .field(
                "private_key_pem",
                &self.private_key_pem.as_ref().map(|_| "<redacted>"),
            )
            .field("api_base", &self.api_base)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_client_degrades_and_base_is_overridable() {
        let disabled = Client::disabled();
        assert!(!disabled.enabled());
        assert_eq!(disabled.api_base(), "https://api.github.com");
        let seam = Client::disabled().with_api_base("http://127.0.0.1:9/api");
        assert_eq!(seam.api_base(), "http://127.0.0.1:9/api");
        assert!(!format!("{seam:?}").contains("BEGIN PRIVATE KEY"));
    }
}
