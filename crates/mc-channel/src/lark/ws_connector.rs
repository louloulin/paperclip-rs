//! lark **长连接会话**：引导 → 拨号 → 帧循环（ping/pong、分片、解码、ACK）（上游
//! `internal/integrations/lark/{ws_connector.go 582 行,connector.go 38 行}` 的运行时那一半）。
//!
//! - **写者**：M7-11（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §28）。
//! - 引导与地址参数在 [`super::ws_endpoint`]、二进制帧在 [`super::ws_frame`]、事件解码在
//!   [`super::ws_frame_decoder`]、生产拨号器在 [`tungstenite`]。
//!
//! # 连接的生命周期**不在**这里（写进契约，别在本文件加重连）
//!
//! 重连 / 退避 / 租约归 `engine::Supervisor`（上游注释逐字）。所以
//! [`Connector::run_session`] 只跑**一条** socket 会话，返回 [`SessionOutcome`]：
//!
//! | 收尾 | 返回 | 判据 |
//! | --- | --- | --- |
//! | 停机信号置位 | `Ok(`[`SessionOutcome::Cancelled`]`)` | **不是错误**（对齐 [`crate::channel::Channel`] 的取消语义） |
//! | 对端**正常**关闭（`Close` 帧） | `Ok(`[`SessionOutcome::Closed`]`)` | 上游 `websocket.IsCloseError(CloseNormalClosure, CloseGoingAway)` 那一支 |
//! | 引导失败 / 拨号失败 / 读失败 / 读超时 / **ACK 写失败** / emit 报基础设施错 | `Err` | supervisor 按"这次尝试失败"退避重连 |
//!
//! 上游的 §4.4 不变式（"ctx 取消必须打断阻塞读"）在本仓由 `select!` 表达：读是
//! `tokio::time::timeout(...)` 的一支，停机信号是另一支 ⇒ **取消即返回**，不需要额外的
//! watchdog goroutine（上游要它是因 gorilla 的读不看 ctx）。这条不变式仍然是**承重**的：
//! 它让"丢掉 `connect` 的 future"（supervisor 的取消方式）等价于真的拆链路。
//!
//! # 与上游的三处**形态**差异（登记 `docs/32` §28 的 D 项）
//!
//! 1. **单所有者连接**：上游把写分给 ping goroutine + 读循环两支，用 `sync.Mutex` 串行化；
//!    本仓把 ping 与读放进同一个 `select!`（一个连接只有一个所有者）。代价是"写 ping 的那一瞬
//!    不读"，收益是不必把 socket 拆两半、用例的替身也更简单（同 `dingtalk::stream` 差异 2）。
//! 2. **读截止的重置点**：上游在每次 `ReadMessage` 前 `SetReadDeadline`；本仓用
//!    `timeout(read_deadline, next_event())`，语义等价（"空闲但健康 ⇒ 不超时"）。
//! 3. **凭据由调用方解析**：上游的 `CredentialsProvider` 端口（从安装行解密 `app_secret`）
//!    在本仓归**调用方**（M7-12 的 `feishu_channel.rs` / M7-14 的安装面）—— 那里才拿得到
//!    解密器与安装行。⇒ [`Connector::run_session`] 直接收 [`InstallationCredentials`]，
//!    明文 secret 的生命周期因此**只**覆盖一次会话（不驻留在连接器对象里）。
//!
//! # 凭据面（`docs/60` §2.3 四条判据，逐条落在这里）
//!
//! 1. 本文件**不新增**承载凭据的类型：明文 secret 只在 [`InstallationCredentials`] 里过一手
//!    （那个类型的 `Debug` 已是 `<redacted>`，M7-10 落）；一次性地址在
//!    [`super::ws_endpoint::WsEndpoint`]（手写 `Debug` 脱敏）；
//! 2. `tracing::*` **只**插值 `app_id`（不是秘密，上游日志逐字打印它）、`service_id`、
//!    `ping_interval`、`message_id`、`event_type`、错误**码**与字节数 —— 从不插值 secret、
//!    一次性地址、帧体（帧体是用户正文）；
//! 3. 「错误路径不回显凭据」由三条用例钉住：拨号失败不透出 `WsDialer` 的错误（一次性地址在
//!    错误里，见 [`WsDialer`]）、引导失败不回显响应体（[`super::ws_endpoint`]）、写失败只报
//!    "哪一步失败"（[`tungstenite`]）；
//! 4. 键名进 redaction 表那一侧在 `mc-telemetry`（`docs/60` §2.3 第 4 条），本文件不新增日志字段。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;

use crate::channel::{ChannelError, ChannelResult};

use super::params::InstallationCredentials;
use super::ws_endpoint::{EndpointFetcher, WsEndpoint};
use super::ws_frame::{
    new_ack_frame, new_pong_frame, ChunkAssembler, Frame, FRAME_HEADER_TYPE_PING,
    FRAME_METHOD_CONTROL,
};
use super::ws_frame_decoder::{DecodeOutcome, FrameDecoder, LarkInboundEvent};

pub mod tungstenite;

pub use tungstenite::TungsteniteDialer;

// =====================================================================
// 时间旋钮（上游 `WSConnectorConfig` 的 `withDefaults`）
// =====================================================================

/// 静态心跳间隔（上游 `PingInterval` 的默认值，等于 SDK 的 2 分钟）——
/// ⚠️ 服务端在引导响应里下发的 `PingInterval` **优先**，这里只是它缺席时的兜底。
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_mins(2);
/// 单次读的截止（上游 `ReadDeadline` 默认 6 分钟）：健康连接带着 2 分钟心跳**永不**触发它。
pub const DEFAULT_READ_DEADLINE: Duration = Duration::from_mins(6);
/// 单次写的上限（上游 `WriteTimeout` 默认 10s）。
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// 一次会话的时间旋钮（用例调到毫秒级，免得睡真觉）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionKnobs {
    /// 服务端没下发 `PingInterval` 时用的心跳间隔。
    pub ping_interval: Duration,
    /// 单次读的截止。
    pub read_deadline: Duration,
    /// 单次写的上限。
    pub write_timeout: Duration,
    /// 分片部分状态的寿命（透传给 [`ChunkAssembler`]）。
    pub chunk_ttl: Duration,
}

impl Default for SessionKnobs {
    fn default() -> Self {
        Self {
            ping_interval: DEFAULT_PING_INTERVAL,
            read_deadline: DEFAULT_READ_DEADLINE,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
            chunk_ttl: super::ws_frame::DEFAULT_CHUNK_TTL,
        }
    }
}

// =====================================================================
// 停机信号
// =====================================================================

/// 置位端：宿主（或 adapter 的收口路径）置位后，帧循环**优雅**退出（`connect` 返回 `Ok`）。
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
    /// 从一条既有的 `watch` 接收端造等待端（例如宿主的收口信号）。
    #[must_use]
    pub fn from_receiver(receiver: watch::Receiver<bool>) -> Self {
        Self(receiver)
    }

    /// 是否已置位。
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }

    /// 等置位（已置位 ⇒ 立即返回）。
    pub async fn wait(&mut self) {
        if self.is_stopped() {
            return;
        }
        // `changed()` 只在**新值**到来时返回；`None` = 发送端已丢弃（等价于停机）。
        let _ = self.0.changed().await;
    }
}

// =====================================================================
// WebSocket 端口（上游 `wsConn` / `wsDialer` 两个接口）
// =====================================================================

/// 一次读到的 WS 事件。
///
/// ⚠️ Lark 只发**二进制**帧（帧体是 protobuf 的 [`Frame`]）；文本帧是 lark 侧的 schema 回归
/// ⇒ 上抛给循环记一条 warn 后丢弃（**不**拆链路）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsEvent {
    /// 二进制帧（唯一会被解码的载荷）。
    Binary(Vec<u8>),
    /// 文本帧（本协议不用；上抛以便循环告警）。
    Text(String),
    /// 对端协议层 ping（本协议不用；上抛以便循环刷新读截止）。
    Ping,
    /// 对端协议层 pong（同上）。
    Pong,
    /// 对端关闭（上游判为**干净**收尾的那一支）。
    Closed,
}

/// 一条 WS 会话的最小面（上游 `wsConn` 接口的逐方法对应）。
///
/// 抽成 trait 是为了让用例注入脚本化的内存 socket（上游注释逐字：`tests inject a fake`）。
#[async_trait]
pub trait WsConnection: Send + Sync {
    /// 读下一个事件；`None` = 流结束（无更多帧）。
    async fn next_event(&mut self) -> Option<ChannelResult<WsEvent>>;
    /// 写一帧**二进制**（本协议唯一的出站形态：ping / pong / ACK）。
    async fn send_binary(&mut self, bytes: &[u8]) -> ChannelResult<()>;
    /// 关闭（幂等）。
    async fn close(&mut self);
}

/// 拨号接缝（上游 `WSDialer` 接口）。
///
/// ⚠️ **实现不得把错误原样透出**：`WsEndpoint::url` 自带一次性 `device_id`（等价于凭据），
/// 而 `tokio-tungstenite` 的握手错误里带完整 URL ⇒ 拨号失败一律映射成**固定文案**。
#[async_trait]
pub trait WsDialer: Send + Sync {
    /// 拨一条已引导的端点（地址 + 握手头）。
    ///
    /// # Errors
    ///
    /// 握手失败 / 地址非法 ⇒ [`ChannelError::Transport`]（文案不含地址）。
    async fn dial(&self, endpoint: &WsEndpoint) -> ChannelResult<Box<dyn WsConnection>>;
}

// =====================================================================
// 每安装的连接与事件汇（上游 `EventConnector` / `EventEmitter`）
// =====================================================================

/// 一条**已解码**入站事件的消费者（上游 `EventEmitter`）。
///
/// 上游契约逐字：连接器**只看 error** —— 非 nil = 真的基础设施失败（DB 挂了、dispatcher
/// 配错），连接器应当上报并让 supervisor 退避重连；nil = 消息已被接受并分类（它仍可能按
/// **产品理由**被丢弃：dedup 命中、发件人未绑定、群过滤 —— 那**不是**错误，判决带来的出站
/// 回复由运行时**脱离 ACK 路径**处理）。连接器**不得**绕过 emit 直接写 DB：emit 是唯一的入口。
///
/// 本仓的实现是 M7-12 的 `feishu_channel.rs`（它把 [`LarkInboundEvent`] 归一化成
/// `mc_core::channel::message::InboundMessage` 并交给 engine 的共享 handler）。
#[async_trait]
pub trait EventEmitter: Send + Sync {
    /// 消费一条事件。
    ///
    /// # Errors
    ///
    /// 只用于**基础设施失败**（见 trait 文档）；产品性丢弃返回 `Ok(())`。
    async fn emit(&self, event: LarkInboundEvent) -> ChannelResult<()>;
}

/// 每安装的长连接（上游 `EventConnector`）：打开一条会话、解码事件、逐条调 emit。
///
/// [`EventConnector::run`] **必须阻塞**到停机信号置位（返回 `Ok`）或链路不可本地恢复地断开
/// （返回 `Err`）。实现必须容忍在**不同代会话**上被反复调用 —— supervisor 会 run → 返回 →
/// 退避 → 再 run（上游逐字）。
#[async_trait]
pub trait EventConnector: Send + Sync {
    /// 跑一条会话。
    ///
    /// # Errors
    ///
    /// 见 [`Connector::run_session`]。
    async fn run(
        &self,
        creds: &InstallationCredentials,
        emit: Arc<dyn EventEmitter>,
        stop: StopHandle,
    ) -> ChannelResult<SessionOutcome>;
}

// =====================================================================
// 会话执行体（上游 `WSLongConnConnector`）
// =====================================================================

/// 一条会话的收尾方式（`Err` 才是"这次尝试失败"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    /// 停机信号置位（**不是错误**，`connect` 返回 `Ok`）。
    Cancelled,
    /// 对端**正常**关闭（上游把 `CloseNormalClosure` / `CloseGoingAway` 判为干净收尾）。
    Closed,
}

/// 一个安装的长连接执行体（上游 `WSLongConnConnector`）。
pub struct Connector {
    fetcher: Arc<dyn EndpointFetcher>,
    dialer: Arc<dyn WsDialer>,
    decoder: Arc<dyn FrameDecoder>,
    knobs: SessionKnobs,
}

impl std::fmt::Debug for Connector {
    /// 三个端口都只打印存在性（`Arc<dyn …>` 不可打印），旋钮原样给出。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connector")
            .field("fetcher", &"<dyn EndpointFetcher>")
            .field("dialer", &"<dyn WsDialer>")
            .field("decoder", &"<dyn FrameDecoder>")
            .field("knobs", &self.knobs)
            .finish()
    }
}

impl Connector {
    /// 装配（时间旋钮用上游默认值）。
    #[must_use]
    pub fn new(
        fetcher: Arc<dyn EndpointFetcher>,
        dialer: Arc<dyn WsDialer>,
        decoder: Arc<dyn FrameDecoder>,
    ) -> Self {
        Self {
            fetcher,
            dialer,
            decoder,
            knobs: SessionKnobs::default(),
        }
    }

    /// 换时间旋钮（用例用）。
    #[must_use]
    pub fn with_knobs(mut self, knobs: SessionKnobs) -> Self {
        self.knobs = knobs;
        self
    }

    /// 本实例的旋钮（诊断 / 用例）。
    #[must_use]
    pub fn knobs(&self) -> SessionKnobs {
        self.knobs
    }

    /// 引导 + 拨号 + 服务帧，直到停机 / 对端正常关闭 / 链路断。
    ///
    /// # Errors
    ///
    /// 引导失败、凭据不全、拨号失败、读失败、读超时、**emit 基础设施错**、以及"已经 ACK 了
    /// 一条事件之后 ACK 写失败" ⇒ [`ChannelError::Transport`]（supervisor 按"这次尝试失败"
    /// 退避重连）。
    /// ⚠️ **坏帧 / 解不开的载荷 / 非二进制帧不在此列**：它们只记日志并继续（一个坏载荷不该
    /// 放大成重连风暴，上游逐字）。
    pub async fn run_session(
        &self,
        creds: &InstallationCredentials,
        emit: Arc<dyn EventEmitter>,
        mut stop: StopHandle,
    ) -> ChannelResult<SessionOutcome> {
        // 每次会话引导一次：地址是一次性的（见 `ws_endpoint` 的模块文档），复用会拿到一次
        // "看起来像 lark 宕机"的鉴权拒绝。
        let endpoint = self.fetcher.endpoint(creds).await?;

        // 服务端下发的 PingInterval 优先；零（服务端省略）回落静态默认值 ⇒ 永不退化成
        // "每 0 秒 ping 一次"。
        let ping_interval = if endpoint.ping_interval.is_zero() {
            self.knobs.ping_interval
        } else {
            endpoint.ping_interval
        };

        let mut connection = self.dialer.dial(&endpoint).await?;

        tracing::info!(
            app_id = creds.app_id,
            service_id = endpoint.service_id,
            ping_interval_secs = ping_interval.as_secs(),
            reconnect_interval_secs = endpoint.reconnect_interval.as_secs(),
            reconnect_count = endpoint.reconnect_count,
            "lark ws: connected"
        );

        let service_id = endpoint.service_id;
        // 分片状态**不跨会话**：重连后 lark 从第 0 片重发整条事件 ⇒ 每次会话一个新重组器，
        // 顺带释放上一条会话里被放弃的半成品。
        let assembler = ChunkAssembler::new(self.knobs.chunk_ttl, super::ws_frame::system_clock());

        let mut pings = tokio::time::interval(ping_interval);
        pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval` 的第一拍**立刻**返回；真正的第一个 ping 在 `ping_interval` 之后。
        pings.tick().await;

        let outcome = loop {
            tokio::select! {
                biased;
                () = stop.wait() => {
                    break SessionOutcome::Cancelled;
                }
                _ = pings.tick() => {
                    // ping 写失败与读循环无关：它自己会因为链路死了而报错（上游逐字，
                    // 在这里拆链路会与读循环的收尾打架）。
                    if let Err(error) = self
                        .write_frame(&mut *connection, &super::ws_frame::new_ping_frame(service_id))
                        .await
                    {
                        tracing::warn!(
                            app_id = creds.app_id,
                            code = error.code(),
                            "lark ws: ping write failed"
                        );
                    }
                }
                event = tokio::time::timeout(self.knobs.read_deadline, connection.next_event()) => {
                    match event {
                        Err(_elapsed) => {
                            connection.close().await;
                            return Err(ChannelError::Transport {
                                message: "lark ws: read deadline exceeded".to_string(),
                            });
                        }
                        Ok(None | Some(Ok(WsEvent::Closed))) => {
                            // 干净收尾：关链接交给循环外那一处（`close` 幂等，但不必来两次）。
                            break SessionOutcome::Closed;
                        }
                        Ok(Some(Err(error))) => {
                            connection.close().await;
                            return Err(error);
                        }
                        // 协议层 ping/pong：只为刷新读截止而存在（见 `WsEvent` 的文档）。
                        Ok(Some(Ok(WsEvent::Ping | WsEvent::Pong))) => {}
                        Ok(Some(Ok(WsEvent::Text(text)))) => {
                            tracing::warn!(
                                app_id = creds.app_id,
                                len = text.len(),
                                "lark ws: dropped non-binary frame"
                            );
                        }
                        Ok(Some(Ok(WsEvent::Binary(bytes)))) => {
                            match self
                                .handle_binary(
                                    &mut *connection,
                                    &bytes,
                                    &emit,
                                    &assembler,
                                    &creds.app_id,
                                    service_id,
                                )
                                .await
                            {
                                Ok(()) => {}
                                Err(error) => {
                                    connection.close().await;
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
            }
        };
        connection.close().await;
        tracing::info!(
            app_id = creds.app_id,
            outcome = ?outcome,
            "lark ws: session ended"
        );
        Ok(outcome)
    }

    /// 处置一个二进制帧（一帧一步；每条分支都能被单独断言）。
    async fn handle_binary(
        &self,
        connection: &mut dyn WsConnection,
        bytes: &[u8],
        emit: &Arc<dyn EventEmitter>,
        assembler: &ChunkAssembler,
        app_id: &str,
        service_id: i32,
    ) -> ChannelResult<()> {
        let frame = match Frame::unmarshal(bytes) {
            Ok(frame) => frame,
            Err(error) => {
                // 一帧坏了不该拆掉整条链路（上游逐字）。
                tracing::warn!(
                    app_id = app_id,
                    code = error.to_string(),
                    raw_len = bytes.len(),
                    "lark ws: undecodable frame envelope"
                );
                return Ok(());
            }
        };
        if frame.method == FRAME_METHOD_CONTROL {
            // 控制帧只有心跳要动作：回 pong。⚠️ `Service` 用**引导响应**里的 `service_id`
            // （上游逐字）—— 不回声入站帧的 `Service`：服务端的 ping 允许带 0，照抄会把
            // 出站帧的寻址键打成 0。
            if frame.frame_type() == FRAME_HEADER_TYPE_PING {
                if let Err(error) = self
                    .write_frame(connection, &new_pong_frame(service_id))
                    .await
                {
                    // 上游：pong 写失败只告警（读循环会自己发现链路死了）。
                    tracing::warn!(
                        app_id = app_id,
                        code = error.code(),
                        "lark ws: pong write failed"
                    );
                }
            }
            return Ok(());
        }
        self.handle_data_frame(connection, &frame, emit, assembler, app_id)
            .await
    }

    /// 处置一个 data 帧：分片重组 → 解码 → emit → ACK。
    ///
    /// 三处**承重**的顺序（都对"不重复投递"有直接贡献，见 `docs/32` §28 的 R 项）：
    ///
    /// 1. 分片没齐 ⇒ **不 emit、不 ACK**（服务端才好重投整条事件）；
    /// 2. emit 成功才 ACK 200（未 ACK 的事件会被重投 ⇒ 不丢消息）；
    /// 3. emit 报基础设施错 ⇒ NACK 500 + 结束本次会话（supervisor 退避重连；**租约仍在本副本
    ///    手里**，所以另一副本不可能同时消费同一条安装）。
    async fn handle_data_frame(
        &self,
        connection: &mut dyn WsConnection,
        frame: &Frame,
        emit: &Arc<dyn EventEmitter>,
        assembler: &ChunkAssembler,
        app_id: &str,
    ) -> ChannelResult<()> {
        let (sum, seq, message_id) = super::ws_frame::parse_chunk_headers(frame);
        let payload = if sum > 1 {
            if let Some(assembled) = assembler.admit(&message_id, sum, seq, frame.payload_bytes()) {
                assembled
            } else {
                tracing::debug!(
                    app_id = app_id,
                    message_id = message_id,
                    seq = seq,
                    sum = sum,
                    pending = assembler.pending_count(),
                    "lark ws: partial chunk buffered"
                );
                return Ok(());
            }
        } else {
            frame.payload_bytes().to_vec()
        };

        match self.decoder.decode(&payload) {
            // 一帧坏了不该放大成重连风暴；但**仍然**回 200 —— 帧在 wire 上是合法的，我们只是
            // 认不出它，NACK 会让服务端重投一个我们已经证明解不开的载荷（上游逐字）。
            Err(error) => {
                tracing::warn!(
                    app_id = app_id,
                    code = error.to_string(),
                    payload_len = frame.payload_bytes().len(),
                    "lark ws: undecodable event payload"
                );
                self.write_ack(connection, frame, true, app_id).await
            }
            // 心跳形状 / 未订阅的事件类型：静默丢弃 + ACK 200（"我们处理什么"由解码器定）。
            Ok(DecodeOutcome::Ignored) => self.write_ack(connection, frame, true, app_id).await,
            Ok(DecodeOutcome::Message(event)) => {
                let event_id = event.event_id.clone();
                let message_id = event.message_id.clone();
                if let Err(error) = emit.emit(*event).await {
                    // 基础设施失败：尽力 NACK（让服务端重投这条事件），然后结束会话让
                    // supervisor 退避重连。NACK 写失败只告警（上游逐字）。
                    if let Err(write_error) = self.write_ack(connection, frame, false, app_id).await
                    {
                        tracing::warn!(
                            app_id = app_id,
                            code = write_error.code(),
                            "lark ws: nack write failed"
                        );
                    }
                    tracing::warn!(
                        app_id = app_id,
                        event_id = event_id,
                        code = error.code(),
                        "lark ws: dispatch failed"
                    );
                    return Err(ChannelError::Transport {
                        message: format!("lark ws: dispatch failed ({})", error.code()),
                    });
                }
                tracing::debug!(
                    app_id = app_id,
                    message_id = message_id,
                    "lark ws: event emitted"
                );
                self.write_ack(connection, frame, true, app_id).await
            }
        }
    }

    /// 回一条 ACK / NACK；写失败是**致命**的（调用方据此结束会话）。
    async fn write_ack(
        &self,
        connection: &mut dyn WsConnection,
        frame: &Frame,
        code_ok: bool,
        app_id: &str,
    ) -> ChannelResult<()> {
        self.write_frame(connection, &new_ack_frame(frame, code_ok))
            .await
            .map_err(|error| {
                tracing::warn!(
                    app_id = app_id,
                    code = error.code(),
                    "lark ws: ack write failed"
                );
                ChannelError::Transport {
                    message: "lark ws: ack write failed".to_string(),
                }
            })
    }

    /// 编码一帧并写出去（带写上限）。
    async fn write_frame(
        &self,
        connection: &mut dyn WsConnection,
        frame: &Frame,
    ) -> ChannelResult<()> {
        let payload = frame.marshal();
        match tokio::time::timeout(self.knobs.write_timeout, connection.send_binary(&payload)).await
        {
            Ok(result) => result,
            Err(_) => Err(ChannelError::Transport {
                message: "lark ws: write timed out".to_string(),
            }),
        }
    }
}

#[async_trait]
impl EventConnector for Connector {
    async fn run(
        &self,
        creds: &InstallationCredentials,
        emit: Arc<dyn EventEmitter>,
        stop: StopHandle,
    ) -> ChannelResult<SessionOutcome> {
        self.run_session(creds, emit, stop).await
    }
}

#[cfg(test)]
mod tests;
