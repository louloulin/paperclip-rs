//! 连接引导：`POST /v1.0/gateway/connections/open` 换一条可拨的 `wss://…?ticket=…`
//! （上游 `ws_endpoint.go` 105 行）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 拆出来是**门 ⑩**（800 行硬限）的要求：边界正好是上游那个文件自己的边界。
//!
//! # 三条校验（缺一条就会把一次性票据发到错的地方）
//!
//! 1. `endpoint` / `ticket` 都非空；
//! 2. endpoint 必须是**带 host 的 `wss`**（明文票据等于泄凭据）；
//! 3. `ticket` 按 RFC 3986 的 unreserved 集百分号编码后**追加**到既有 query 之后。
//!
//! # 错误面（`docs/60` §2.3 第 3 条）
//!
//! 上游把响应体原样拼进错误文案；本仓**只带状态码** —— 网关的错误体可能回声请求，而请求里
//! 就带着 `AppSecret`。两条「错误路径不回显凭据」的用例钉住这一条（含本地 loopback 服务端）。

use std::fmt::Write as _;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::channel::{ChannelError, ChannelResult};

use super::{
    BOT_MESSAGE_TOPIC, CONNECTIONS_OPEN_PATH, DEFAULT_API_BASE, FRAME_TYPE_CALLBACK,
    FRAME_TYPE_SYSTEM, MAX_RESPONSE_BODY_BYTES, OPEN_CONNECT_TIMEOUT, STREAM_USER_AGENT,
    SYSTEM_TOPIC_DISCONNECT, SYSTEM_TOPIC_PING,
};

// =====================================================================
// 连接引导（上游 `ws_endpoint.go`）
// =====================================================================

/// 引导请求里的一条 `{type, topic}` 订阅。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSubscription {
    #[serde(rename = "type")]
    pub frame_type: String,
    pub topic: String,
}

/// 机器人消息流的**固定**订阅集（上游 `chatbotSubscriptions`）：两个 `SYSTEM` 控制 topic +
/// 一个回调 topic。顺序是上游的字面顺序（网关不要求，但用例逐字断言它）。
#[must_use]
pub fn chatbot_subscriptions() -> Vec<StreamSubscription> {
    vec![
        StreamSubscription {
            frame_type: FRAME_TYPE_SYSTEM.to_string(),
            topic: SYSTEM_TOPIC_PING.to_string(),
        },
        StreamSubscription {
            frame_type: FRAME_TYPE_SYSTEM.to_string(),
            topic: SYSTEM_TOPIC_DISCONNECT.to_string(),
        },
        StreamSubscription {
            frame_type: FRAME_TYPE_CALLBACK.to_string(),
            topic: BOT_MESSAGE_TOPIC.to_string(),
        },
    ]
}

/// 引导请求体（上游 `openConnectionRequest`）。
///
/// ⚠️ `client_secret` 是**明文 `AppSecret`** ⇒ `Debug` 手写脱敏（`docs/60` §2.3 第 1 条）。
/// `client_id`（`AppKey`）**不是**密钥：上游把它明文存在 `config->>'app_id'` 里，且它是
/// per-installation 的路由键。
#[derive(Clone, Serialize)]
pub struct OpenConnectionRequest {
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "clientSecret")]
    pub client_secret: String,
    pub subscriptions: Vec<StreamSubscription>,
    #[serde(rename = "ua")]
    pub user_agent: String,
}

impl std::fmt::Debug for OpenConnectionRequest {
    /// 手写脱敏：只交代**哪个**字段是密钥，绝不打印它的值。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenConnectionRequest")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("subscriptions", &self.subscriptions)
            .field("user_agent", &self.user_agent)
            .finish()
    }
}

/// 组装引导请求体（纯函数：用例不需要网络就能钉住它的逐字形态）。
#[must_use]
pub fn build_open_request(app_key: &str, app_secret: &str) -> OpenConnectionRequest {
    OpenConnectionRequest {
        client_id: app_key.to_string(),
        client_secret: app_secret.to_string(),
        subscriptions: chatbot_subscriptions(),
        user_agent: STREAM_USER_AGENT.to_string(),
    }
}

/// 引导响应（上游 `openConnectionResponse`）：单次可用的 `endpoint` + `ticket`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenConnectionResponse {
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub ticket: String,
}

/// 把引导响应变成**可拨**的 `wss://…?ticket=…`（上游 `openConnection` 的最后一段）。
///
/// 三条校验（缺一条就会把一次性票据发到错的地方）：
/// 1. `endpoint` / `ticket` 都非空；
/// 2. endpoint 必须是**带 host 的 `wss`**（`ws://` / `http://` 一律拒 —— 明文票据等于泄凭据）；
/// 3. `ticket` 按 RFC 3986 的 unreserved 集做百分号编码后**追加**到既有 query 之后
///    （上游走 `url.Values.Encode()`；空集没有尾 `?`）。
///
/// # Errors
///
/// 上述任一条不成立 ⇒ [`ChannelError::Transport`]（错误文案**不带** endpoint / ticket）。
pub fn dial_url_from_response(response: &OpenConnectionResponse) -> ChannelResult<String> {
    if response.endpoint.is_empty() || response.ticket.is_empty() {
        return Err(ChannelError::Transport {
            message: "dingtalk stream: open connection returned an empty endpoint or ticket"
                .to_string(),
        });
    }
    let Some(rest) = response.endpoint.strip_prefix("wss://") else {
        return Err(ChannelError::Transport {
            message: "dingtalk stream: open connection returned a non-secure websocket endpoint"
                .to_string(),
        });
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if authority_end == 0 {
        return Err(ChannelError::Transport {
            message: "dingtalk stream: open connection returned an endpoint without a host"
                .to_string(),
        });
    }
    let separator = if response.endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    Ok(format!(
        "{}{separator}ticket={}",
        response.endpoint,
        percent_encode_query(&response.ticket)
    ))
}

/// RFC 3986 的 unreserved 集 + `%XX` 转义（Go `url.Values.Encode` 的口径：空白也转义而不是
/// 变成 `+`；票据是 Base64 一类串，两种口径都能还原，转义是更保守的那一种）。
fn percent_encode_query(value: &str) -> String {
    const KEEP: &[u8] = b"-_.~";
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || KEEP.contains(byte) {
            out.push(*byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// 引导接缝（上游 `openConnection(ctx, httpClient, apiBase, appKey, appSecret)`）。
///
/// 端口化的理由与 engine 的其它端口一致：**用例不该开真 socket**。生产实现是
/// [`ReqwestOpener`]（本地 loopback 用例也走它，验的是真 `reqwest` 路径）。
///
/// **错误面**：实现**不得**把响应体写进错误（上游把 `string(body)` 拼进错误文案；本仓按
/// `docs/60` §2.3 第 3 条只带状态码 —— 网关的错误体可能回声请求，而请求里就带着 `AppSecret`）。
#[async_trait]
pub trait ConnectionOpener: Send + Sync {
    /// 拿一条可拨的 `wss://…?ticket=…`。
    ///
    /// # Errors
    ///
    /// 网络失败 / 非 2xx / 响应体解不开 / 端点校验不过 ⇒ [`ChannelError::Transport`]。
    async fn open(&self, app_key: &str, app_secret: &str) -> ChannelResult<String>;
}

/// 生产引导：`reqwest` POST [`CONNECTIONS_OPEN_PATH`]，5 秒超时（上游 `openConnectTimeout`）。
#[derive(Debug, Clone)]
pub struct ReqwestOpener {
    api_base: String,
    client: reqwest::Client,
}

impl Default for ReqwestOpener {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestOpener {
    /// 生产形态（[`DEFAULT_API_BASE`]）。
    #[must_use]
    pub fn new() -> Self {
        Self::with_api_base(DEFAULT_API_BASE)
    }

    /// 换基址（用例的 loopback 服务端靠它；与 slack 的 `api_base()` / M8-1 的
    /// `GITHUB_API_BASE` 同款做法）。
    #[must_use]
    pub fn with_api_base(api_base: impl Into<String>) -> Self {
        Self {
            api_base: api_base.into(),
            client: reqwest::Client::builder()
                .timeout(OPEN_CONNECT_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// 本实现的基址（诊断用）。
    #[must_use]
    pub fn api_base(&self) -> &str {
        &self.api_base
    }
}

#[async_trait]
impl ConnectionOpener for ReqwestOpener {
    async fn open(&self, app_key: &str, app_secret: &str) -> ChannelResult<String> {
        send_open_request(&self.client, &self.api_base, app_key, app_secret).await
    }
}

/// `POST /v1.0/gateway/connections/open` 的实际请求（拆出来是为了让"5 秒超时"那一层与
/// "怎么组装 URL"那一层各自可测）。
///
/// # Errors
///
/// 网络失败 / 非 2xx / 响应体解不开 / 端点校验不过 ⇒ [`ChannelError::Transport`]。
pub async fn send_open_request(
    client: &reqwest::Client,
    api_base: &str,
    app_key: &str,
    app_secret: &str,
) -> ChannelResult<String> {
    let request = build_open_request(app_key, app_secret);
    let url = format!("{}{CONNECTIONS_OPEN_PATH}", api_base.trim_end_matches('/'));
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|error| ChannelError::Transport {
            // 只带 reqwest 的错误**类别**：`Display` 里可能回显 URL（本处 URL 不带凭据，
            // 但错误文案一律不含它，免得将来把带 ticket 的 URL 也拼进去）。
            message: format!(
                "dingtalk stream: open connection request failed ({})",
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
            message: "dingtalk stream: open connection response is unreadable".to_string(),
        })?;
    if body.len() > MAX_RESPONSE_BODY_BYTES {
        return Err(ChannelError::Transport {
            message: "dingtalk stream: open connection response is too large".to_string(),
        });
    }
    if !status.is_success() {
        // ⚠️ **不**回显响应体（见 [`ConnectionOpener`] 的错误面）。
        return Err(ChannelError::Transport {
            message: format!(
                "dingtalk stream: open connection failed with status {}",
                status.as_u16()
            ),
        });
    }
    let decoded: OpenConnectionResponse =
        serde_json::from_slice(&body).map_err(|_| ChannelError::Transport {
            message: "dingtalk stream: open connection response is not the expected JSON"
                .to_string(),
        })?;
    dial_url_from_response(&decoded)
}
