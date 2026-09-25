//! Slack 的 **Socket Mode 传输面**：信封帧 + 建连端口 + [`SlackChannel`] + 工厂。
//!
//! - **写者**：M7-3（本片；`docs/60-M7-PLAN.md` §3.3 的写集勘误 —— 见 `slack/mod.rs` 的
//!   「写集勘误」一节：门 ⑩ 的 800 行硬限要求把 `inbound.rs` 的「归一化 / 传输」两半拆开）。
//! - **上游**：`internal/integrations/slack/slack_channel.go`（268 行）的接收循环那一半。
//! - 归一化（事件 → [`mc_core::channel::message::InboundMessage`]）在
//!   [`crate::slack::inbound`]；本文件只管"怎么把帧拿到手、什么时候 ACK、链路断了怎么办"。
//!
//! # ACK 在投递**之前**
//!
//! Slack 对未 ACK 的信封 ~3s 就过期，远短于 handler 的 DB 工作 ⇒ ACK 与 handler 的判决**无关**；
//! 相反，**先投递后 ACK** 会让一次慢查询把消息变成"对方重投"。所以
//! [`SlackChannel::connect`] 的循环顺序是：解码 → 判决 → ACK → 投递。
//!
//! # 失败模式（三条，都有用例）
//!
//! 1. 坏帧（非 JSON / 缺字段）⇒ **告警并继续**：一帧坏了不该拆掉整条链路；
//! 2. handler 返回非 `Ok` ⇒ 基础设施失败 ⇒ 退出循环，让 supervisor 退避重连；
//! 3. `disconnect` 帧 / 流结束 ⇒ 退出循环（同一个出口）。
//!
//! # 出站**不在**本文件
//!
//! `Channel::send` 的实现归 **M7-4**；本片把它**失败关闭**（明说未接线），而不是交一个发不出去
//! 却自称 `TEXT` 的半成品。

use async_trait::async_trait;
use mc_core::channel::message::{InboundMessage, OutboundMessage, SendResult};
use mc_core::channel::ChannelKind;
use serde_json::Value;

use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult, Factory};
use crate::message::SharedInboundHandler;
use crate::slack::config::{
    decrypt_token, Decrypter, InstallConfig, Sensitive, SlackDeps, FIELD_APP_TOKEN, FIELD_BOT_TOKEN,
};
use crate::slack::inbound::{inbound_from_event, parse_events_api, TYPE_SLACK};

/// Slack `apps.connections.open` 的端点（Socket Mode 的引导接口）。
const CONNECTIONS_OPEN_URL: &str = "https://slack.com/api/apps.connections.open";

// =====================================================================
// Socket Mode 信封
// =====================================================================

/// 一帧 Socket Mode 信封（上游 `socketmode.Event` 的类型分支）。
#[derive(Debug, Clone, PartialEq)]
pub enum SocketFrame {
    /// 连接成功后的握手帧（**不** ACK）。
    Hello,
    /// Events API 事件（要 ACK，且 ACK 在投递**之前**）。
    EventsApi { envelope_id: String, payload: Value },
    /// 斜杠命令（要 ACK；处理归 M7-4）。
    SlashCommand { envelope_id: String },
    /// 交互回调（要 ACK；处理归 M7-4）。
    Interactive { envelope_id: String },
    /// 对端要求重连（**不** ACK）。
    Disconnect { reason: String },
    /// 认得出类型但本片不处理的信封（要 ACK）。
    Other {
        kind: String,
        envelope_id: Option<String>,
    },
}

impl SocketFrame {
    /// 该帧的 `envelope_id`（有就要回 ACK）。
    #[must_use]
    pub fn envelope_id(&self) -> Option<&str> {
        match self {
            Self::EventsApi { envelope_id, .. }
            | Self::SlashCommand { envelope_id }
            | Self::Interactive { envelope_id } => Some(envelope_id),
            Self::Other { envelope_id, .. } => envelope_id.as_deref(),
            Self::Hello | Self::Disconnect { .. } => None,
        }
    }

    /// 是否需要回 ACK（`disconnect` / `hello` 不回）。
    #[must_use]
    pub fn needs_ack(&self) -> bool {
        !matches!(self, Self::Hello | Self::Disconnect { .. })
    }
}

/// 帧解析失败（上游由 `socketmode` 库吞掉的那些形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// 不是合法 JSON。
    #[error("slack: socket frame is not valid JSON")]
    NotJson,
    /// 没有 `type` 字段。
    #[error("slack: socket frame has no type")]
    MissingType,
    /// `events_api` 帧缺 `envelope_id`（没法 ACK）。
    #[error("slack: events_api frame has no envelope_id")]
    MissingEnvelopeId,
    /// `events_api` 帧缺 `payload`。
    #[error("slack: events_api frame has no payload")]
    MissingPayload,
}

/// 解析一帧文本（上游 `socketmode` 的 `Event` 解码）。
pub fn parse_socket_frame(text: &str) -> Result<SocketFrame, FrameError> {
    let value: Value = serde_json::from_str(text).map_err(|_| FrameError::NotJson)?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or(FrameError::MissingType)?;
    let envelope_id = value
        .get("envelope_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(match kind {
        "hello" => SocketFrame::Hello,
        "events_api" => SocketFrame::EventsApi {
            envelope_id: envelope_id.ok_or(FrameError::MissingEnvelopeId)?,
            payload: value
                .get("payload")
                .cloned()
                .ok_or(FrameError::MissingPayload)?,
        },
        "slash_commands" => SocketFrame::SlashCommand {
            envelope_id: envelope_id.ok_or(FrameError::MissingEnvelopeId)?,
        },
        "interactive" => SocketFrame::Interactive {
            envelope_id: envelope_id.ok_or(FrameError::MissingEnvelopeId)?,
        },
        "disconnect" => SocketFrame::Disconnect {
            reason: value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("disconnected")
                .to_owned(),
        },
        other => SocketFrame::Other {
            kind: other.to_owned(),
            envelope_id,
        },
    })
}

/// ACK 帧的正文（`{"envelope_id": "…"}`，上游 `socketmode.Ack` 的线形态）。
#[must_use]
pub fn ack_json(envelope_id: &str) -> String {
    serde_json::json!({ "envelope_id": envelope_id }).to_string()
}

/// 一帧该对连接做什么（接收循环的判决表，**纯函数**）。
#[derive(Debug, Clone, PartialEq)]
pub enum FrameAction {
    /// 什么都不做（握手帧 / 不处理的信封类型 / 认不出的事件）。
    Ignore,
    /// 投递给 engine。
    Dispatch(Box<InboundMessage>),
    /// 退出接收循环，让 supervisor 退避重连。
    Reconnect(String),
}

/// 判决一帧（先 ACK 后投递的顺序由调用方保证，见 [`SlackChannel::connect`]）。
#[must_use]
pub fn dispatch_frame(frame: &SocketFrame, bot_user_id: &str) -> FrameAction {
    match frame {
        SocketFrame::EventsApi { payload, .. } => {
            match parse_events_api(payload)
                .and_then(|event| inbound_from_event(&event, bot_user_id))
            {
                Some(message) => FrameAction::Dispatch(Box::new(message)),
                None => FrameAction::Ignore,
            }
        }
        SocketFrame::Disconnect { reason } => FrameAction::Reconnect(reason.clone()),
        SocketFrame::Hello
        | SocketFrame::SlashCommand { .. }
        | SocketFrame::Interactive { .. }
        | SocketFrame::Other { .. } => FrameAction::Ignore,
    }
}

// =====================================================================
// 传输端口（上游 `socketmode.Client`）
// =====================================================================

/// 一条已经建好的 Socket Mode 会话。
#[async_trait]
pub trait SocketSession: Send {
    /// 下一条文本帧；`None` = 对端关闭了流。
    async fn next_text(&mut self) -> Option<ChannelResult<String>>;
    /// 发一条文本帧（ACK）。
    async fn send_text(&mut self, text: &str) -> ChannelResult<()>;
}

/// 建连端口：`apps.connections.open` 引导 + WebSocket 握手。
///
/// 端口化的理由与 engine 的其它端口一致：**用例不该开真 socket**。生产实现是
/// [`TungsteniteTransport`]；M7-4 的端到端回路（`docs/60` §4.2）在本地 WS 服务端上跑。
#[async_trait]
pub trait SocketTransport: Send + Sync {
    /// 用本安装的 `xapp-` 令牌建一条 Socket Mode 会话（阻塞级：直到拿到会话才返回）。
    async fn connect(&self, app_token: &str) -> ChannelResult<Box<dyn SocketSession>>;
}

/// 生产传输：`reqwest` 引导 + `tokio-tungstenite` 连接。
#[derive(Debug, Default, Clone)]
pub struct TungsteniteTransport;

#[async_trait]
impl SocketTransport for TungsteniteTransport {
    async fn connect(&self, app_token: &str) -> ChannelResult<Box<dyn SocketSession>> {
        let url = open_connection(app_token).await?;
        // 连接失败的错误**不**原样透出：`connect_async` 的错里带完整 URL，
        // 而那条 URL 自带票据（等价于凭据）。
        let (stream, _response) =
            tokio_tungstenite::connect_async(url)
                .await
                .map_err(|_| ChannelError::Transport {
                    message: "slack: socket mode handshake failed".to_string(),
                })?;
        Ok(Box::new(TungsteniteSession { stream }))
    }
}

/// `apps.connections.open`：拿一条 `wss://` 引导 URL。
async fn open_connection(app_token: &str) -> ChannelResult<String> {
    let response = reqwest::Client::new()
        .post(CONNECTIONS_OPEN_URL)
        .bearer_auth(app_token)
        .send()
        .await
        .map_err(|_| ChannelError::Transport {
            message: "slack: apps.connections.open request failed".to_string(),
        })?;
    let body: Value = response.json().await.map_err(|_| ChannelError::Transport {
        message: "slack: apps.connections.open returned a non-JSON body".to_string(),
    })?;
    if body.get("ok").and_then(Value::as_bool) != Some(true) {
        // 只带 Slack 自己的错误码（`invalid_auth` 一类），**绝不**带令牌。
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        return Err(ChannelError::Auth {
            message: format!("slack: apps.connections.open refused ({code})"),
        });
    }
    body.get("url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ChannelError::Transport {
            message: "slack: apps.connections.open returned no url".to_string(),
        })
}

/// `tokio-tungstenite` 承载的会话。
struct TungsteniteSession {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

#[async_trait]
impl SocketSession for TungsteniteSession {
    async fn next_text(&mut self) -> Option<ChannelResult<String>> {
        use futures_util::StreamExt as _;

        loop {
            match self.stream.next().await? {
                Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                    return Some(Ok(text));
                }
                Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes)) => {
                    return Some(
                        String::from_utf8(bytes).map_err(|_| ChannelError::Transport {
                            message: "slack: binary frame is not UTF-8".to_string(),
                        }),
                    );
                }
                // Ping / Pong / 其它控制帧由库自己处理；继续读。
                Ok(_) => {}
                Err(_) => {
                    return Some(Err(ChannelError::Transport {
                        message: "slack: socket read failed".to_string(),
                    }));
                }
            }
        }
    }

    async fn send_text(&mut self, text: &str) -> ChannelResult<()> {
        use futures_util::SinkExt as _;

        self.stream
            .send(tokio_tungstenite::tungstenite::Message::text(text))
            .await
            .map_err(|_| ChannelError::Transport {
                message: "slack: socket write failed".to_string(),
            })
    }
}

// =====================================================================
// Channel 实现
// =====================================================================

/// **一个安装的** Socket Mode 连接（上游 `slackChannel`）。
///
/// `connect` 阻塞跑接收循环；`disconnect` 是 no-op（连接的整个生命周期都圈在 `connect` 里，
/// 返回即已释放 —— 与上游 `feishuChannel.Disconnect` 同形）。
pub struct SlackChannel {
    app_id: String,
    bot_user_id: String,
    /// 明文 `xapp-`（鉴权 Socket Mode 连接）。**手写脱敏**类型。
    app_token: Sensitive,
    /// 明文 `xoxb-`（出站 Web API 用；send 归 M7-4）。
    bot_token: Sensitive,
    handler: Option<SharedInboundHandler>,
    transport: std::sync::Arc<dyn SocketTransport>,
}

impl std::fmt::Debug for SlackChannel {
    /// 手写脱敏：两个令牌都是 [`Sensitive`]（`Debug` 输出 `<redacted>`）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackChannel")
            .field("app_id", &self.app_id)
            .field("bot_user_id", &self.bot_user_id)
            .field("app_token", &self.app_token)
            .field("bot_token", &self.bot_token)
            .field("has_handler", &self.handler.is_some())
            .finish_non_exhaustive()
    }
}

impl SlackChannel {
    /// 装配一条连接（`handler` = engine 注入的共享入站入口）。
    #[must_use]
    pub fn new(
        app_id: impl Into<String>,
        bot_user_id: impl Into<String>,
        app_token: impl Into<String>,
        bot_token: impl Into<String>,
        handler: Option<SharedInboundHandler>,
        transport: std::sync::Arc<dyn SocketTransport>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            bot_user_id: bot_user_id.into(),
            app_token: Sensitive::new(app_token),
            bot_token: Sensitive::new(bot_token),
            handler,
            transport,
        }
    }

    /// 本安装的 Slack app id（路由键）。
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// 本安装的 bot user id（提及判定用）。
    #[must_use]
    pub fn bot_user_id(&self) -> &str {
        &self.bot_user_id
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn kind(&self) -> ChannelKind {
        TYPE_SLACK
    }

    /// 建连并跑接收循环（见模块文档的 push 语义）。
    ///
    /// - ACK **先**发、投递**后**做：Slack 对未 ACK 的信封 ~3s 就过期，远短于 handler 的
    ///   DB 工作，所以 ACK 与 handler 的判决**无关**；
    /// - 坏帧（非 JSON / 缺字段）只告警并继续：一帧坏了不该拆掉整条链路；
    /// - 非 `Ok` 的 handler 判决 = 基础设施失败 ⇒ 退出让 supervisor 退避重连。
    async fn connect(&self) -> ChannelResult<()> {
        let Some(handler) = self.handler.clone() else {
            return Err(ChannelError::Transport {
                message: "slack: inbound handler not configured".to_string(),
            });
        };
        if self.app_token.is_empty() {
            return Err(ChannelError::Transport {
                message: "slack: app-level token not configured".to_string(),
            });
        }
        let mut session = self.transport.connect(self.app_token.expose()).await?;
        loop {
            let Some(frame) = session.next_text().await else {
                return Err(ChannelError::Transport {
                    message: "slack: socket mode event stream closed".to_string(),
                });
            };
            let text = frame?;
            let envelope = match parse_socket_frame(&text) {
                Ok(envelope) => envelope,
                Err(error) => {
                    tracing::warn!(
                        app_id = self.app_id,
                        error = %error,
                        "slack: undecodable socket mode frame skipped"
                    );
                    continue;
                }
            };
            let action = dispatch_frame(&envelope, &self.bot_user_id);
            if envelope.needs_ack() {
                if let Some(envelope_id) = envelope.envelope_id() {
                    if let Err(error) = session.send_text(&ack_json(envelope_id)).await {
                        // ACK 是尽力而为（上游：`slog.Warn` 后继续）。
                        tracing::warn!(
                            app_id = self.app_id,
                            error = %error,
                            "slack: ack failed"
                        );
                    }
                }
            }
            match action {
                FrameAction::Ignore => {}
                FrameAction::Dispatch(message) => handler.handle(*message).await?,
                FrameAction::Reconnect(reason) => {
                    return Err(ChannelError::Transport {
                        message: format!("slack: socket mode disconnect: {reason}"),
                    });
                }
            }
        }
    }

    /// 无长期资源（连接的生命周期圈在 [`SlackChannel::connect`] 里）。
    async fn disconnect(&self) -> ChannelResult<()> {
        Ok(())
    }

    /// 出站归 **M7-4**（`slack/{outbound.rs,replier.rs}`）。
    ///
    /// 本片**失败关闭**：明说未接线，而不是假装发成功。选 `Transport` 是因为它表达的正是
    /// "这条出站链路还不存在"（`send` 不在 supervisor 的退避路径上，不会被误重试）。
    async fn send(&self, _out: OutboundMessage) -> ChannelResult<SendResult> {
        Err(ChannelError::Transport {
            message: "slack: outbound (chat.postMessage) is not wired yet — lands in M7-4"
                .to_string(),
        })
    }

    /// 上游 `CapText | CapThreadReply`（出站面归 M7-4，位图先照上游声明）。
    fn capabilities(&self) -> Capability {
        Capability::TEXT.union(Capability::THREAD_REPLY)
    }
}

// =====================================================================
// 工厂
// =====================================================================

/// 造本平台工厂（上游 `newSlackFactory`）。
///
/// 工厂**校验**配置并返回 `Err`，而不是交出半成品（[`crate::channel::Factory`] 的契约）：
/// 配置解不开、`xapp-` 解不开、`xapp-` 为空 —— 三种都在这里拒掉。
#[must_use]
pub fn factory(deps: &SlackDeps) -> Factory {
    let deps = deps.clone();
    std::sync::Arc::new(move |config: ChannelConfig| {
        let cfg: InstallConfig = serde_json::from_value(config.raw.clone()).map_err(|error| {
            ChannelError::InvalidConfig {
                kind: TYPE_SLACK.as_str().to_string(),
                reason: format!("decode installation config failed at {error}"),
            }
        })?;
        let app_token = decrypt_token(&cfg.app_token_encrypted, FIELD_APP_TOKEN, &deps.decrypt)
            .map_err(|error| ChannelError::InvalidConfig {
                kind: TYPE_SLACK.as_str().to_string(),
                reason: error.to_string(),
            })?;
        if app_token.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_SLACK.as_str().to_string(),
                reason: "installation has no app-level token".to_string(),
            });
        }
        let bot_token = decrypt_token(&cfg.bot_token_encrypted, FIELD_BOT_TOKEN, &deps.decrypt)
            .map_err(|error| ChannelError::InvalidConfig {
                kind: TYPE_SLACK.as_str().to_string(),
                reason: error.to_string(),
            })?;
        Ok(std::sync::Arc::new(SlackChannel::new(
            cfg.app_id,
            cfg.bot_user_id,
            app_token,
            bot_token,
            config.handler,
            std::sync::Arc::new(TungsteniteTransport),
        )) as std::sync::Arc<dyn Channel>)
    })
}

/// 工厂的显式解密器形态（`register` 的默认值见 [`crate::slack::register`]）。
#[must_use]
pub fn factory_with_decrypter(decrypt: Decrypter) -> Factory {
    factory(&SlackDeps { decrypt })
}

#[cfg(test)]
mod tests;
