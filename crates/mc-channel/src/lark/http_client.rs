//! lark **传输核心**：真 HTTP 客户端 —— `tenant_access_token` 的缓存与过期重铸、
//! 每请求超时、region 感知的主机解析、13 个端点、以及资源下载
//! （上游 `internal/integrations/lark/http_client.go` 的传输那一半）。
//!
//! - **写者**：M7-10（`docs/60-M7-PLAN.md` §3.3；本片是 stage 5 的第一片，**0 路由**）。
//! - **本文件的边界**（`docs/60` §6.3 的四文件切分）：**传输与令牌**。请求构建在
//!   [`super::params`]、响应解码在 [`super::types`]、错误分类在 [`super::client`]。
//!   上游 `http_client.go` 1,370 行 ⇒ 实现前先按这三条轴拆（本片 `DoD` 的预飞项）。
//!
//! # 令牌缓存与过期刷新（本片专属验收第 1 条）
//!
//! - **键只有 `app_id`**（不是 `(app_id, region)`）：飞书 / Lark 的 `app_id`（`cli_…`）
//!   在两个**云之间全局唯一**、一个应用只存在于其中一个云，所以一个 `app_id` 永远不会映射到
//!   两个 region。DB 用 `lark_installation` 的 `UNIQUE(app_id)` 钉住同一个假设。
//! - **提前量**：`expire - TOKEN_SAFETY_MARGIN`（60s）—— 稳稳盖过本文件的任何在飞超时
//!   （10s / 45s），于是调用方**永不**在飞途中用上一枚正要过期的令牌。
//! - **兜底夹紧**：平台若给一个亚分钟的 `expire`，缓存寿命夹到 `>= 2 × 提前量`，
//!   于是坏上游回的值也**不会**让我们缓存一枚已经过了安全窗口的令牌。
//! - **并发**：与上游逐字一致 —— 只把**查表 / 写表**放在锁里，未命中时不折叠（不学
//!   `DingTalk` 侧的 singleflight）。稳态争用因此是"锁下一次 map 读"，而不是每调用一次往返。
//! - **被拒即作废 + 重放一次**：Lark 在一个**非 2xx**（HTTP 400 + `{"code":99991663}`）或一个
//!   2xx 信封里都可能说"这枚令牌不认"。两条路径都在 [`HttpApiClient::do_authed_json`] 统一
//!   检查 ⇒ 作废缓存、重铸、**重放恰好一次**。
//!   重放对这里**每个**调用方都安全：Lark 在**处理请求之前**就拒掉坏令牌，什么都没投递，
//!   所以重放不可能重复一条消息。只重试令牌错 —— 业务码、5xx、链路失败原样透出
//!   （它们要么是定论、要么对"是否已投递"不明确，换令牌都不解决）。
//!
//! # 与上游的两处**形态**差异（不改变语义，登记 `docs/32` §25 的 D 项）
//!
//! 1. **每请求超时代替两个 `http.Client`**：上游配 `HTTPClient`（10s）与
//!    `ResourceHTTPClient`（45s）两个客户端。本仓用同一个 [`reqwest::Client`] + 每请求的
//!    [`reqwest::RequestBuilder::timeout`]（reqwest 的总超时含读体）—— 两条超时线各自独立、
//!    连接池共享，语义对价。
//! 2. **统一检查信封里的业务码**：上游在每个端点里各写一次 `if resp.Code != 0 {...}`。
//!    本仓在 [`HttpApiClient::attempt`] 里一次判完（2xx 的非零码与带码的非 2xx 走**同一条**
//!    作废 + 重放路径）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - [`CachedToken`] 持有明文令牌 ⇒ **不派生 `Debug`**；[`HttpApiClient`] 手写 `Debug`
//!   只报缓存**条数**；
//! - 本文件的 `tracing::*` **只**插值操作名、`app_id`（上游日志逐字打印它）与平台机器码；
//! - 「错误路径不回显凭据」有专门用例（`http_client/tests.rs`，跑真 HTTP 的四条错误路径）。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::client::{is_token_error, parse_lark_error_body, ApiError, TokenCacheInvalidator};
use super::params::{InstallationCredentials, TENANT_ACCESS_TOKEN_PATH};
use super::types::TenantTokenResponse;

// =====================================================================
// 常量（上游 `http_client.go`，逐字）
// =====================================================================

/// 普通 `OpenAPI` 调用的每请求超时（上游 `defaultRequestTimeout`）。
///
/// Lark 的 API 正常远低于 1s；这里留出自建部署到 `feishu.cn` 的跨区延迟余量。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 令牌提前失效的余量（上游 `tokenSafetyMargin`）—— 从 Lark 的 `expire` 里减掉它，
/// 于是我们会在一枚令牌**真的**过期之前刷新。60s 稳稳超过下面任何在飞超时。
pub const TOKEN_SAFETY_MARGIN: Duration = Duration::from_secs(60);

/// 一次消息资源下载的默认上限（上游 `DefaultResourceDownloadTimeout`，公开常量 ——
/// 上游注释：通道媒体面的"结算"不变式用例要断言它会**远大于**流水线里的每个预算）。
pub const DEFAULT_RESOURCE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(45);

/// 可注入的时钟（用例用它做**确定性**的令牌过期测试；生产就是 [`Instant::now`]）。
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

// =====================================================================
// 配置
// =====================================================================

/// 生产 lark HTTP 客户端的配置（上游 `HTTPClientConfig`）。
#[derive(Clone)]
pub struct HttpClientConfig {
    /// **部署级**主机覆盖，例如 `https://open.feishu.cn` 或 `https://open.larksuite.com`。
    ///
    /// 非空时**无视**安装的 region，把所有流量都指到这个主机；用例把它指向本地替身。
    /// **空（生产默认）= 没有覆盖** ⇒ 每次调用按 [`InstallationCredentials::region`] 解析主机，
    /// 于是一个部署同时服务飞书与 Lark。尾部 `/` 会被剥掉。
    pub base_url: Option<String>,
    /// 每次出站调用用的传输。用例换成指向本地替身的客户端。
    pub http: reqwest::Client,
    /// 普通 `OpenAPI` 调用的每请求超时（见模块文档差异 1）。
    pub request_timeout: Duration,
    /// 资源下载的每请求超时（长于普通调用：消息视频是二进制传输，不是 JSON RPC）。
    /// 上游把它压在入站 dedup 的陈旧认领窗口（60s）之下，好让一次慢下载不至于邀请副本二来
    /// 重抢同一条消息。
    pub resource_download_timeout: Duration,
    /// 可注入的时钟（确定性过期测试用）。
    pub now: Clock,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            http: reqwest::Client::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            resource_download_timeout: DEFAULT_RESOURCE_DOWNLOAD_TIMEOUT,
            now: Arc::new(Instant::now),
        }
    }
}

impl HttpClientConfig {
    /// 起一份默认配置。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 设定部署级主机覆盖（尾部 `/` 剥掉；空串等同不设置）。
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let trimmed = base_url.trim_end_matches('/').to_string();
        self.base_url = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        self
    }

    /// 换传输（用例指向本地替身）。
    #[must_use]
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// 换普通调用的每请求超时。
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// 换资源下载的每请求超时。
    #[must_use]
    pub fn with_resource_download_timeout(mut self, timeout: Duration) -> Self {
        self.resource_download_timeout = timeout;
        self
    }

    /// 换时钟（确定性过期测试用）。
    #[must_use]
    pub fn with_now(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }
}

// =====================================================================
// 客户端
// =====================================================================

/// 一条缓存的令牌。
///
/// ⚠️ **不派生 `Debug`**：它持有明文令牌（见模块文档的凭据纪律）。
struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// 真的会跟 Lark 开放平台讲 HTTPS 的 [`ApiClient`]（上游 `httpAPIClient`）。
///
/// 每安装的凭据**随每次调用传入**；令牌按 `app_id` 缓存，于是一个 Multica 服务端会复用同一
/// 应用在 Lark 上的 `tenant_access_token`。
pub struct HttpApiClient {
    config: HttpClientConfig,
    tokens: Mutex<HashMap<String, CachedToken>>,
}

impl fmt::Debug for HttpApiClient {
    /// 手写：**只报缓存条数**，绝不打印缓存的令牌（见模块文档的凭据纪律）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cached = self.tokens.lock().map_or(0, |map| map.len());
        formatter
            .debug_struct("HttpApiClient")
            .field("base_url", &self.config.base_url)
            .field("cached_tokens", &cached)
            .finish_non_exhaustive()
    }
}

impl Default for HttpApiClient {
    fn default() -> Self {
        Self::new(HttpClientConfig::default())
    }
}

impl HttpApiClient {
    /// 装配（令牌缓存初始为空）。
    #[must_use]
    pub fn new(config: HttpClientConfig) -> Self {
        Self {
            config,
            tokens: Mutex::new(HashMap::new()),
        }
    }

    /// 缓存里**尚未过期**的令牌。
    fn cached_token(&self, app_id: &str, now: Instant) -> Option<String> {
        let map = self.tokens.lock().ok()?;
        let entry = map.get(app_id)?;
        if entry.expires_at > now {
            return Some(entry.value.clone());
        }
        None
    }

    /// 写入缓存。
    fn store_token(&self, app_id: &str, value: &str, expires_at: Instant) {
        let Ok(mut map) = self.tokens.lock() else {
            return;
        };
        map.insert(
            app_id.to_string(),
            CachedToken {
                value: value.to_string(),
                expires_at,
            },
        );
    }

    /// 丢掉 `app_id` 的缓存令牌（平台说这枚不认了之后调用）。
    fn invalidate_token(&self, app_id: &str) {
        if let Ok(mut map) = self.tokens.lock() {
            map.remove(app_id);
        }
    }

    /// 一次调用的开放平台主机：显式覆盖优先，否则按安装的 region（上游 `resolveBaseURL`）。
    #[must_use]
    pub fn resolve_base_url<'a>(&'a self, credentials: &'a InstallationCredentials) -> &'a str {
        match self.config.base_url.as_deref() {
            Some(base) => base,
            None => credentials.region.open_platform_base_url(),
        }
    }

    /// 给这条安装取一枚可用的 `tenant_access_token`（上游 `tenantAccessToken`）。
    ///
    /// 缓存命中（且在安全窗口内）就直接复用；否则向
    /// [`TENANT_ACCESS_TOKEN_PATH`] 铸一枚新的（**自建**应用端点，见该常量的文档）。
    ///
    /// 上游把它公开到包内（本仓 `pub`）：路由层的 WS 凭据提供者也要它。
    ///
    /// # Errors
    ///
    /// `app_id` / `app_secret` 缺失；链路失败；平台拒绝；响应缺令牌。
    pub async fn tenant_access_token(
        &self,
        credentials: &InstallationCredentials,
    ) -> Result<String, ApiError> {
        const OP: &str = "tenant access token";
        if credentials.app_id.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing app_id",
            });
        }
        if credentials.app_secret.is_empty() {
            return Err(ApiError::InvalidRequest {
                op: OP,
                reason: "missing app_secret",
            });
        }

        let now = (self.config.now)();
        if let Some(token) = self.cached_token(&credentials.app_id, now) {
            return Ok(token);
        }

        let base = self.resolve_base_url(credentials).to_string();
        let body = json!({
            "app_id": credentials.app_id,
            "app_secret": credentials.app_secret.expose(),
        });
        let response: TenantTokenResponse = decode(
            OP,
            self.do_json(
                &base,
                reqwest::Method::POST,
                TENANT_ACCESS_TOKEN_PATH,
                None,
                Some(&body),
                OP,
            )
            .await?,
        )?;
        if response.tenant_access_token.is_empty() {
            return Err(ApiError::Malformed { op: OP });
        }

        let expire = duration_from_secs(response.expire);
        // 夹紧到 `>= 2 × 提前量`：坏上游回一个亚分钟 expire 时，我们**不会**缓存一枚
        // 已经过了安全窗口的令牌（上游注释逐字）。
        let usable = expire
            .max(TOKEN_SAFETY_MARGIN * 2)
            .saturating_sub(TOKEN_SAFETY_MARGIN);
        self.store_token(
            &credentials.app_id,
            &response.tenant_access_token,
            now + usable,
        );
        Ok(response.tenant_access_token)
    }

    /// 带令牌调一次，并在**平台说这枚令牌不认**时作废缓存 + 重铸 + 重放**恰好一次**。
    ///
    /// # Errors
    ///
    /// 令牌铸造失败；两次都是令牌错时的第二次结果；其余错误原样透出。
    async fn do_authed_json<T: DeserializeOwned>(
        &self,
        credentials: &InstallationCredentials,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        op: &'static str,
    ) -> Result<T, ApiError> {
        let token = self.tenant_access_token(credentials).await?;
        let base = self.resolve_base_url(credentials).to_string();
        match self
            .attempt::<T>(&base, method.clone(), path, &token, body, op)
            .await
        {
            Err(ApiError::Refused { code, .. }) if is_token_error(code) => {
                tracing::warn!(
                    op,
                    app_id = %credentials.app_id,
                    "lark http client: tenant_access_token rejected; refreshing and retrying once"
                );
                self.invalidate_token(&credentials.app_id);
                let fresh = self.tenant_access_token(credentials).await?;
                self.attempt::<T>(&base, method, path, &fresh, body, op)
                    .await
            }
            other => other,
        }
    }

    /// 一次尝试：取 JSON，先判信封里的业务码，再解成目标形状。
    async fn attempt<T: DeserializeOwned>(
        &self,
        base: &str,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<&Value>,
        op: &'static str,
    ) -> Result<T, ApiError> {
        let value = self
            .do_json(base, method, path, Some(token), body, op)
            .await?;
        // 2xx 里的业务拒绝（上游在每个端点里各写一次的那段）。
        if let Some(code) = envelope_code(&value).filter(|code| *code != 0) {
            return Err(ApiError::Refused {
                op,
                status: None,
                code,
            });
        }
        decode(op, value)
    }

    /// 一次裸 JSON 调用：编码请求体、发出去、按状态码与信封分流。
    ///
    /// `token == None` 时不带 `Authorization` 头（只有铸令牌端点走那条）。
    ///
    /// # Errors
    ///
    /// 链路失败；非 2xx；2xx 但不是 JSON。
    async fn do_json(
        &self,
        base: &str,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
        op: &'static str,
    ) -> Result<Value, ApiError> {
        let mut request = self
            .config
            .http
            .request(method, format!("{base}{path}"))
            .timeout(self.config.request_timeout);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| ApiError::Transport { op })?;
        let status = response.status().as_u16();
        let raw = response
            .bytes()
            .await
            .map_err(|_| ApiError::Transport { op })?;
        if !(200..300).contains(&status) {
            // Lark 把业务码放在**非 2xx** 的体里（凭据失效就是 HTTP 400 + code 99991663）。
            // 不看体就永远发现不了缓存里的令牌已经死了。
            return Err(refusal_or_http(status, &raw, op));
        }
        if raw.is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_slice::<Value>(&raw).map_err(|_| ApiError::Malformed { op })
    }
}

#[async_trait]
impl TokenCacheInvalidator for HttpApiClient {
    fn invalidate_token_cache(&self, app_id: &str) {
        self.invalidate_token(app_id);
    }
}

// =====================================================================
// 小工具
// =====================================================================

/// 非 2xx 的分流：体里有**非零**平台码就报 [`ApiError::Refused`]（带码），否则报纯状态码。
///
/// 体里 `code = 0` 也算"没有可用的分类信号"⇒ 落到 [`ApiError::Http`]（与上游把 `Code == 0`
/// 当"无码"的口径一致）。
pub(super) fn refusal_or_http(status: u16, raw: &[u8], op: &'static str) -> ApiError {
    match parse_lark_error_body(raw) {
        Some(code) if code != 0 => ApiError::Refused {
            op,
            status: Some(status),
            code,
        },
        _ => ApiError::Http { op, status },
    }
}

/// 信封里的业务码（非数字 / 缺字段 ⇒ `None`）。
fn envelope_code(value: &Value) -> Option<i32> {
    value
        .get("code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

/// 解成目标形状；解不出来就是形状错。
fn decode<T: DeserializeOwned>(op: &'static str, value: Value) -> Result<T, ApiError> {
    serde_json::from_value::<T>(value).map_err(|_| ApiError::Malformed { op })
}

/// 秒 → [`Duration`]（负数按 0）。
fn duration_from_secs(secs: i64) -> Duration {
    Duration::from_secs(u64::try_from(secs.max(0)).unwrap_or(0))
}

// =====================================================================
// 子模块（门 ⑩ 的 800 行硬限切分；见本文件的模块文档）
// =====================================================================

mod api;
/// 资源响应面（下载 + 响应头解码）。
///
/// `pub` 而不是私有：`ApiClient`（[`crate::lark::client`]）是**公开 trait**，它的方法返回
/// 本模块的 [`resource::DownloadedResource`] / [`resource::DownloadedResourceStream`] ⇒
/// 这两个类型必须是公开的（私有类型不得出现在公开接口里），模块因此也公开。
/// `api` 只装 impl 块、没有可命名项，保持私有。
pub mod resource;

#[cfg(test)]
mod tests;
