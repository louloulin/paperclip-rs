//! lark 长连接的**引导**：`POST /callback/ws/endpoint` 换一条一次性的 `wss://` 地址
//! （上游 `internal/integrations/lark/{ws_endpoint.go 202 行,connector.go 38 行,noop_connector.go 64 行}`）。
//!
//! - **写者**：M7-11（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §28）。
//! - 本文件只管"换地址 + 地址里的运行参数"，**不**拨号（那是 [`super::ws_connector`]）。
//!
//! # 🔴 这个端点是**我方发往 lark 的地址**，**不是**本服务暴露的路由
//!
//! `docs/60` §1.5 的结论必须落成代码注释，防止后来者照上游 `router.go` 那段陈旧注释去注册
//! 一个 OAuth callback 路由：
//!
//! - `POST /callback/ws/endpoint` 打在 **lark 的开放平台主机**上（`open.feishu.cn` /
//!   `open.larksuite.com`，即 [`super::types::Region::open_platform_base_url`]）；
//! - 它是**出站**请求，请求体是 `{AppID, AppSecret}`（**明文**，**不带**
//!   `tenant_access_token` 授权头）；响应体里给出真正要拨的 `wss://`；
//! - 上游 456 条路由表里**没有**任何一条渠道入站路由（五家全是出站长连接）
//!   ⇒ 本仓**不注册**本路径，注册了就是凭空多一条 `EXTRA_ALIAS` 形态缺陷（门 ③）。
//!
//! # 地址是**一次性的**，所以不缓存（上游逐字）
//!
//! 响应里的 `wss://` 自带 `device_id` / `service_id` 查询参数当凭据用，`device_id` 每次引导
//! 都轮换 ⇒ 重连时复用旧地址会拿到一次"看起来像 lark 宕机"的鉴权拒绝。所以**每次 `connect`
//! 引导一次**（[`super::ws_connector::Connector::run_session`] 的第一次动作）。
//!
//! ⇒ 那条 URL **等价于凭据**（[`WsEndpoint`] 手写 `Debug` 只输出 `<redacted>`），并且
//! **任何错误路径都不得回显它**（拨号失败的用例钉住这一条）。
//!
//! # 三处与上游的**有意**偏差（登记 `docs/32` §28 的 D 项）
//!
//! 1. **不回显 Lark 的原始响应体**：上游把 `truncate(string(rawResp), 512)` 拼进错误文案。
//!    本仓**只带 HTTP 状态码** —— 网关的错误体可能回声请求，而请求体里就是 `AppSecret`
//!    （`docs/60` §2.3 第 3 条）。实测先例：`dingtalk::stream::endpoint` 同款。
//! 2. **结构化错误里只回 `code` + 脱敏后的 `msg`**：Lark 的 `code` / `msg` 是运维区分
//!    "应用类型不支持"（`PersonalAgent` 那条已知风险）与"凭据错"与"lark 宕机"的**唯一**依据，
//!    所以保留；但 `msg` 先过 [`scrub_secret`]（响应是不可信输入，理论上可以回声我们的明文
//!    secret），命中即替换成 `<redacted>`。
//! 3. **不读 env**：上游的部署级覆盖来自 `MULTICA_LARK_CALLBACK_BASE_URL`。本 crate 的纪律是
//!    **只有** `mc_http::state::ChannelKeys` 读 env（`docs/60` §2.3 第 4 条）⇒ 本文件只提供
//!    [`HttpEndpointFetcher::with_base_url`] 这个**注入口**，宿主装配时把值传进来。
//!
//! # 凭据面（`docs/60` §2.3 四条判据，逐条落在这里）
//!
//! 1. 承载明文 secret 的类型 [`BootstrapRequest`] 与承载一次性地址的 [`WsEndpoint`]
//!    **手写 `Debug`** 输出 `<redacted>`；
//! 2. 本文件的 `tracing::*` **零条**（连接器只插值 `service_id` / `ping_interval` 这类非秘密量）；
//! 3. 「错误路径不回显凭据」两条用例：HTTP 错误 / Lark 业务错误（`ws_endpoint/tests.rs`，
//!    真 `reqwest` + 本地 loopback 服务端）；
//! 4. 键名进 redaction 表那一侧在 `mc-telemetry`（`docs/60` §2.3 第 4 条），本文件不新增日志字段。

use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

use super::http_client::DEFAULT_REQUEST_TIMEOUT;
use super::params::InstallationCredentials;
use super::types::Region;
use crate::channel::{ChannelError, ChannelResult};

// =====================================================================
// 常量（上游 `ws_endpoint.go`）
// =====================================================================

/// 引导路径 —— 打在 **lark** 的开放平台主机上（见模块文档，**不是**本服务的路由）。
pub const WS_ENDPOINT_PATH: &str = "/callback/ws/endpoint";

/// 引导请求的 `Content-Type`（上游逐字带 `charset=utf-8`）。
pub const BOOTSTRAP_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// 引导请求的 `locale` 头（上游逐字发 `zh`）：Lark 用它决定错误 `msg` 的语言。
pub const BOOTSTRAP_LOCALE: &str = "zh";

/// `service_id` 查询参数的键名（连接器要用它寻址出站帧）。
pub const SERVICE_ID_QUERY_KEY: &str = "service_id";

// =====================================================================
// 请求（上游 `bootstrapRequest`）
// =====================================================================

/// 引导请求体。
///
/// 字段名是 **`PascalCase`**（`AppID` / `AppSecret`），**不是** `snake_case` —— 服务端的 JSON 标签
/// 就是 PascalCase（官方 SDK 的 `pbbp2` schema 定了格式，写 `snake_case` 会不匹配而拿不到地址）。
///
/// ⚠️ `app_secret` 是**明文** ⇒ 手写 `Debug` 脱敏（`docs/60` §2.3 第 1 条）。
#[derive(Clone, Serialize)]
pub struct BootstrapRequest {
    /// 应用 id（`cli_…`）。不是秘密（上游日志逐字打印它）。
    #[serde(rename = "AppID")]
    pub app_id: String,
    /// 应用 secret（**秘密**；`Debug` 只输出 `<redacted>`）。
    #[serde(rename = "AppSecret")]
    pub app_secret: String,
}

impl std::fmt::Debug for BootstrapRequest {
    /// 手写脱敏：只交代**哪个**字段是密钥，绝不打印它的值。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapRequest")
            .field("app_id", &self.app_id)
            .field("app_secret", &"<redacted>")
            .finish()
    }
}

/// 组装引导请求体（**纯函数**：用例不需要网络就能钉住它的逐字形态）。
#[must_use]
pub fn build_bootstrap_request(creds: &InstallationCredentials) -> BootstrapRequest {
    BootstrapRequest {
        app_id: creds.app_id.clone(),
        app_secret: creds.app_secret.expose().to_string(),
    }
}

// =====================================================================
// 响应（上游 `endpointResponse`）
// =====================================================================

/// 引导响应（上游 `endpointResponse` + `Endpoint` + `ClientConfig`）。
///
/// 字段名同为 `PascalCase`（`URL` / `ClientConfig` / `PingInterval`…）—— 与请求侧同一个理由。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EndpointResponse {
    /// Lark 的业务码（0 = 成功）。
    #[serde(default)]
    pub code: i32,
    /// 业务错误文案（**不可信输入**，见 [`scrub_secret`]）。
    #[serde(default)]
    pub msg: String,
    /// 数据段。
    #[serde(default)]
    pub data: EndpointData,
}

/// 引导响应的 `data` 段。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EndpointData {
    /// 一次性 `wss://` 地址（**等价于凭据**：`device_id` / `service_id` 就在 query 里）。
    #[serde(rename = "URL", default)]
    pub url: String,
    /// 服务端下发的运行参数（**秒**）。
    #[serde(rename = "ClientConfig", default)]
    pub client_config: ClientConfig,
}

/// 服务端下发的 `ClientConfig`（上游同名结构；单位是**秒**）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientConfig {
    /// 重连次数上限（`-1` = 不限）。
    #[serde(rename = "ReconnectCount", default)]
    pub reconnect_count: i32,
    /// 重连间隔（秒）。
    #[serde(rename = "ReconnectInterval", default)]
    pub reconnect_interval: i32,
    /// 重连抖动（秒）。
    #[serde(rename = "ReconnectNonce", default)]
    pub reconnect_nonce: i32,
    /// 心跳间隔（秒）。
    #[serde(rename = "PingInterval", default)]
    pub ping_interval: i32,
}

// =====================================================================
// 解析出来的端点（上游 `WSEndpoint`）
// =====================================================================

/// 一条已解析的传输目标 + 服务端下发的运行参数（上游 `WSEndpoint`）。
///
/// ⚠️ `url` 与 `headers` **手写 `Debug` 脱敏**：`url` 自带一次性的 `device_id`（凭据），
/// `headers` 是握手头（将来可能带授权）。这是本片承载凭据的**第二个**类型（第一个是
/// [`BootstrapRequest`]），也是"手写 `Debug`"那条 `DoD` 在本片的落点。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WsEndpoint {
    /// 一次性 `wss://` 地址（**别记日志**）。
    pub url: String,
    /// 握手头（Lark 当前恒为空；保留上游形态）。
    pub headers: HeaderMap,
    /// 出站帧要用的 `service_id`（从 `url` 的 query 里解出来）。
    pub service_id: i32,
    /// 心跳间隔（服务端下发优先于静态默认值）。
    pub ping_interval: Duration,
    /// 重连间隔（服务端下发的建议；**本仓的重连节奏归 supervisor**，见 `docs/60` §2.4）。
    pub reconnect_interval: Duration,
    /// 重连抖动（同上，仅随连接日志上报）。
    pub reconnect_nonce: Duration,
    /// 重连次数上限（`-1` = 不限；同上）。
    pub reconnect_count: i32,
}

impl std::fmt::Debug for WsEndpoint {
    /// 手写脱敏：`url` / `headers` 只报存在性，其余（非秘密的运行参数）原样给出。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WsEndpoint")
            .field("url", &"<redacted>")
            .field("headers", &"<redacted>")
            .field("service_id", &self.service_id)
            .field("ping_interval", &self.ping_interval)
            .field("reconnect_interval", &self.reconnect_interval)
            .field("reconnect_nonce", &self.reconnect_nonce)
            .field("reconnect_count", &self.reconnect_count)
            .finish()
    }
}

/// 从 `wss://…?service_id=42` 里取出 `service_id`（上游 `parseServiceIDFromURL`）。
///
/// 连接器没有这个值就无法寻址出站帧（ping / pong / ACK 的 `Frame.Service`）⇒ 解不出就是
/// **引导失败**，不许拿 0 蒙过去。
///
/// # Errors
///
/// URL 解不开 / 没有 `service_id` / 不是整数 ⇒ [`ChannelError::Transport`]
/// （错误文案**只带故障类别**，不含 URL —— 那条 URL 自带凭据）。
pub fn parse_service_id_from_url(raw_url: &str) -> ChannelResult<i32> {
    let parsed = reqwest::Url::parse(raw_url).map_err(|_| ChannelError::Transport {
        message: "lark ws endpoint: the returned wss url is not a valid url".to_string(),
    })?;
    let raw = parsed
        .query_pairs()
        .find(|(key, _)| key == SERVICE_ID_QUERY_KEY)
        .map(|(_, value)| value.into_owned());
    let Some(raw) = raw else {
        return Err(ChannelError::Transport {
            message: "lark ws endpoint: the returned wss url has no service_id".to_string(),
        });
    };
    raw.parse::<i32>().map_err(|_| ChannelError::Transport {
        message: "lark ws endpoint: service_id is not an integer".to_string(),
    })
}

/// 把明文 secret 从**不可信**文案里抠掉（`docs/60` §2.3 第 3 条）。
///
/// 只用于"平台给我们的错误文案"这一类不可信文本：上游会把 `msg` 原样交给运维，而响应体理论上
/// 可以回声请求（请求里有明文 secret）。命中即整段替换成 `<redacted>`。空 secret 不做替换
/// （`str::replace` 空串会插到每个位置，那会毁掉文案）。
#[must_use]
pub fn scrub_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "<redacted>")
}

// =====================================================================
// 引导端口（上游 `EndpointFetcher`）
// =====================================================================

/// 按安装换一条一次性 `wss://` 地址的接缝（上游 `EndpointFetcher`）。
///
/// 端口化的理由与 engine 的其它端口一致：连接器的用例不该开真 socket。生产实现是
/// [`HttpEndpointFetcher`]（本地 loopback 用例也走它，验的是真 `reqwest` 路径）。
///
/// **错误面**：实现**不得**把响应体原样写进错误，也**不得**把 `wss://` 地址写进错误
/// （它自带凭据）—— 见模块文档的偏差 1/2。
#[async_trait]
pub trait EndpointFetcher: Send + Sync {
    /// 引导一条地址。
    ///
    /// # Errors
    ///
    /// 凭据不全 ⇒ [`ChannelError::InvalidConfig`]；网络 / 非 2xx / 响应解不开 / 业务码非 0 /
    /// 地址里没有 `service_id` ⇒ [`ChannelError::Transport`]。
    async fn endpoint(&self, creds: &InstallationCredentials) -> ChannelResult<WsEndpoint>;
}

/// 生产引导：`reqwest` POST [`WS_ENDPOINT_PATH`]，超时 [`DEFAULT_REQUEST_TIMEOUT`]（10s）。
#[derive(Debug, Clone)]
pub struct HttpEndpointFetcher {
    base_url: Option<String>,
    http: reqwest::Client,
}

impl Default for HttpEndpointFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpEndpointFetcher {
    /// 生产形态（**没有**部署级覆盖 ⇒ 每次调用按安装的 region 解析主机）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_url: None,
            http: reqwest::Client::builder()
                .timeout(DEFAULT_REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// 部署级主机覆盖（上游的 `MULTICA_LARK_CALLBACK_BASE_URL`；宿主注入）。
    ///
    /// 非空时**无视**安装的 region（所有安装都指到这个主机）；**空 = 没有覆盖**。
    /// 尾部 `/` 会被剥掉。用例的 loopback 服务端靠它（与 `dingtalk::stream::ReqwestOpener`
    /// / `slack` 的 `api_base()` / M8-1 的 `GITHUB_API_BASE` 同款做法）。
    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let trimmed = base_url.into();
        let trimmed = trimmed.trim_end_matches('/');
        Self {
            base_url: if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            },
            ..Self::new()
        }
    }

    /// 换一个 `reqwest::Client`（用例 / 代理 / 自定义 TLS）。
    #[must_use]
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// 本实例的部署级覆盖（诊断用；`None` = 按 region 解析）。
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// 这次调用该打哪个主机：显式覆盖优先，否则按安装的 region。
    #[must_use]
    pub fn resolve_base_url<'a>(&'a self, creds: &'a InstallationCredentials) -> &'a str {
        self.base_url
            .as_deref()
            .unwrap_or_else(|| creds.region.open_platform_base_url())
    }
}

#[async_trait]
impl EndpointFetcher for HttpEndpointFetcher {
    async fn endpoint(&self, creds: &InstallationCredentials) -> ChannelResult<WsEndpoint> {
        if !creds.is_complete() {
            // 只报"缺哪个"，绝不带值（`InvalidConfig` 的 `reason` 会进错误体）。
            return Err(ChannelError::InvalidConfig {
                kind: "lark".to_string(),
                reason: if creds.app_id.is_empty() {
                    "lark ws endpoint: missing app_id".to_string()
                } else {
                    "lark ws endpoint: missing app_secret".to_string()
                },
            });
        }
        let base = self.resolve_base_url(creds);
        let url = format!("{}{WS_ENDPOINT_PATH}", base.trim_end_matches('/'));
        let request = build_bootstrap_request(creds);
        let response = self
            .http
            .post(&url)
            .header("Content-Type", BOOTSTRAP_CONTENT_TYPE)
            .header("locale", BOOTSTRAP_LOCALE)
            .json(&request)
            .send()
            .await
            .map_err(|error| ChannelError::Transport {
                // 只带**类别**：`Display` 里可能回显 URL 与请求体（后者含 secret）。
                message: format!(
                    "lark ws endpoint: bootstrap request failed ({})",
                    if error.is_timeout() {
                        "timeout"
                    } else {
                        "transport"
                    }
                ),
            })?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|_| ChannelError::Transport {
                message: "lark ws endpoint: the bootstrap response is unreadable".to_string(),
            })?;
        if !status.is_success() {
            // ⚠️ **不**回显响应体（见模块文档偏差 1）。
            return Err(ChannelError::Transport {
                message: format!(
                    "lark ws endpoint: bootstrap failed with status {}",
                    status.as_u16()
                ),
            });
        }
        let decoded: EndpointResponse =
            serde_json::from_slice(&body).map_err(|_| ChannelError::Transport {
                message: "lark ws endpoint: the bootstrap response is not the expected JSON"
                    .to_string(),
            })?;
        if decoded.code != 0 || decoded.data.url.is_empty() {
            // 业务码原样给出（运维要靠它区分"应用类型不支持"/"凭据错"/"lark 宕机"）；
            // `msg` 先脱敏（不可信输入，见模块文档偏差 2）。
            return Err(ChannelError::Transport {
                message: format!(
                    "lark ws endpoint: code={} msg={:?}",
                    decoded.code,
                    scrub_secret(&decoded.msg, creds.app_secret.expose())
                ),
            });
        }
        let service_id = parse_service_id_from_url(&decoded.data.url)?;
        let config = decoded.data.client_config;
        Ok(WsEndpoint {
            url: decoded.data.url,
            headers: HeaderMap::new(),
            service_id,
            ping_interval: seconds(config.ping_interval),
            reconnect_interval: seconds(config.reconnect_interval),
            reconnect_nonce: seconds(config.reconnect_nonce),
            reconnect_count: config.reconnect_count,
        })
    }
}

/// 秒 → [`Duration`]（**非正数**回落 0：服务端省略字段时是 0，连接器再套静态默认值）。
fn seconds(value: i32) -> Duration {
    if value <= 0 {
        Duration::ZERO
    } else {
        // 上游把负数当"未设置"；这里只对正数取值，故 u64 转换必成功。
        Duration::from_secs(u64::try_from(value).unwrap_or(0))
    }
}

/// 该安装的引导主机（诊断 / 用例：不经过网络就能断言 region 解析）。
#[must_use]
pub fn bootstrap_base_url(region: Region) -> &'static str {
    region.open_platform_base_url()
}

#[cfg(test)]
mod tests;
