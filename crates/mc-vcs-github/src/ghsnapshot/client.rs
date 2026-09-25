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
//! `client.go:64-65` 的 `apiBase` 字段 —— 本地替身按 REST / GraphQL 形状答，
//! 由 [`Client::with_api_base`] 注入。
//!
//! # 本文件的职责划分（M8-1 vs M8-5）
//!
//! | 层 | 文件 | 内容 |
//! | --- | :-: | --- |
//! | **凭据链**（本文件，M8-1） | `client.rs` | App JWT（RS256）→ installation token（**缓存 + 单飞**，见 [`crate::token_cache`]）→ 带认证的 REST / GraphQL 调用 |
//! | 查询与归一化（M8-5） | `ghsnapshot/{snapshot,refresh}.rs` | `prSnapshotQuery` 的游标分页、`normalizeNode` 的三态裁决、worker 池 / TTL / 退避 |
//!
//! anchor 期 `client.rs` 里有一个 `fetch_pr_snapshot(repo_owner, repo_name, pr_number)`
//! 的桩。它被**移除**，换成通用原语 [`Client::graph_ql`]：GraphQL 的**查询文本**在上游住在
//! `snapshot.go`（M8-5 的文件），把它抄进本文件等于让两个写者各持一份查询。上游的分层就是
//! `snapshot.go` 调 `client.graphQL(...)` —— 本仓照搬（登记 `docs/32` §9.12）。
//!
//! # 凭据纪律
//!
//! App 私钥（PEM）与 installation token 都**不派生 `Debug`**（手写脱敏）；错误消息只带
//! 原因名与状态码，**不回显**任何密钥材料（`docs/61` §2.4）。

use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::app::AppJwtSigner;
use crate::rest::{GithubClient, GithubError};
use crate::token_cache::{get_or_fetch, InstallationToken, InstallationTokenCache};

/// GitHub GraphQL/REST 客户端（App 凭据链 + token 缓存 + 单飞）。
pub struct Client {
    /// `GITHUB_APP_ID`（JWT 的 `iss`）；未配置为 `None`。
    pub(crate) app_id: Option<String>,
    /// App 私钥 PEM（**绝不**进 `Debug`）；未配置为 `None`。
    pub(crate) private_key_pem: Option<String>,
    /// REST / GraphQL base（替身接缝）。
    pub(crate) api_base: String,
    /// 带认证的 HTTP 出口（与 `api_base` 同步构造）。
    http: GithubClient,
    /// 每 installation 一枚 token 的缓存（缓存 + 单飞，`docs/61` §4.1 的 M8-1 行）。
    tokens: Arc<InstallationTokenCache>,
}

impl Client {
    /// 默认 base（上游 `defaultAPIBase`）。
    pub const DEFAULT_API_BASE: &'static str = "https://api.github.com";

    /// 上游 `http.Client{Timeout: 20 * time.Second}`。
    pub const HTTP_TIMEOUT_SECS: u64 = 20;

    /// 从 App 配置构造（`api_base` 取默认）。
    pub fn new(app_id: Option<String>, private_key_pem: Option<String>) -> Self {
        Self::with_base(app_id, private_key_pem, Self::DEFAULT_API_BASE)
    }

    fn with_base(
        app_id: Option<String>,
        private_key_pem: Option<String>,
        api_base: impl Into<String>,
    ) -> Self {
        let api_base = api_base.into();
        Self {
            app_id,
            private_key_pem,
            http: GithubClient::new(api_base.clone()),
            api_base,
            tokens: Arc::new(InstallationTokenCache::new()),
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
        self.http = GithubClient::new(self.api_base.clone());
        self
    }

    /// 自定义 token 缓存（测试注入余量 / 断言单飞）。
    #[must_use]
    pub fn with_token_cache(mut self, tokens: Arc<InstallationTokenCache>) -> Self {
        self.tokens = tokens;
        self
    }

    /// App 凭据是否配置齐（上游 `Enabled()`：nil 或私钥缺失 ⇒ `false`）。
    pub fn enabled(&self) -> bool {
        self.app_id.is_some() && self.private_key_pem.is_some()
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// token 缓存（诊断/测试）。
    pub fn token_cache(&self) -> &Arc<InstallationTokenCache> {
        &self.tokens
    }

    /// 签一枚 App JWT（上游 `signAppJWT`；`iat` 回拨 60s、`exp` 封顶 9 分钟）。
    ///
    /// `now_unix` 显式传入（**不读系统时钟**）⇒ 过期/时钟偏移可注入测试。
    ///
    /// PEM 每次现解析（`ring` 解析这枚 2KB 密钥是微秒级，而换 token 一小时才发生一次）——
    /// 这样 [`Client::new`] 保持**不会失败**的签名（anchor 定的形状），签名器不需要跨调用的
    /// 缓存，也少一个 `OnceCell` 的失败态。
    ///
    /// # Errors
    ///
    /// 未配置 ⇒ [`GithubError::NotConfigured`]；私钥非法 ⇒ [`GithubError::Malformed`]。
    pub fn sign_app_jwt(&self, now_unix: i64) -> Result<String, GithubError> {
        let app_id = self.app_id.as_deref().ok_or(GithubError::NotConfigured)?;
        let pem = self
            .private_key_pem
            .as_deref()
            .ok_or(GithubError::NotConfigured)?;
        let signer = AppJwtSigner::from_pem(app_id, pem)
            .map_err(|e| GithubError::Malformed(e.to_string()))?;
        signer
            .sign_app_jwt(now_unix)
            // `AppJwtError` 的实现保证不含密钥材料（`ring` 只给原因名）。
            .map_err(|e| GithubError::Malformed(format!("github app jwt: {e}")))
    }

    /// 取（或换）一枚 installation token —— **缓存 + 单飞**。
    ///
    /// # Errors
    ///
    /// 未配置 / 签名失败 / 交换失败 ⇒ [`GithubError`]（失败**不**污染缓存）。
    pub async fn installation_token(
        &self,
        installation_id: i64,
        now_unix: i64,
    ) -> Result<InstallationToken, GithubError> {
        if !self.enabled() {
            return Err(GithubError::NotConfigured);
        }
        let tokens = self.tokens.clone();
        get_or_fetch(&tokens, installation_id, now_unix, || async {
            let app_jwt = self.sign_app_jwt(now_unix)?;
            let exchanged = self
                .http
                .exchange_installation_token(&app_jwt, installation_id)
                .await?;
            Ok(InstallationToken::new(
                exchanged.expose(),
                expiry_unix(&exchanged.expires_at, now_unix),
            ))
        })
        .await
    }

    /// 以 installation 身份发一次 GraphQL 查询，返回信封里的 `data`。
    ///
    /// # Errors
    ///
    /// token 取不到、传输失败、查询级错误 ⇒ [`GithubError`]。
    pub async fn graph_ql(
        &self,
        installation_id: i64,
        query: &str,
        variables: &JsonValue,
        now_unix: i64,
    ) -> Result<JsonValue, GithubError> {
        let token = self.installation_token(installation_id, now_unix).await?;
        self.http.graph_ql(token.expose(), query, variables).await
    }

    /// **尽力而为**地撤销一枚 installation token（上游 `revokeGitHubInstallationToken`）。
    ///
    /// 缓存路径**不**调用它：worker 共用的 token 被撤销会毒化缓存。它给需要「用完即弃」的
    /// 场景（browse 路径由 [`crate::rest::GithubClient`] 自己撤销）留一个出口。
    ///
    /// # Errors
    ///
    /// 传输失败 / 非 2xx ⇒ [`GithubError`]。
    pub async fn revoke_token(&self, token: &str) -> Result<(), GithubError> {
        self.http.revoke_installation_token(token).await
    }

    /// 本客户端的带认证 HTTP 出口（M8-5 的 `refresh.rs` 需要直接发 REST 调用时用）。
    pub fn http(&self) -> &GithubClient {
        &self.http
    }
}

/// `expires_at`（RFC3339）→ Unix 秒；缺失/非法时按上游兜底 `now + 1h`。
fn expiry_unix(expires_at: &str, now_unix: i64) -> i64 {
    if expires_at.is_empty() {
        return now_unix + 3_600;
    }
    chrono::DateTime::parse_from_rfc3339(expires_at).map_or(now_unix + 3_600, |t| t.timestamp())
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
            // `http` / `tokens` 不是可展示的字段（一个是连接池，一个是密钥缓存）。
            .finish_non_exhaustive()
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

    #[test]
    fn disabled_client_signs_nothing() {
        let err = Client::disabled().sign_app_jwt(1_000).unwrap_err();
        assert!(matches!(err, GithubError::NotConfigured));
        assert!(!err.to_string().contains("PRIVATE KEY"));
    }

    #[tokio::test]
    async fn disabled_client_has_no_installation_token() {
        let err = Client::disabled()
            .installation_token(1, 1_000)
            .await
            .unwrap_err();
        assert!(matches!(err, GithubError::NotConfigured));
    }

    #[test]
    fn malformed_private_key_never_leaks_key_material() {
        let client = Client::new(
            Some("123".into()),
            Some("-----BEGIN PRIVATE KEY-----\nnope\n-----END PRIVATE KEY-----\n".into()),
        );
        // `enabled()` 只看「配没配」，配错私钥是取 token 时才暴露（上游 `NewClientFromEnv` 同判）。
        assert!(client.enabled());
        let err = client.sign_app_jwt(1_000).unwrap_err();
        let rendered = format!("{err}");
        assert!(!rendered.contains("nope"));
        assert!(!rendered.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn expiry_parsing_falls_back_to_one_hour() {
        // 1_000_000_000 == 2001-09-09T01:46:40Z（不做换算，直接钉住已知的 unix 值）。
        assert_eq!(expiry_unix("2001-09-09T01:46:40Z", 0), 1_000_000_000);
        assert_eq!(expiry_unix("", 1_000), 4_600);
        assert_eq!(expiry_unix("not-a-date", 1_000), 4_600);
    }
}
