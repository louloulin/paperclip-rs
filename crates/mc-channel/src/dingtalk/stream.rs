//! `DingTalk` **Stream 传输面**：帧编解码 + 连接引导 + 帧服务循环（上游 `ws_frame.go` 67 行 +
//! `ws_endpoint.go` 105 行 + `ws_connector.go` 226 行 = 398 行，共 3 个上游文件）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 归一化（回调 → [`mc_core::channel::message::InboundMessage`]）在
//!   [`crate::dingtalk::inbound`]；本文件只管"怎么把帧拿到手、什么时候回什么、链路断了怎么办"。
//!
//! # 三段，各自可测
//!
//! | 段 | 上游 | 形态 | 用例形态 |
//! | --- | --- | --- | --- |
//! | 帧编解码（[`DataFrame`] / [`DataFrameResponse`]） | `ws_frame.go` | 纯数据 + 两个构造函数 | 逐字段 JSON 断言（`messageId` 回声是网关的**相关键**，不能丢） |
//! | 连接引导（[`build_open_request`] / [`dial_url_from_response`] / [`ConnectionOpener`]） | `ws_endpoint.go` | 纯函数 + 端口（`reqwest` 实现） | 纯函数表驱动 + 本地 loopback HTTP 服务端（真 `reqwest` 路径） |
//! | 帧服务循环（[`Connector::run_session`]） | `ws_connector.go` | 单所有者循环 + [`WsConnection`] 端口 | 脚本化内存 socket（不睡真觉 / 不开真 socket） |
//!
//! # 连接的生命周期**不在**这里（写进契约，别在本文件加重连）
//!
//! 重连 / 退避 / 租约归 `engine::Supervisor`（上游注释逐字）。所以
//! [`Connector::run_session`] 只跑**一条** socket 会话：
//!
//! - 网关发 `SYSTEM/disconnect` ⇒ 干净返回 [`SessionOutcome::DisconnectRequested`]（supervisor 重拨）；
//! - 停机信号置位 ⇒ [`SessionOutcome::Cancelled`]（**不是错误**，对齐 [`crate::channel::Channel`]
//!   的取消语义）；
//! - 读失败 / 读超时 / 流结束 ⇒ `Err`（supervisor 按"这次尝试失败"退避重连）。
//!
//! # 凭据面（`docs/60` §2.3 的四条判据，逐条落在这里）
//!
//! 1. 承载密钥的类型 [`AppSecret`] **手写 `Debug`** 输出 `<redacted>`；[`OpenConnectionRequest`]
//!    的 `Debug` 同款（它的 `clientSecret` 字段就是明文 `AppSecret`）；
//! 2. 任何 `tracing::*` 调用**不插值**这两者（本文件只插值 `type` / `topic` / 状态码）；
//! 3. 两条「错误路径不回显凭据」的用例：拨号失败**不**透出 `WsDialer` 的错误（`dial_url` 自带
//!    一次性 ticket，等价于凭据 —— 上游注释逐字点明这条），引导失败**不**回显响应体
//!    （上游把响应体原样写进错误，本仓只带状态码 ⇒ 见 [`ConnectionOpener`] 的文档）；
//! 4. 键名进 redaction 表的检查在 `docs/60` §2.3 第 4 条那一侧（`mc-telemetry`），本文件不新增
//!    日志字段。
//!
//! # 与上游的三处形态差异（登记 `docs/32` §19 的 D 项）
//!
//! 1. **读截止的重置点**：上游 `SetReadDeadline(now+90s)` 在**每次** `ReadMessage` 之前设一次，
//!    且 pong 处理器再设一次。本仓的读是一次 `timeout`，靠 [`WsEvent`] 把 pong **显式**交给循环
//!    ⇒ 每收到一个事件（文本 / pong）就刷新一次截止。语义等价（"空闲但健康 ⇒ 不超时"），
//!    且能被用例钉住（见 `stream/tests.rs` 的 `a_pong_refreshes_the_read_deadline`）。
//! 2. **单所有者写**：上游的 ping 走 `WriteControl`（gorilla 下并发安全），与读并发；本仓把 ping
//!    与读放进同一个 `select!`（一个连接只有一个所有者）。代价是"写 ping 的那一瞬不读"，
//!    收益是不需要把 socket 拆成两半（`&mut self` 的端口因此可用，用例的替身也简单）。
//! 3. **`ping` 事件的处置**：gorilla 自动回 pong；`tokio-tungstenite` 同样自动回（其文档逐字：
//!    "the library will automatically reply to Ping frames with Pong frames"）⇒ 本仓读到
//!    [`WsEvent::Ping`] 只刷新读截止、不显式回写。

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::channel::{ChannelError, ChannelResult};

use super::inbound::BotCallbackData;

pub mod endpoint;
pub mod tungstenite;

pub use endpoint::{
    build_open_request, chatbot_subscriptions, dial_url_from_response, send_open_request,
    ConnectionOpener, OpenConnectionRequest, OpenConnectionResponse, ReqwestOpener,
    StreamSubscription,
};
pub use tungstenite::TungsteniteDialer;

// =====================================================================
// 帧词表（上游 `ws_frame.go` 的常量）
// =====================================================================

/// 网关控制帧的类型判别式：连接级控制（`ping` / `disconnect`）。
pub const FRAME_TYPE_SYSTEM: &str = "SYSTEM";
/// 业务回调帧的类型判别式。
pub const FRAME_TYPE_CALLBACK: &str = "CALLBACK";

/// `SYSTEM` 帧的心跳 topic（收到要回 pong）。
pub const SYSTEM_TOPIC_PING: &str = "ping";
/// `SYSTEM` 帧的"请重连"topic（收到要**干净返回**，让 supervisor 重拨）。
pub const SYSTEM_TOPIC_DISCONNECT: &str = "disconnect";
/// 机器人消息回调的 topic（`CALLBACK` 帧里唯一被本 adapter 消费的那个）。
pub const BOT_MESSAGE_TOPIC: &str = "/v1.0/im/bot/messages/get";

/// 回帧里 `code` 的成功值。
pub const FRAME_RESPONSE_CODE_OK: i32 = 200;

/// 心跳间隔（上游 `streamPingInterval`）。
pub const STREAM_PING_INTERVAL: Duration = Duration::from_secs(30);
/// 读截止（上游 `streamReadDeadline`）：超过它没有任何事件 ⇒ 判定链路已死。
pub const STREAM_READ_DEADLINE: Duration = Duration::from_secs(90);
/// 一次写的上限（上游 `streamWriteTimeout`）。
pub const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// 引导端点的路径（上游 `connectionsOpenPath`）。
pub const CONNECTIONS_OPEN_PATH: &str = "/v1.0/gateway/connections/open";
/// 引导请求的 `ua`（上游 `streamUserAgent`）。
pub const STREAM_USER_AGENT: &str = "multica-dingtalk/1.0";
/// 引导请求的超时（上游 `openConnectTimeout`）。
pub const OPEN_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 生产 API 基址（上游由共享出站 Client 给出；用例换基址见 [`ReqwestOpener::with_api_base`]）。
pub const DEFAULT_API_BASE: &str = "https://api.dingtalk.com";
/// 响应体读入的上限（上游 `io.LimitReader(resp.Body, 1<<20)`）。
const MAX_RESPONSE_BODY_BYTES: usize = 1 << 20;

// =====================================================================
// 凭据（手写脱敏）
// =====================================================================

/// **明文** `AppSecret`：`Debug` 只输出 `<redacted>`，明文只能经 [`AppSecret::expose`] 显式取出
/// （`docs/60` §2.3 第 1 条）—— 于是"不小心把它插进日志"在**类型层面**就做不到。
///
/// ⚠️ 这是本 crate 的**第三份**同形件（`slack::config::Sensitive` / `telegram::config::Sensitive`）。
/// 收敛（提到一个共享模块）要动 M7-3/4/5 的已合文件 ⇒ **不在本片写集**；本片的形态与 M7-9 的
/// `dingtalk/config.rs`（上游 `config.go`）合并时一并收敛，登记在 `docs/32` §19 的 D 项。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AppSecret(String);

impl std::fmt::Debug for AppSecret {
    /// 手写脱敏：**任何**格式化路径都拿不到明文。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AppSecret(<redacted>)")
    }
}

impl AppSecret {
    /// 包一个明文。
    #[must_use]
    pub fn new(plaintext: impl Into<String>) -> Self {
        Self(plaintext.into())
    }

    /// 取出明文（**唯一**出口：调用点因此总是显式可见的）。
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 是否为空（含空串 = 未配置）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// =====================================================================
// 帧（上游 `ws_frame.go`）
// =====================================================================

/// 一条入站 Stream 帧。网关把路由键放在 `headers.topic`、相关键放在 `headers.messageId`；
/// `data` 是帧体（机器人消息 topic 下就是一次回调的 JSON **字符串**）。
///
/// `headers` 用 [`BTreeMap`]（不是 `serde_json::Map`）：诊断与用例要**确定性**的键序。
/// 帧里任一 header 不是字符串 ⇒ 整帧解码失败（与上游 `map[string]string` 同款）—— 调用方按
/// "坏帧告警并继续"处置，别在这里放宽。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFrame {
    #[serde(rename = "type", default)]
    pub frame_type: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub data: String,
}

impl DataFrame {
    /// 路由键（`headers.topic`；缺席 ⇒ 空串）。
    #[must_use]
    pub fn topic(&self) -> &str {
        self.headers.get("topic").map_or("", String::as_str)
    }

    /// 相关键（`headers.messageId`）：**回帧必须原样回声**，网关靠它配对。
    #[must_use]
    pub fn message_id(&self) -> &str {
        self.headers.get("messageId").map_or("", String::as_str)
    }
}

/// 回帧（上游 `dataFrameResponse`）：每条入站帧都要回一条。
///
/// 网关靠回声的 `messageId` 把回帧与投递的帧配对 ⇒ 那个 header 是**承重**的；普通回调的
/// `data` 保持空（真正的回复经 Open API **带外**投递）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFrameResponse {
    pub code: i32,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: String,
}

/// 普通回调的 200 ack（回声 `messageId`，`message` / `data` 空）。
#[must_use]
pub fn new_ack_response(message_id: &str) -> DataFrameResponse {
    DataFrameResponse {
        code: FRAME_RESPONSE_CODE_OK,
        headers: response_headers(message_id),
        message: String::new(),
        data: String::new(),
    }
}

/// `SYSTEM/ping` 的 pong（回声 `messageId` **与** `data` —— 网关期望这个 shape）。
#[must_use]
pub fn new_pong_response(message_id: &str, data: &str) -> DataFrameResponse {
    DataFrameResponse {
        code: FRAME_RESPONSE_CODE_OK,
        headers: response_headers(message_id),
        message: "ok".to_string(),
        data: data.to_string(),
    }
}

fn response_headers(message_id: &str) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("messageId".to_string(), message_id.to_string());
    headers.insert("contentType".to_string(), "application/json".to_string());
    headers
}

// =====================================================================
// WebSocket 端口（上游 `wsConn` / `wsDialer` 两个接口）
// =====================================================================

/// 一次读到的 WS 事件。
///
/// 控制帧**显式**上抛（不像 slack 那样由库吞掉）：读截止靠"每个事件刷新一次"实现（见模块
/// 文档差异 1），所以 `Pong` 不能在这里被吃掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsEvent {
    /// 文本帧（唯一会被解码的载荷）。
    Text(String),
    /// 二进制帧（本协议不用；上抛以便循环刷新读截止）。
    Binary(Vec<u8>),
    /// 对端 ping（`tokio-tungstenite` 自动回 pong；本循环只刷新读截止）。
    Ping,
    /// 对端 pong（心跳的应答 ⇒ 链路健康）。
    Pong,
    /// 对端关闭。
    Closed,
}

/// 一条 WS 会话的最小面（上游 `wsConn` 接口的逐方法对应）。
///
/// 抽成 trait 是为了让用例注入脚本化的内存 socket（上游注释逐字：`tests can inject a fake`）。
#[async_trait]
pub trait WsConnection: Send + Sync {
    /// 读下一个事件；`None` = 流结束（无更多帧）。
    async fn next_event(&mut self) -> Option<ChannelResult<WsEvent>>;
    /// 写一帧文本。
    async fn send_text(&mut self, text: &str) -> ChannelResult<()>;
    /// 写一个 ping（心跳；上游 `WriteControl(PingMessage)`）。
    async fn send_ping(&mut self) -> ChannelResult<()>;
    /// 关闭（幂等）。
    async fn close(&mut self);
}

/// 拨号接缝（上游 `wsDialer` 接口）。
///
/// ⚠️ **实现不得把错误原样透出**：`dial_url` 自带一次性 Stream ticket（等价于凭据），
/// `tokio-tungstenite` 的错误里带完整 URL ⇒ 拨号失败一律映射成**固定文案**（上游逐字）。
#[async_trait]
pub trait WsDialer: Send + Sync {
    /// 拨一条 `wss://…?ticket=…`。
    ///
    /// # Errors
    ///
    /// 握手失败 ⇒ [`ChannelError::Transport`]（文案不含 dial URL）。
    async fn dial(&self, dial_url: &str) -> ChannelResult<Box<dyn WsConnection>>;
}

// =====================================================================
// 停机信号
// =====================================================================

/// 置位端：宿主 / dispatcher 的收口置位后，帧循环**优雅**退出（`connect` 返回 `Ok`）。
#[derive(Debug, Clone)]
pub struct StopSignal(watch::Sender<bool>);

/// 等待端：交给 [`Connector::run_session`]。
#[derive(Debug, Clone)]
pub struct StopHandle(watch::Receiver<bool>);

impl StopSignal {
    /// 造一对（初值 `false` = 未停机）。
    #[must_use]
    pub fn pair() -> (Self, StopHandle) {
        let (sender, receiver) = watch::channel(false);
        (Self(sender), StopHandle(receiver))
    }

    /// 置位（幂等；没有等待者时不是错误 —— 循环可能已经退出了）。
    pub fn stop(&self) {
        let _ = self.0.send(true);
    }

    /// 是否已置位（诊断 / 用例）。
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }
}

impl StopHandle {
    /// 从一条既有的 `watch` 接收端造等待端（例如 dispatcher 的收口信号）。
    #[must_use]
    pub fn from_receiver(receiver: watch::Receiver<bool>) -> Self {
        Self(receiver)
    }

    /// 是否已置位。
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }

    /// 等待置位（已置位 ⇒ 立即返回）。
    pub async fn wait(&mut self) {
        if self.is_stopped() {
            return;
        }
        // `changed()` 只在**新值**到来时返回；`None` = 发送端已丢弃（等价于停机）。
        let _ = self.0.changed().await;
    }
}

// =====================================================================
// 回调汇（归一化那一侧的入口）
// =====================================================================

/// 一条**已解码**的机器人消息回调的消费者。
///
/// 实现在 [`crate::dingtalk::Channel`] 那一侧：它做的是"入队到 per-conversation 串行队列"
/// （[`crate::dingtalk::dispatch`]），于是帧循环**永不**阻塞在 DB / 媒体上（上游 `dispatch.go`
/// 的整个存在理由）。
#[async_trait]
pub trait CallbackSink: Send + Sync {
    /// 消费一条回调。
    ///
    /// # Errors
    ///
    /// 任何错误都**只是**一条日志：本层仍然照常 ACK（见 [`Connector::handle_frame`] 的注释）。
    async fn on_callback(&self, callback: BotCallbackData) -> ChannelResult<()>;
}

// =====================================================================
// 帧服务循环（上游 `ws_connector.go`）
// =====================================================================

/// 时间旋钮（用例调到毫秒级，免得睡真觉）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamKnobs {
    pub ping_interval: Duration,
    pub read_deadline: Duration,
    pub write_timeout: Duration,
}

impl Default for StreamKnobs {
    fn default() -> Self {
        Self {
            ping_interval: STREAM_PING_INTERVAL,
            read_deadline: STREAM_READ_DEADLINE,
            write_timeout: STREAM_WRITE_TIMEOUT,
        }
    }
}

/// 一条会话的收尾方式（`Err` 才是"这次尝试失败"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    /// 停机信号置位（**不是错误**，`connect` 返回 `Ok`）。
    Cancelled,
    /// 网关要求重连（上游：干净返回 `nil`，由 supervisor 重拨）。
    DisconnectRequested,
}

/// 一帧的处理判决（本文件内部；拆出来是为了让每条分支都能被单独断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum FrameAction {
    /// 不认识的帧 / 坏帧 ⇒ 告警继续（**不**拆链路）。
    Ignore,
    /// `SYSTEM/ping` ⇒ pong；写失败是**致命**的（上游：pong 写失败回错误）。
    Pong(DataFrameResponse),
    /// 回调 ⇒ ack；写失败**只告警**（上游：ack 尽力而为）。
    Ack(DataFrameResponse),
    /// `SYSTEM/disconnect` ⇒ 干净返回，让 supervisor 重拨。
    Disconnect,
}

/// 一个安装的 Stream 会话执行体（上游 `wsConnector`）。
pub struct Connector {
    opener: std::sync::Arc<dyn ConnectionOpener>,
    dialer: std::sync::Arc<dyn WsDialer>,
    app_key: String,
    app_secret: AppSecret,
    sink: std::sync::Arc<dyn CallbackSink>,
    knobs: StreamKnobs,
}

impl std::fmt::Debug for Connector {
    /// 手写脱敏：`app_secret` 是 [`AppSecret`]（`<redacted>`），两个端口只打印存在性。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connector")
            .field("opener", &"<dyn ConnectionOpener>")
            .field("dialer", &"<dyn WsDialer>")
            .field("app_key", &self.app_key)
            .field("app_secret", &self.app_secret)
            .field("sink", &"<dyn CallbackSink>")
            .field("knobs", &self.knobs)
            .finish()
    }
}

impl Connector {
    /// 装配（时间旋钮用上游默认值）。
    #[must_use]
    pub fn new(
        opener: std::sync::Arc<dyn ConnectionOpener>,
        dialer: std::sync::Arc<dyn WsDialer>,
        app_key: impl Into<String>,
        app_secret: AppSecret,
        sink: std::sync::Arc<dyn CallbackSink>,
    ) -> Self {
        Self {
            opener,
            dialer,
            app_key: app_key.into(),
            app_secret,
            sink,
            knobs: StreamKnobs::default(),
        }
    }

    /// 换时间旋钮（用例用）。
    #[must_use]
    pub fn with_knobs(mut self, knobs: StreamKnobs) -> Self {
        self.knobs = knobs;
        self
    }

    /// 本连接的路由键（AppKey）：入站信封要把它盖进 `raw`（回调自己不带 robot code）。
    #[must_use]
    pub fn app_key(&self) -> &str {
        &self.app_key
    }

    /// 引导 + 拨号 + 服务帧，直到停机 / 网关要求重连 / 链路断。
    ///
    /// # Errors
    ///
    /// 引导失败、握手失败、读超时、读失败、pong 写失败 ⇒ [`ChannelError::Transport`]
    /// （supervisor 按"这次尝试失败"退避重连）。
    pub async fn run_session(&self, mut stop: StopHandle) -> ChannelResult<SessionOutcome> {
        let dial_url = self
            .opener
            .open(&self.app_key, self.app_secret.expose())
            .await?;
        let mut connection = self.dialer.dial(&dial_url).await?;

        let mut pings = tokio::time::interval(self.knobs.ping_interval);
        pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval` 的第一拍**立刻**返回；真正的第一个 ping 在 `ping_interval` 之后。
        pings.tick().await;

        loop {
            tokio::select! {
                biased;
                () = stop.wait() => {
                    connection.close().await;
                    return Ok(SessionOutcome::Cancelled);
                }
                _ = pings.tick() => {
                    if self.write_ping_with_timeout(&mut *connection).await.is_err() {
                        connection.close().await;
                        return Err(ChannelError::Transport {
                            message: "dingtalk stream: ping write failed".to_string(),
                        });
                    }
                }
                event = tokio::time::timeout(self.knobs.read_deadline, connection.next_event()) => {
                    match event {
                        Err(_elapsed) => {
                            connection.close().await;
                            return Err(ChannelError::Transport {
                                message: "dingtalk stream: read deadline exceeded".to_string(),
                            });
                        }
                        Ok(None | Some(Ok(WsEvent::Closed))) => {
                            connection.close().await;
                            return Err(ChannelError::Transport {
                                message: "dingtalk stream: socket closed".to_string(),
                            });
                        }
                        Ok(Some(Err(error))) => {
                            connection.close().await;
                            return Err(error);
                        }
                        // 控制帧 / 二进制帧：只为刷新读截止而存在（见模块文档差异 1）。
                        Ok(Some(Ok(WsEvent::Ping | WsEvent::Pong | WsEvent::Binary(_)))) => {}
                        Ok(Some(Ok(WsEvent::Text(text)))) => {
                            let action = self.handle_frame(&text).await;
                            match action {
                                FrameAction::Ignore => {}
                                FrameAction::Disconnect => {
                                    connection.close().await;
                                    return Ok(SessionOutcome::DisconnectRequested);
                                }
                                FrameAction::Pong(response) => {
                                    let Ok(text) = serde_json::to_string(&response) else {
                                        continue;
                                    };
                                    if self
                                        .write_text_with_timeout(&mut *connection, &text)
                                        .await
                                        .is_err()
                                    {
                                        connection.close().await;
                                        return Err(ChannelError::Transport {
                                            message: "dingtalk stream: pong write failed".to_string(),
                                        });
                                    }
                                }
                                FrameAction::Ack(response) => {
                                    let Ok(text) = serde_json::to_string(&response) else {
                                        continue;
                                    };
                                    // ACK 是**尽力而为**：DingTalk 对未 ACK 的帧过期很快，而真正
                                    // 的回复经 Open API 带外投递；engine 的 `(installation, msgId)`
                                    // 去重兜住任何重投（上游逐字）。
                                    if let Err(error) =
                                        self.write_text_with_timeout(&mut *connection, &text).await
                                    {
                                        tracing::warn!(
                                            app_key = self.app_key,
                                            code = error.code(),
                                            "dingtalk stream: ack write failed"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// 分类一帧并（必要时）驱动它的副作用。
    async fn handle_frame(&self, text: &str) -> FrameAction {
        let Ok(frame) = serde_json::from_str::<DataFrame>(text) else {
            // 一帧坏了不该拆掉整条链路（同 slack 的处置）。
            tracing::warn!(
                app_key = self.app_key,
                "dingtalk stream: malformed frame skipped"
            );
            return FrameAction::Ignore;
        };
        match (frame.frame_type.as_str(), frame.topic()) {
            (FRAME_TYPE_SYSTEM, SYSTEM_TOPIC_PING) => {
                FrameAction::Pong(new_pong_response(frame.message_id(), &frame.data))
            }
            (FRAME_TYPE_SYSTEM, SYSTEM_TOPIC_DISCONNECT) => FrameAction::Disconnect,
            (FRAME_TYPE_CALLBACK, BOT_MESSAGE_TOPIC) => {
                self.dispatch_callback(&frame).await;
                FrameAction::Ack(new_ack_response(frame.message_id()))
            }
            _ => {
                tracing::warn!(
                    app_key = self.app_key,
                    frame_type = frame.frame_type,
                    topic = frame.topic(),
                    "dingtalk stream: unhandled frame"
                );
                FrameAction::Ignore
            }
        }
    }

    /// 解码回调 → 交给 sink → **无论如何**都 ACK。
    ///
    /// 解码失败或 sink 报错都只记日志：DingTalk 对未 ACK 的帧过期很快，而 engine 的
    /// `(installation, msgId)` 去重兜住重投（上游逐字）。
    async fn dispatch_callback(&self, frame: &DataFrame) {
        if let Ok(callback) = serde_json::from_str::<BotCallbackData>(&frame.data) {
            if let Err(error) = self.sink.on_callback(callback).await {
                tracing::warn!(
                    app_key = self.app_key,
                    code = error.code(),
                    "dingtalk stream: callback handler failed"
                );
            }
        } else {
            tracing::warn!(
                app_key = self.app_key,
                msg_id = frame.message_id(),
                "dingtalk stream: undecodable callback payload"
            );
        }
    }

    /// 带写上限地写一帧文本。
    async fn write_text_with_timeout(
        &self,
        connection: &mut dyn WsConnection,
        text: &str,
    ) -> ChannelResult<()> {
        match tokio::time::timeout(self.knobs.write_timeout, connection.send_text(text)).await {
            Ok(result) => result,
            Err(_) => Err(ChannelError::Transport {
                message: "dingtalk stream: write timed out".to_string(),
            }),
        }
    }

    /// 带写上限地写一个 ping。
    async fn write_ping_with_timeout(
        &self,
        connection: &mut dyn WsConnection,
    ) -> ChannelResult<()> {
        match tokio::time::timeout(self.knobs.write_timeout, connection.send_ping()).await {
            Ok(result) => result,
            Err(_) => Err(ChannelError::Transport {
                message: "dingtalk stream: write timed out".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests;
