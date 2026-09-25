//! GitHub REST 客户端 —— 上游 `handler/github.go` 的 REST 面（分页/仓库/撤销 installation）
//! （M8-0 anchor 建桩，**实现归 M8-1**）。
//!
//! # `api_base` 是**离线替身的关键接缝**（R-M8-1，`docs/61` §4.2）
//!
//! 上游 `github.go:34-36` 有一个**可写包级变量** `var githubAPIBase = "https://api.github.com"`，
//! 注释逐字「Mutable so tests can…」。本仓对应物是 [`GithubClient::api_base`] ——
//! 本地 HTTP 替身按 REST 形状答 `/app/installations/{id}/access_tokens`、
//! `/installation/repositories`，端到端断言链不需要真连 GitHub。

use std::time::Duration;

use serde::Deserialize;

/// GitHub 面统一错误（**不得**含 App 私钥 / installation token / webhook secret）。
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    /// HTTP 传输层失败。
    #[error("github: transport error: {0}")]
    Transport(String),
    /// 401/403（installation 被撤销 / token 失效）。
    #[error("github: unauthorized")]
    Unauthorized,
    /// 限流：`Retry-After`（秒）由上游给出或取保守默认值。
    #[error("github: rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: i64 },
    /// 载荷形状不符合预期。
    #[error("github: malformed payload: {0}")]
    Malformed(String),
    /// 该功能未配置（缺 App 私钥 ⇒ feature disabled，**不是**错误，但用同一通道回落）。
    #[error("github: not configured")]
    NotConfigured,
}

/// installation-token 认证的 GitHub REST 客户端。
///
/// ⚠️ anchor 期所有方法 `todo!()`（实现归 M8-1）。
pub struct GithubClient {
    /// REST base（替身接缝）。
    pub(crate) api_base: String,
    /// 复用连接池的 HTTP 客户端（M8-1 的所有调用都用它；anchor 期还没被读）。
    #[allow(dead_code)]
    pub(crate) http: reqwest::Client,
}

impl GithubClient {
    /// 构造（`api_base` 不 trailing slash）。
    pub fn new(api_base: impl Into<String>) -> Self {
        Self {
            api_base: api_base.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
        }
    }

    /// 默认 base（上游 `https://api.github.com`）。
    pub fn with_default_base() -> Self {
        Self::new(crate::port::GithubAppConfig::DEFAULT_API_BASE)
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// 用 App JWT 换 installation access token（上游 `POST /app/installations/{id}/access_tokens`）。
    ///
    /// # Errors
    ///
    /// 传输失败 / 401 / 载荷非法时返回 [`GithubError`]。
    pub async fn exchange_installation_token(
        &self,
        _app_jwt: &str,
        _installation_id: i64,
    ) -> Result<ExchangedInstallationToken, GithubError> {
        todo!("M8-1：POST {{api_base}}/app/installations/{{id}}/access_tokens（docs/61 §4.1 的 M8-1 行）")
    }
}

impl std::fmt::Debug for GithubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubClient")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// token 交换的响应载荷（上游 `access_tokens` 的 `{token, expires_at}`）。
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangedInstallationToken {
    /// ⚠️ 手写脱敏见 [`ExchangedInstallationToken::Debug`]。
    pub token: String,
    /// RFC3339 过期时刻。
    pub expires_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_base_defaults_to_github_and_is_overridable() {
        assert_eq!(
            GithubClient::with_default_base().api_base(),
            "https://api.github.com"
        );
        assert_eq!(
            GithubClient::new("http://127.0.0.1:9").api_base(),
            "http://127.0.0.1:9"
        );
    }
}
