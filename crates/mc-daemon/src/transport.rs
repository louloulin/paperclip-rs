//! daemon 客户端的**传输缝**（M3-7 / LUM-1438）。
//!
//! ## 为什么要一层 trait
//!
//! 上游 daemon 有两条传输：HTTP（`daemon/client.go`）与 WS RPC（`rpc-v1`）。
//! 它们对同一批动作（注册 / 心跳 / claim）发同样的 JSON、拿同样的响应，
//! 只差「字节怎么出去」。把这层差异收在一个 trait 后面，收益是：
//!
//! 1. **可测**：`crates/mc-daemon/tests/client_loop.rs` 用一个记录请求的假传输
//!    断言「发了哪条路径、body 长什么样、状态机怎么迁移」，不需要起 HTTP 服务；
//! 2. **可换**：M3-7 只实现 HTTP 腿（[`HttpTransport`]）。WS RPC 腿
//!    （`daemon:rpc_request` / `rpc_response` 帧）属于 daemon 二进制的连接管理，
//!    在同一个 trait 上补一个实现即可，客户端逻辑一行不改。
//!
//! ## 与门禁的关系
//!
//! 这一层只搬字节，不做鉴权决策：token / daemon-id 头由实现自己持有，
//! 业务语义（谁能不能领哪条任务）完全在服务端。客户端**不信任**任何响应字段
//! 来做权限判断 —— 上游同理。

use async_trait::async_trait;
use serde_json::Value;

/// daemon 上报自己版本的请求头（上游 `daemon/client.go` 每条请求都带）。
///
/// 本地服务端**不消费**它（`ClientIdentity.client_version` 只用于日志，且 HTTP 腿
/// 根本不读），带上是为了与上游线形状一致，并让中间网关能按版本做灰度。
pub const CLIENT_VERSION_HEADER: &str = "X-Client-Version";

/// 传输层故障。
///
/// 三类分开是有意的：调用方对它们的反应完全不同 ——
/// [`TransportError::Unreachable`] 与 5xx 该退避重试，4xx 该停下来修配置，
/// [`TransportError::Malformed`] 说明双方线格式已经漂了（该报警，不是该重试）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// 连不上 / 读超时 / DNS 失败 —— 唯一的处置是退避重试。
    #[error("transport unreachable: {url}: {reason}")]
    Unreachable {
        /// 出错的完整 URL。
        url: String,
        /// 底层原因（reqwest 的 Display）。
        reason: String,
    },
    /// 服务端明确回了一个非 2xx。
    #[error("server returned {status}: {code}: {message}")]
    Status {
        /// HTTP 状态码。
        status: u16,
        /// 服务端错误体里的 `error.code`（无结构时是空串）。
        code: String,
        /// 服务端错误体里的 `error.message`（无结构时是原始响应文本）。
        message: String,
    },
    /// 响应不是合法 JSON（或形状与契约不符）。
    #[error("malformed response from {url}: {reason}")]
    Malformed {
        /// 出错的完整 URL。
        url: String,
        /// 解析失败原因。
        reason: String,
    },
}

impl TransportError {
    /// HTTP 状态码（`Unreachable` / `Malformed` 没有）。
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            Self::Unreachable { .. } | Self::Malformed { .. } => None,
        }
    }

    /// 服务端错误码（`Unreachable` / `Malformed` 没有）。
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Status { code, .. } => Some(code.as_str()),
            Self::Unreachable { .. } | Self::Malformed { .. } => None,
        }
    }

    /// 值不值得重试（上游 `isTransientError` 的本地等价物）。
    ///
    /// - 连不上：值得（网络抖动、服务重启）。
    /// - `408` / `429` / `5xx`：值得（超时、限流、服务端瞬时故障）。
    /// - 其余 4xx：不值得 —— 重试只会把同一个错误再打一遍。
    /// - `Malformed`：不值得自动重试（重试一百次也还是同样的字节），但需要人工介入。
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Unreachable { .. } => true,
            Self::Malformed { .. } => false,
            Self::Status { status, .. } => {
                *status == 408 || *status == 429 || (500..600).contains(status)
            }
        }
    }

    /// 服务端说「这个对象不存在」（`404`）。
    ///
    /// daemon 用它判断 runtime 是不是被删了（上游 `isRuntimeNotFoundError`
    /// → `handleRuntimeGone`：把死 UUID 从本机台账里摘掉，而不是无限重试）。
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
}

impl From<reqwest::Error> for TransportError {
    fn from(err: reqwest::Error) -> Self {
        let url = err.url().map(ToString::to_string).unwrap_or_default();
        Self::Unreachable {
            url,
            reason: err.to_string(),
        }
    }
}

/// 一条 daemon → server 的 JSON 请求。
///
/// 只用 POST：daemon 面全部 36+8 条里客户端需要发的都是 POST（GET 面是
/// workspace/repos/profile 台账，属于 daemon 启动时一次性拉取，由
/// `mc-cli`/desktop 侧驱动，不在本切片的最小可用实现里）。
#[async_trait]
pub trait DaemonTransport: Send + Sync {
    /// 发一条 POST，返回解析好的 JSON 响应体。
    ///
    /// # Errors
    ///
    /// 见 [`TransportError`] —— 连接故障、非 2xx、响应不是 JSON 三类。
    async fn post_json(&self, path: &str, body: &Value) -> Result<Value, TransportError>;
}

/// 真 HTTP 传输（`reqwest` + rustls）。
///
/// 身份有两种形态，与 `crates/mc-http/src/routes/daemon/scope.rs` 的认证表一一对应：
///
/// | 形态 | 头 | 说明 |
/// |------|----|------|
/// | 生产 | `Authorization: Bearer mdt_…` | daemon token，服务端查 `daemon_token` 表 |
/// | dev（偏离 D-1） | `X-Multica-User-Id` + `X-Daemon-Id` | 本仓 M1/M2 的 dev-mode 约定 |
///
/// 两者可以同时存在：有 token 时服务端走 token 分支，dev 头被忽略。这也是
/// `crates/mc-http/tests/daemon/support.rs` 里 e2e 用例的用法。
pub struct HttpTransport {
    base_url: String,
    client: reqwest::Client,
    token: Option<String>,
    dev_user_id: Option<String>,
    dev_daemon_id: Option<String>,
    client_version: String,
    capabilities: String,
}

impl HttpTransport {
    /// 新传输。`base_url` 是服务端根地址（不带尾斜杠，例如 `https://api.example.com`）。
    ///
    /// 能力串默认取 [`mc_daemon_proto::capabilities::daemon_http_capabilities`]
    /// （公共 10 条，**不含** `claim-poll-hints-v1` —— 那是 WS 专属，见 proto 模块文档）。
    #[must_use]
    pub fn new(base_url: impl Into<String>, client_version: impl Into<String>) -> Self {
        let capabilities = mc_daemon_proto::capabilities::encode_capabilities_header(
            &mc_daemon_proto::capabilities::daemon_http_capabilities(),
        );
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            client: reqwest::Client::new(),
            token: None,
            dev_user_id: None,
            dev_daemon_id: None,
            client_version: client_version.into(),
            capabilities,
        }
    }

    /// 用 daemon token 认证（生产形态）。
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// 用 dev-mode 身份认证（偏离 D-1，仅测试与本机开发）。
    #[must_use]
    pub fn with_dev_identity(
        mut self,
        user_id: impl Into<String>,
        daemon_id: impl Into<String>,
    ) -> Self {
        self.dev_user_id = Some(user_id.into());
        self.dev_daemon_id = Some(daemon_id.into());
        self
    }

    /// 换掉能力串（默认已是上游 HTTP 口径）。
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: impl Into<String>) -> Self {
        self.capabilities = capabilities.into();
        self
    }

    /// 换掉底层 `reqwest::Client`（超时 / 代理 / 连接池配置在这里注入）。
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// 组好一条请求（头全在这里，调用方不再操心身份）。
    fn request(&self, url: &str, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(url)
            .header(CLIENT_VERSION_HEADER, &self.client_version)
            .header(
                mc_daemon_proto::capabilities::CLIENT_CAPABILITIES_HEADER,
                &self.capabilities,
            )
            .json(body);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        if let Some(user_id) = &self.dev_user_id {
            req = req.header("X-Multica-User-Id", user_id);
        }
        if let Some(daemon_id) = &self.dev_daemon_id {
            req = req.header("X-Daemon-Id", daemon_id);
        }
        req
    }
}

#[async_trait]
impl DaemonTransport for HttpTransport {
    async fn post_json(&self, path: &str, body: &Value) -> Result<Value, TransportError> {
        let url = format!("{}{path}", self.base_url);
        let response = self.request(&url, body).send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(status_error(status.as_u16(), &text));
        }
        serde_json::from_str(&text).map_err(|err| TransportError::Malformed {
            url,
            reason: err.to_string(),
        })
    }
}

/// 把错误响应文本翻成 [`TransportError::Status`]。
///
/// 服务端错误体是 `{"error":{"code","message"}}`（`mc-http` 的 `ErrorBody`）。
/// 不是这个形状时**不猜**：把原始文本当 message 带出去（截断到 512 字节，
/// 免得一个 HTML 错误页把日志刷爆），code 留空串。
fn status_error(status: u16, text: &str) -> TransportError {
    /// 原始文本的上限：一个 HTML 错误页不该把日志刷爆。
    const MAX_RAW: usize = 512;
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let error = &value["error"];
        if error.is_object() {
            return TransportError::Status {
                status,
                code: error["code"].as_str().unwrap_or_default().to_owned(),
                message: error["message"].as_str().unwrap_or_default().to_owned(),
            };
        }
    }
    let mut raw = text.to_owned();
    if raw.len() > MAX_RAW {
        raw.truncate(MAX_RAW);
        raw.push('…');
    }
    TransportError::Status {
        status,
        code: String::new(),
        message: raw,
    }
}
