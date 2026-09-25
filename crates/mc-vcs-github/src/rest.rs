//! GitHub REST 客户端 —— 上游 `handler/github.go` 的 REST 面（App JWT → installation
//! token → 仓库分页 / 撤销）（**写者 M8-1**，`docs/61-M8-PLAN.md` §3.3 / §4.1）。
//!
//! # `api_base` 是**离线替身的关键接缝**（R-M8-1，`docs/61` §4.2）
//!
//! 上游 `github.go:34-36` 有一个**可写包级变量** `var githubAPIBase = "https://api.github.com"`，
//! 注释逐字「Mutable so tests can…」。本仓对应物是 [`GithubClient::api_base`] ——
//! 本地 HTTP 替身按 REST 形状答 `/app/installations/{id}`、
//! `/app/installations/{id}/access_tokens`、`/installation/repositories`、`/installation/token`，
//! 端到端断言链（路由 → App JWT → token 交换 → 分页列表 → 真库）**不需要真连 GitHub**。
//!
//! # 与 [`crate::ghsnapshot::Client`] 的分工（**不是两份实现**）
//!
//! | | `rest::GithubClient` | `ghsnapshot::Client` |
//! | --- | --- | --- |
//! | 凭据来源 | 调用方给（App JWT / installation token 实参） | 自己从 `app_id` + PEM 签 JWT |
//! | token 生命周期 | **不留缓存**：按上游 browse 路径每次交换、用完即撤销 | **缓存 + 单飞**（`token_cache`），worker 池共用 |
//! | 上游对应 | `fetchGitHubInstallationRepositories` / `revokeGitHubInstallationToken` | `ghsnapshot/client.go` |
//!
//! 两条路径**不共用 token**：browse 路径按上游语义在 `defer` 里撤销刚换来的 token
//! （`github.go:863`），把它写进共享缓存会立刻毒化缓存里的那一枚。所以本文件**不**缓存，
//! 缓存与单飞只落在 [`crate::token_cache`]，由 ghsnapshot 的 `Client` 持有。
//!
//! # 凭据纪律（`docs/61` §2.4 的四条判据）
//!
//! App 私钥与 installation token **都不派生 `Debug`**（手写脱敏）；错误消息只带 **HTTP 状态码**
//! （上游逐字注释：「Never echo the body — a token-mint failure body can contain sensitive hints」）。

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value as JsonValue;

/// 上游 `githubAPIResponseLimit = 4 << 20`（响应体上限）。
pub const API_RESPONSE_LIMIT: usize = 4 << 20;

/// 上游 browse 路径的 `http.Client{Timeout: 15 * time.Second}`。
pub const BROWSE_TIMEOUT_SECS: u64 = 15;

/// App JWT 认证路径的超时（上游 `fetchInstallationAccount` 用无超时的
/// `http.DefaultClient`；本仓给一个上限，登记在 `docs/32` §9.12）。
pub const DEFAULT_TIMEOUT_SECS: u64 = 20;

/// 上游 `setGitHubAPIHeaders` 的 `Accept` 字面量。
const ACCEPT_JSON: &str = "application/vnd.github+json";

/// 上游 `setGitHubAPIHeaders` 的 `X-GitHub-Api-Version` 字面量。
const API_VERSION: &str = "2022-11-28";

/// GitHub 面统一错误（**不得**含 App 私钥 / installation token / webhook secret）。
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    /// HTTP 传输层失败。
    #[error("github: transport error: {0}")]
    Transport(String),
    /// 401（installation 被撤销 / token 失效）。
    #[error("github: unauthorized")]
    Unauthorized,
    /// 限流 / 配额耗尽：`Retry-After`（秒）由上游给出或取保守默认值。
    #[error("github: rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: i64 },
    /// 载荷形状不符合预期。
    #[error("github: malformed payload: {0}")]
    Malformed(String),
    /// 非 200/201/401/403/429 的状态码（**只带状态码**，不回显响应体）。
    #[error("github: unexpected status {0}")]
    UnexpectedStatus(u16),
    /// 该功能未配置（缺 App 私钥 ⇒ feature disabled，**不是**错误，但用同一通道回落）。
    #[error("github: not configured")]
    NotConfigured,
}

/// App JWT 认证的 GitHub REST 客户端（**零 token 缓存**，见模块头）。
pub struct GithubClient {
    /// REST base（替身接缝）。
    api_base: String,
    /// 复用连接池的 HTTP 客户端。
    http: reqwest::Client,
}

impl GithubClient {
    /// 构造（`api_base` 一般不带 trailing slash；调用侧统一 `TrimRight` 后再用）。
    pub fn new(api_base: impl Into<String>) -> Self {
        Self {
            api_base: api_base.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .build()
                .unwrap_or_default(),
        }
    }

    /// 默认 base（上游 `githubAPIBase` 的字面量）。
    pub fn with_default_base() -> Self {
        Self::new(crate::port::GithubAppConfig::DEFAULT_API_BASE)
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// `strings.TrimRight(base, "/")` 的等价物（上游每处调用都做一次）。
    fn trimmed_base(&self) -> &str {
        self.api_base.trim_end_matches('/')
    }

    /// 上游 `setGitHubAPIHeaders(req, token)`。
    fn api_headers(token: &str, with_content_type: bool) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static(ACCEPT_JSON),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-github-api-version"),
            reqwest::header::HeaderValue::from_static(API_VERSION),
        );
        // token 一定来自 hex/base64url 之外的字符集？不一定 —— 用 HeaderValue::from_str
        // 失败时退化成不带 Authorization（宁可 401 也不要 header 注入）。
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        if with_content_type {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
        }
        headers
    }

    /// 用 App JWT 换 installation access token
    /// （上游 `POST {base}/app/installations/{id}/access_tokens`，期望 **201**）。
    ///
    /// 请求体逐字照上游：`{"permissions":{"metadata":"read"}}`。
    ///
    /// # Errors
    ///
    /// 传输失败 / 非 201 / 载荷非法 / token 为空 ⇒ [`GithubError`]。
    pub async fn exchange_installation_token(
        &self,
        app_jwt: &str,
        installation_id: i64,
    ) -> Result<ExchangedInstallationToken, GithubError> {
        let endpoint = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.trimmed_base()
        );
        let resp = self
            .http
            .post(&endpoint)
            .headers(Self::api_headers(app_jwt, true))
            .body(r#"{"permissions":{"metadata":"read"}}"#)
            .send()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        if status == 401 {
            return Err(GithubError::Unauthorized);
        }
        if status == 403 || status == 429 {
            return Err(GithubError::RateLimited {
                retry_after_secs: retry_after_secs(&headers, now_unix()),
            });
        }
        if status != 201 {
            return Err(GithubError::UnexpectedStatus(status));
        }
        let body: ExchangedInstallationToken = read_json(resp).await?;
        if body.token.is_empty() {
            return Err(GithubError::Malformed(
                "github returned an empty installation token".into(),
            ));
        }
        Ok(body)
    }

    /// 读一次 `GET {base}/app/installations/{id}` 取账号展示信息
    /// （上游 `fetchInstallationAccount`）。
    ///
    /// **永不失败**：上游明确要求「网络抖动只留下 `unknown` 占位，不得中断安装回调」，
    /// 所以任何失败都回落到 [`InstallationAccount::unknown`]，只有 JWT 签名失败会打
    /// 一条 WARN 面包屑（私钥配错是运维可行动的）。
    pub async fn fetch_installation_account(
        &self,
        app_jwt: Option<&str>,
        installation_id: i64,
    ) -> InstallationAccount {
        let endpoint = format!(
            "{}/app/installations/{installation_id}",
            self.trimmed_base()
        );
        let mut headers = Self::api_headers(app_jwt.unwrap_or(""), false);
        if app_jwt.is_none() {
            // 没配 App 身份 ⇒ 不带 Authorization（上游 `token == ""` 分支）。
            headers.remove(reqwest::header::AUTHORIZATION);
        }
        let resp = match self.http.get(&endpoint).headers(headers).send().await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "github: fetch installation account failed");
                return InstallationAccount::unknown();
            }
        };
        if resp.status().as_u16() != 200 {
            return InstallationAccount::unknown();
        }
        match read_json::<InstallationEnvelope>(resp).await {
            Ok(body) => InstallationAccount::from_envelope(body),
            Err(_) => InstallationAccount::unknown(),
        }
    }

    /// 列出一个 installation 可访问的仓库（上游 `fetchGitHubInstallationRepositories`
    /// 的 GET 段 + `parseGitHubPageParam` 算出的分页）。
    ///
    /// # Errors
    ///
    /// 传输失败 / 非 200 / 载荷非法 ⇒ [`GithubError`]（调用侧翻 **502**
    /// `failed to list github repositories`，与上游逐字一致）。
    pub async fn list_installation_repositories(
        &self,
        token: &str,
        page: u32,
        per_page: u32,
    ) -> Result<RepositoryPage, GithubError> {
        let endpoint = format!(
            "{}/installation/repositories?page={page}&per_page={per_page}",
            self.trimmed_base()
        );
        let resp = self
            .http
            .get(&endpoint)
            .headers(Self::api_headers(token, false))
            .send()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if status == 401 {
            return Err(GithubError::Unauthorized);
        }
        if status != 200 {
            return Err(GithubError::UnexpectedStatus(status));
        }
        let mut body: RepositoryPage = read_json(resp).await?;
        // 上游的 next_page 只由 `page * per_page < total_count` 决定（不看 Link 头）。
        body.next_page = if i64::from(page) * i64::from(per_page) < body.total_count {
            page.checked_add(1)
        } else {
            None
        };
        Ok(body)
    }

    /// **尽力而为**地撤销一枚 installation token（上游 `revokeGitHubInstallationToken`）。
    ///
    /// 上游把它放在 `defer` 里且**忽略一切失败**（撤销失败不回滚已完成的列表响应，
    /// 也不影响删除安装行）。本仓保留同一个语义：返回值只供测试断言，调用侧一律 `let _ =`。
    ///
    /// # Errors
    ///
    /// 传输失败 / 非 204 / 非 401 状态 ⇒ [`GithubError`]（调用侧忽略）。
    pub async fn revoke_installation_token(&self, token: &str) -> Result<(), GithubError> {
        let endpoint = format!("{}/installation/token", self.trimmed_base());
        // 上游给这个调用单独一个 5s 超时（`context.WithTimeout(..., 5*time.Second)`）。
        let resp = self
            .http
            .delete(&endpoint)
            .headers(Self::api_headers(token, false))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if status == 204 || status == 200 {
            Ok(())
        } else if status == 401 {
            Err(GithubError::Unauthorized)
        } else {
            Err(GithubError::UnexpectedStatus(status))
        }
    }

    /// 发一次 GraphQL 查询（installation token 认证），返回信封里的 `data` 对象。
    ///
    /// GitHub 对**查询级**错误也回 200，所以除了 HTTP 状态还要看 `errors` 数组：
    /// `type == "RATE_LIMITED"` ⇒ [`GithubError::RateLimited`]（上游 `graphQL` 的口径）。
    ///
    /// # Errors
    ///
    /// 传输失败 / 非 200 / `errors` 非空 / `data` 缺失 ⇒ [`GithubError`]。
    pub async fn graph_ql(
        &self,
        token: &str,
        query: &str,
        variables: &JsonValue,
    ) -> Result<JsonValue, GithubError> {
        let endpoint = format!("{}/graphql", self.trimmed_base());
        let payload = serde_json::json!({ "query": query, "variables": variables });
        let resp = self
            .http
            .post(&endpoint)
            .headers(Self::api_headers(token, true))
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        if status == 403 || status == 429 {
            return Err(GithubError::RateLimited {
                retry_after_secs: retry_after_secs(&headers, now_unix()),
            });
        }
        if status != 200 {
            return Err(GithubError::UnexpectedStatus(status));
        }
        let envelope: GraphQlEnvelope = read_json(resp).await?;
        if !envelope.errors.is_empty() {
            // GraphQL 错误消息不含凭据；但只回显**首条消息**，其余计数（与上游一致）。
            if envelope.errors.iter().any(|e| e.kind == "RATE_LIMITED") {
                return Err(GithubError::RateLimited {
                    retry_after_secs: 60,
                });
            }
            return Err(GithubError::Malformed(format!(
                "github graphql error: {}",
                envelope.errors[0].message
            )));
        }
        match envelope.data {
            Some(data) if !data.is_null() => Ok(data),
            _ => Err(GithubError::Malformed("github graphql: empty data".into())),
        }
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
///
/// ⚠️ 手写 `Debug`：**绝不**打印 token 字节（anchor 期误派生了 `Debug`，
/// M8-1 修，登记 `docs/32` §9.12）。
#[derive(Clone, Deserialize)]
pub struct ExchangedInstallationToken {
    /// 裸 token（唯一出口是 [`ExchangedInstallationToken::expose`]）。
    pub token: String,
    /// RFC3339 过期时刻（GitHub 恒给；缺失时调用侧按 1 小时兜底）。
    #[serde(default)]
    pub expires_at: String,
}

impl ExchangedInstallationToken {
    /// 取裸 token（交给 `Authorization` 头）。**不要**把它塞进任何日志/错误。
    pub fn expose(&self) -> &str {
        &self.token
    }
}

impl std::fmt::Debug for ExchangedInstallationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExchangedInstallationToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// `GET /app/installations/{id}` 的账号信息（上游 `fetchInstallationAccount` 的三元组）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationAccount {
    pub login: String,
    pub account_type: String,
    pub avatar_url: Option<String>,
}

impl InstallationAccount {
    /// 上游的回落占位：`login="unknown"` / `account_type="User"` / 无头像。
    ///
    /// `account_type` 必须是表 CHECK 的两值之一（`User` / `Organization`），
    /// 所以这里**不能**给空串。
    pub fn unknown() -> Self {
        Self {
            login: "unknown".into(),
            account_type: "User".into(),
            avatar_url: None,
        }
    }

    fn from_envelope(envelope: InstallationEnvelope) -> Self {
        let account = envelope.account;
        Self {
            login: if account.login.is_empty() {
                "unknown".into()
            } else {
                account.login
            },
            account_type: if account.kind.is_empty() {
                "User".into()
            } else {
                account.kind
            },
            avatar_url: account.avatar_url.filter(|v| !v.is_empty()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct InstallationEnvelope {
    #[serde(default)]
    account: InstallationAccountBody,
}

#[derive(Debug, Default, Deserialize)]
struct InstallationAccountBody {
    #[serde(default)]
    login: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    avatar_url: Option<String>,
}

/// `GET /installation/repositories` 的分页响应（上游 `GitHubRepositoriesResponse`）。
#[derive(Debug, Clone, Deserialize)]
pub struct RepositoryPage {
    #[serde(default)]
    pub repositories: Vec<crate::dto::GithubRepositoryResponse>,
    #[serde(default)]
    pub total_count: i64,
    /// 由 `page * per_page < total_count` 推出（不是响应字段，**不参与** 反序列化）。
    #[serde(skip)]
    pub next_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GraphQlEnvelope {
    #[serde(default)]
    data: Option<JsonValue>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
}

/// 当前 Unix 秒（限流头的 `X-RateLimit-Reset` 换算用）。
fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 上游 `rateLimitFromResponse`：`Retry-After`（秒）优先，其次 `X-RateLimit-Reset`
/// （unix 秒），最后保守 60s；等待窗口钳在 `[1s, 5m]`。
pub(crate) fn retry_after_secs(headers: &reqwest::header::HeaderMap, now_unix: i64) -> i64 {
    const DEFAULT: i64 = 60;
    const MIN: i64 = 1;
    const MAX: i64 = 5 * 60;
    let mut wait = DEFAULT;
    match headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
    {
        Some(secs) if secs >= 0 => wait = secs,
        _ => {
            if let Some(reset) = headers
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<i64>().ok())
            {
                let delta = reset - now_unix;
                if delta > 0 {
                    wait = delta;
                }
            }
        }
    }
    wait.clamp(MIN, MAX)
}

/// 读并解析一个 JSON 响应体，超 [`API_RESPONSE_LIMIT`] 直接判非法。
async fn read_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, GithubError> {
    if let Some(len) = resp.content_length() {
        if usize::try_from(len).unwrap_or(usize::MAX) > API_RESPONSE_LIMIT {
            return Err(GithubError::Malformed(format!(
                "response exceeds {API_RESPONSE_LIMIT} bytes"
            )));
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| GithubError::Transport(e.to_string()))?;
    if bytes.len() > API_RESPONSE_LIMIT {
        return Err(GithubError::Malformed(format!(
            "response exceeds {API_RESPONSE_LIMIT} bytes"
        )));
    }
    serde_json::from_slice(&bytes).map_err(|e| GithubError::Malformed(e.to_string()))
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
        // trailing slash 被 trim（上游每处调用都做的 `strings.TrimRight`）。
        assert_eq!(
            GithubClient::new("http://127.0.0.1:9/").trimmed_base(),
            "http://127.0.0.1:9"
        );
    }

    #[test]
    fn exchanged_token_debug_never_echoes_the_token() {
        let token = ExchangedInstallationToken {
            token: "ghs_top_secret".into(),
            expires_at: "2026-09-25T01:00:00Z".into(),
        };
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("ghs_top_secret"));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("2026-09-25T01:00:00Z"));
    }

    #[test]
    fn retry_after_prefers_header_then_reset_then_default_and_clamps() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after_secs(&headers, 1_000), 60);

        headers.insert("retry-after", "12".parse().unwrap());
        assert_eq!(retry_after_secs(&headers, 1_000), 12);

        // 钳下界（1s）。
        headers.insert("retry-after", "0".parse().unwrap());
        assert_eq!(retry_after_secs(&headers, 1_000), 1);

        // `X-RateLimit-Reset` 只在 Retry-After 缺失时生效。
        let mut reset = reqwest::header::HeaderMap::new();
        reset.insert("x-ratelimit-reset", "1100".parse().unwrap());
        assert_eq!(retry_after_secs(&reset, 1_000), 100);
        reset.insert("x-ratelimit-reset", "900".parse().unwrap());
        assert_eq!(retry_after_secs(&reset, 1_000), 60);

        // 钳上界（5m）。
        let mut huge = reqwest::header::HeaderMap::new();
        huge.insert("retry-after", "100000".parse().unwrap());
        assert_eq!(retry_after_secs(&huge, 1_000), 300);
    }

    #[test]
    fn installation_account_falls_back_to_placeholders() {
        assert_eq!(
            InstallationAccount::unknown(),
            InstallationAccount {
                login: "unknown".into(),
                account_type: "User".into(),
                avatar_url: None
            }
        );
        let empty = InstallationAccount::from_envelope(InstallationEnvelope {
            account: InstallationAccountBody::default(),
        });
        assert_eq!(empty, InstallationAccount::unknown());

        let filled = InstallationAccount::from_envelope(InstallationEnvelope {
            account: InstallationAccountBody {
                login: "acme".into(),
                kind: "Organization".into(),
                avatar_url: Some("https://avatars.example/acme.png".into()),
            },
        });
        assert_eq!(filled.login, "acme");
        assert_eq!(filled.account_type, "Organization");
    }

    #[test]
    fn repository_page_next_page_follows_upstream_arithmetic() {
        let body = serde_json::json!({
            "total_count": 250,
            "repositories": [{
                "id": 1, "full_name": "acme/api", "html_url": "https://github.com/acme/api",
                "clone_url": "https://github.com/acme/api.git", "description": null,
                "private": true, "archived": false, "default_branch": "main"
            }]
        });
        let page: RepositoryPage = serde_json::from_value(body).unwrap();
        assert_eq!(page.total_count, 250);
        assert_eq!(page.repositories.len(), 1);
        assert_eq!(
            page.next_page, None,
            "只反序列化时 NextPage 不参与（由客户端算）"
        );
    }
}
