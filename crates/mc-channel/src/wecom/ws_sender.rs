//! **一条 WebSocket 的串行化写侧** + **回帧路由**（上游
//! `internal/integrations/wecom/ws_sender.go`，**1,128 行**）。
//!
//! - **写者**：M7-16（`LUM-1781` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：`gorilla` **禁止并发写**，所以每一帧出站都走同一个
//!   互斥量 —— ping 循环、`aibot_subscribe` 握手、`Send()` 调用共用这一个写者。
//!
//! # 本文件承担 §6.3 拆分里的「帧路由」那一半
//!
//! `docs/60-M7-PLAN.md` §6.3 要求把上游 `ws_frame.go` 按「帧编解码 / 帧路由」拆两文件
//! （见 [`super::ws_frame`] 的模块文档）。「帧路由」在这里落地：上游 `routeResponse` /
//! `deliverReply` / `deliverAck` 就是**把服务端的回帧按 `req_id` 交给等待者**，
//! 它们与写侧的 ack 账本本来就在同一个结构上（`wsSender`）—— 拆到两个文件会把
//! 「谁在等这一帧」这**一个**事实切成两半。
//!
//! # 三个结构，各管一个方向
//!
//! | 结构 | 管什么 | 上游出处 |
//! | --- | --- | --- |
//! | **写者槽**（`Semaphore(1)`） | 帧到 socket 的**顺序**：ping / 握手 / 推送 / 流帧都在这里排队 | `wmu chan struct{}` |
//! | **每个聊一把锁**（[`ChatLocks`]） | **一条逻辑消息**的顺序：一帧的写是一回事，一条回答的几段之间不许插进别人的消息 | `chats chatLocks` |
//! | **ack 账本**（[`AckBook`]） | **配对**：`req_id` → 等待者，以及"这个 `req_id` 上服务端还欠不欠一次判决" | `replies` / `waiters` / `streams` |
//!
//! 前两个管顺序、第三个管配对，**三者互不替代**：上游的注释里，"`mu` 排一帧的写、它在
//! 等 ack 之前就放开了，那正是别人的一条消息插进一条回答两段之间的地方"。
//!
//! # `context.Context` → 截止时刻（`Option<Instant>`）
//!
//! 上游用 Go 的 `ctx` 表达三件事：写者槽的等待、ack 的等待、整条 `sendText` 的预算。
//! 本仓把调用方的预算表示成 **[`Deadline`]（`Option<Instant>`）**：`None` = 上游的
//! `context.Background()`（"该等多久就等多久"），`Some(t)` = 到点即放弃。
//! 之所以不用 `CancellationToken`：workspace 没有 `tokio-util`（`docs/60` §3.1 的
//! 依赖边一次接好、后续片不得新增三方依赖），而上游真正需要的两个事实
//! （"写之前就作废了"与"发出去了但没等到判决"）都能由截止时刻表达。
//! 登记为 `docs/32` §33 的 D5。
//!
//! # 不属于本片的部分
//!
//! - **配额与重试门**（上游 `rate_limit.go:182` 的 `sendMsgFrame`、`sendQuota`、
//!   `sendRetryBackoff`）：本地落点是 M7-20 的 `rate_limit.rs` ⇒ 本片
//!   [`WsSender::send_text`] 的每一段直接走 `request()`，**没有** 429 退避与一次重试。
//!   见 `docs/32` §33 的 D4。
//! - **追踪**（`trace.go` 的 `traceOutFields` / `traceOutAttempt` / `traceOutResult`，
//!   M7-20 的 `trace.rs`）：本片不做逐帧埋点，只在写失败时记一条**不含正文**的日志。
//! - **`errNoLiveConnection`** 与 `sendersRegistry`（M7-20）：`route_response` 的调用方
//!   是 M7-19 的读循环，它拿的是一条**具体**的连接，不需要注册表。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout_at;

use super::ws_frame::{
    decode_frame, encode_frame, frame_with, new_req_id, Frame, FrameEnvelope, FrameError, CMD_PING,
    CMD_RESPOND_MSG, CMD_SEND_MSG, CMD_SUBSCRIBE, MAX_FRAME_BYTES, SEND_MSG_CONTENT_LIMIT,
    WRITE_DEADLINE,
};
/// 上游 `ackTimeout`：等判决的上限。`WeCom` 通常几百毫秒就回；过了这个数就认为 ack
/// **丢了**而不是帧被拒了 —— 这个区别重要，因为两者的正确反应相反。
pub const ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// 上游 `ackOwedPoll`：`await_ack` 复查"被放弃的那一帧的判决到了没有"的间隔。
pub const ACK_OWED_POLL: Duration = Duration::from_millis(10);

/// 上游 `streamAcksMax`：一条长连接上每轮的记账上限。**触到它**是唯一次会退休一个条目的
/// 事情（没有任何定时器在跑），所以它设在远超一个 bot 在一个流窗口内可能有的轮数之上。
pub const STREAM_ACKS_MAX: usize = 2048;

/// 调用方的预算（上游的 `context.Context`）。
///
/// `None` = `context.Background()`：**该等多久就等多久**（ping、握手、主动推送都走这个）。
pub type Deadline = Option<Instant>;

/// 一个截止时刻表示成 `sleep_until` 要的时长；`None` 给一个**远到不会先到**的时刻。
fn deadline_or_far(deadline: Deadline) -> tokio::time::Instant {
    deadline
        .unwrap_or_else(|| Instant::now() + Duration::from_hours(24))
        .into()
}

// =====================================================================
// 子模块
// =====================================================================

mod ack;
mod chat_locks;
mod error;

pub use ack::*;
pub use chat_locks::*;
pub use error::*;

// =====================================================================
// socket 的接缝
// =====================================================================

/// 一次写失败发生在**哪一步**（上游 `writeLocked` 的两段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkFailure {
    /// 在写之前就失败了（设写超时被拒）—— **可证明没发出去**。
    BeforeWrite,
    /// 已经进了写调用 —— 对端**可能**已经拿到字节。
    WriteAttempted,
}

/// socket 层的失败（上游 `wsConn` 方法的错误）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("wecom: {message}")]
pub struct SinkError {
    pub failure: SinkFailure,
    pub message: String,
}

impl SinkError {
    /// 在写之前失败（可证明没发出去）。
    #[must_use]
    pub fn before_write(message: impl Into<String>) -> Self {
        Self {
            failure: SinkFailure::BeforeWrite,
            message: message.into(),
        }
    }

    /// 写了之后失败（**不**是"没投递"的证明）。
    #[must_use]
    pub fn write_attempted(message: impl Into<String>) -> Self {
        Self {
            failure: SinkFailure::WriteAttempted,
            message: message.into(),
        }
    }
}

/// `WeCom` 用到的 WebSocket 连接面（上游 `wsConn` 接口）。
///
/// 上游把 `gorilla` 的 `Conn` 收窄成一个五方法接口，**理由是测试能注入一个假实现而不必
/// 把整个 `gorilla` 的表面嵌进来**；本仓照抄这条理由：M7-19 的读循环持有真实现
/// （`tokio-tungstenite` 的 `SplitSink`），本片的用例持有 [`tests`] 里的假实现。
///
/// `&mut self`（而不是 `&self`）：串行化是**调用方**的责任（本文件的写者槽），所以
/// 实现方不必自己再套一层锁。
#[async_trait]
pub trait WsSink: Send + Sync {
    /// 推一帧文本帧。`deadline` 是调用方给的写预算；实现方**必须**把它真正落到底层写超时上。
    ///
    /// # Errors
    ///
    /// [`SinkError`]，并如实标注 [`SinkFailure`]。
    async fn write_text(&mut self, payload: &[u8], deadline: Instant) -> Result<(), SinkError>;

    /// 关掉 socket（上游 `wsConn.Close`）。
    ///
    /// # Errors
    ///
    /// [`SinkError`]。
    async fn close(&mut self) -> Result<(), SinkError>;
}

// =====================================================================
// 发送者
// =====================================================================

/// 编码一帧，并把"超限"翻成一个**独立**的错误变体。
///
/// 上游没有帧级上限（见 [`super::ws_frame`] 的模块文档），所以这条映射是本仓的：两个上限
/// 失败（内容超 [`SEND_MSG_CONTENT_LIMIT`]、帧超 [`MAX_FRAME_BYTES`]）都归
/// [`SenderError::FrameTooLarge`]，调用方只需要认一个。
fn encode_or_refuse(frame: &Value) -> Result<Vec<u8>, SenderError> {
    encode_frame(frame).map_err(|error| match error {
        FrameError::TooLarge { len, limit } => SenderError::FrameTooLarge { len, limit },
        other => SenderError::Frame(other),
    })
}

/// 一条 WebSocket 的写侧（上游 `wsSender`）。
pub struct WsSender {
    /// socket。外面套一层 `tokio` 的异步互斥量只是为了**能拿到 `&mut`**（`WsSink` 的方法
    /// 要 `&mut self`）：串行化由上面的写者槽负责，所以这一层**永不争用**。
    /// 不用 `std::sync::Mutex` 是因为它会跨越 `await`（`clippy::await_holding_lock`）。
    sink: AsyncMutex<Box<dyn WsSink>>,
    /// 写者槽 —— `gorilla` 禁止并发写。容量 1 的信号量而不是互斥量，因为**有截止时刻的
    /// 调用方必须能停止排队**：一次收尾帧跑在总线订阅者的预算上，排在一条 20KB 推送后面
    /// 会把预算全花在排队上，帧还没到 socket。
    writer: Arc<Semaphore>,
    book: Arc<AckBook>,
    chats: Arc<ChatLocks>,
    /// 一帧等判决的上限。做成字段而不是常量，才能让用例在不干等五秒的前提下走完"放弃"路径。
    ack_timeout: Duration,
    /// [`AckBook::await_ack`] 复查的间隔。同上，做成字段。
    ack_poll: Duration,
    /// 出站帧到达 socket 的顺序号。由写者槽保护 —— ping 循环、agent 回复、入站推送与流帧
    /// 就是在这一点上变成**有序**的，所以它**由构造**就是 wire 顺序。它**从不**上 wire。
    seq: Mutex<u64>,
}

impl WsSender {
    /// 建一个写侧（上游 `newWSSender`）。
    #[must_use]
    pub fn new(sink: Box<dyn WsSink>) -> Self {
        Self {
            sink: AsyncMutex::new(sink),
            writer: Arc::new(Semaphore::new(1)),
            book: Arc::new(AckBook::new()),
            chats: Arc::new(ChatLocks::new()),
            ack_timeout: ACK_TIMEOUT,
            ack_poll: ACK_OWED_POLL,
            seq: Mutex::new(0),
        }
    }

    /// 把判决等待的上限换掉（只给用例用；生产用 [`ACK_TIMEOUT`]）。
    #[must_use]
    pub fn with_ack_timeout(mut self, ack_timeout: Duration) -> Self {
        self.ack_timeout = ack_timeout;
        self
    }

    /// 把"欠着判决"复查的间隔换掉（只给用例用）。
    #[must_use]
    pub fn with_ack_poll(mut self, ack_poll: Duration) -> Self {
        self.ack_poll = ack_poll;
        self
    }

    /// ack 账本（读循环 `route_response` 的入口）。
    #[must_use]
    pub fn book(&self) -> &Arc<AckBook> {
        &self.book
    }

    /// 每个聊一把锁的那张表（诊断与用例用：它是**每条逻辑消息的顺序**那条承诺的载体）。
    #[must_use]
    pub fn chat_locks(&self) -> &Arc<ChatLocks> {
        &self.chats
    }

    /// 本连接已写出的帧数（`seq`）。
    #[must_use]
    pub fn written_frames(&self) -> u64 {
        *match self.seq.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 把服务端的回帧交给在等它的人（上游 `routeResponse` 的转发，读循环调用）。
    #[must_use]
    pub fn route_response(&self, envelope: &FrameEnvelope) -> bool {
        self.book.route_response(envelope)
    }

    /// 解一段 wire 字节并按需路由（读循环的**最小**接缝：M7-19 用它把 ack 喂进来）。
    ///
    /// # Errors
    ///
    /// [`FrameError`]（帧超限或形态不对）。
    pub fn route_raw(&self, raw: &[u8]) -> Result<Frame, FrameError> {
        let frame = decode_frame(raw)?;
        if let Frame::Response(envelope) = &frame {
            let _ = self.route_response(envelope);
        }
        Ok(frame)
    }

    /// 写一帧、不等任何人（上游 `write`）：ping、握手、主动推送都走这里，
    /// 它们**该等多久就等多久**。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn write(&self, frame: &Value) -> Result<(), SenderError> {
        let payload = encode_or_refuse(frame)?;
        self.write_payload(payload, None).await
    }

    /// 写一帧 `{cmd, headers, body}`（上游 `request` / `write` 的组合）。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn write_cmd(&self, req_id: &str, cmd: &str, body: Value) -> Result<(), SenderError> {
        self.write(&frame_with(req_id, cmd, body)).await
    }

    /// 写一帧并用**我们自己铸的** `req_id` 等**整份**应答（上游 `request`）。
    ///
    /// 非 nil 错误要么是 [`SenderError::Api`]（带服务端的 `errcode`）、
    /// [`SenderError::AckTimeout`]，要么是一次传输失败。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn request(
        &self,
        deadline: Deadline,
        cmd: &str,
        body: Value,
    ) -> Result<Value, SenderError> {
        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            // 标记成 NotAttempted：此刻什么都没铸、没登记、没构造，
            // 所以这是"对端什么也没看到"的**证明**。
            return Err(SenderError::NotAttempted);
        }
        let req_id = new_req_id();
        let Some(receiver) = self.book.await_reply(&req_id) else {
            return Err(SenderError::ReqIdTaken {
                cmd: cmd.to_owned(),
                req_id,
            });
        };
        let result = self
            .write_and_wait(&req_id, cmd, body, receiver, deadline)
            .await;
        self.book.cancel_reply(&req_id);
        result
    }

    async fn write_and_wait(
        &self,
        req_id: &str,
        cmd: &str,
        body: Value,
        receiver: oneshot::Receiver<ReplyResult>,
        deadline: Deadline,
    ) -> Result<Value, SenderError> {
        self.write_cmd(req_id, cmd, body).await?;
        // **哪一个先到就算哪一个。** 上游的两个哨兵说的就是这样一件事：
        // `errAckTimeout` = "ack 超时了"（消息很可能已经投递），
        // `errAckAbandoned` = "调用方的预算先到了"。把前者误报成后者会让调用方
        // 按"可能是我们的错"去重试，而二者的正确反应是相反的。
        let ack_timeout = self.ack_timeout;
        let (limit, caller_bound) = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining < ack_timeout {
                    (remaining, true)
                } else {
                    (ack_timeout, false)
                }
            }
            None => (ack_timeout, false),
        };
        match timeout_at((Instant::now() + limit).into(), receiver).await {
            Ok(Ok(reply)) => {
                if reply.code != 0 {
                    return Err(SenderError::Api {
                        cmd: cmd.to_owned(),
                        code: reply.code,
                        message: reply.message,
                    });
                }
                Ok(reply.body)
            }
            Ok(Err(_)) => Err(SenderError::AckAbandoned {
                cause: format!("{cmd} was cancelled while waiting for its verdict"),
            }),
            Err(_) if caller_bound => Err(SenderError::AckAbandoned {
                cause: format!("{cmd} waited past the caller's budget"),
            }),
            Err(_) => Err(SenderError::AckTimeout),
        }
    }

    /// 等判决并把非零 `errcode` 翻成 [`SenderError::Stream`]。
    async fn await_stream_verdict(
        &self,
        waiter: AckWaiterHandle,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        match waiter.wait(self.ack_timeout, deadline).await? {
            Ok(()) => Ok(()),
            Err(error) => Err(SenderError::Stream(error)),
        }
    }

    /// 一次 `aibot_subscribe` 握手（上游 `wecom_channel.go` 的握手，body 由
    /// [`super::ws_frame::subscribe_body`] 构造）。
    ///
    /// `body` 的类型是 [`super::ws_frame::SubscribeBody`]（**手写 `Debug`**），
    /// 本函数是它唯一的消费点 —— 明文只在这里变成 wire 字节。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn subscribe(
        &self,
        deadline: Deadline,
        body: super::ws_frame::SubscribeBody,
    ) -> Result<Value, SenderError> {
        self.request(deadline, CMD_SUBSCRIBE, body.into_value())
            .await
    }

    /// 一次心跳（上游 ping 循环）。回帧 `pong` 由读循环消费，这里只写。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn ping(&self) -> Result<(), SenderError> {
        self.write_cmd(&new_req_id(), CMD_PING, Value::Null).await
    }

    /// 写一帧流式回复并等判决（上游 `respondStream`）。
    ///
    /// `req_id` **不是**我们选的：同一条流的每一帧都必须回显打开这一轮的
    /// `aibot_msg_callback` 的 `req_id`，否则服务端拒（`846605`）。
    /// `stream_id` 是我们的：复用 = 替换气泡正文，`finish` = 封口。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn respond_stream(
        &self,
        req_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        self.respond_stream_frame(req_id, stream_id, content, finish, false, deadline)
            .await
    }

    /// 写一帧这个发送者**已经写过一次**的帧：`seal` 对一次判决没回来的收尾帧的重试
    /// （上游 `respondStreamRewrite`）。
    ///
    /// 它是唯一被"欠着判决"那道门放过去的写，而这是安全的：**同一帧不是第二帧**。
    /// 对一个已封口的流重写，上游 2026-09-03 对活租户实测 —— 六帧写进一个已封口的流，
    /// 同内容与不同内容，全是 `errcode 0`。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn respond_stream_rewrite(
        &self,
        req_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        self.respond_stream_frame(req_id, stream_id, content, finish, true, deadline)
            .await
    }

    async fn respond_stream_frame(
        &self,
        req_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
        rewrite: bool,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        if req_id.is_empty() {
            return Err(SenderError::MissingCallbackReqId);
        }
        let body = super::ws_frame::respond_stream_body(stream_id, content, finish)?;

        let waiter = self
            .book
            .await_ack(
                req_id,
                finish,
                rewrite,
                self.ack_timeout,
                self.ack_poll,
                deadline,
            )
            .await?;

        let waiter_id = waiter.id;
        if let Err(error) = self
            .write_stream_frame(req_id, stream_id, &waiter, finish, body, deadline)
            .await
        {
            self.book.cancel_ack(req_id, waiter_id);
            return Err(error);
        }
        self.await_stream_verdict(waiter, deadline).await
    }

    /// `write()` 的流帧版本：同一个串行化的推送，但**这一轮的记账在写者自己的临界区里**
    /// 做，所以一轮的两帧不可能交错（上游 `writeStreamFrame`）。
    ///
    /// 与 `write()` 不同，这一条**尊重截止时刻** —— 在写可能卡住的两个地方：
    /// 等写者、等 socket。
    async fn write_stream_frame(
        &self,
        req_id: &str,
        stream_id: &str,
        waiter: &AckWaiterHandle,
        finish: bool,
        body: Value,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        let payload = encode_or_refuse(&frame_with(req_id, CMD_RESPOND_MSG, body))?;
        let _permit = self.lock_writer(deadline).await?;
        if !self
            .book
            .begin_stream_frame(req_id, stream_id, Some(waiter), finish)
        {
            return Err(SenderError::StreamSuperseded);
        }
        match self.write_payload_locked(payload, deadline).await {
            Ok(()) => Ok(()),
            Err(error) => {
                // 只有**可证明从未到达 socket** 的帧才把位置还回去。`WriteAttempted`
                // 意味着写调用已经进去了，对端可能已经拿到字节、也可能还会应答它；
                // 把位置还回去会让那个判决去结清**下一帧** —— 正是序号存在的理由。
                if !matches!(error, SenderError::WriteAttempted { .. }) {
                    self.book.abort_stream_frame(req_id);
                }
                Err(error)
            }
        }
    }

    /// 往一个具体聊推一条 `aibot_send_msg` 纯文本（上游 `sendText`）。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn send_text(
        &self,
        chat_id: &str,
        chat_type_int: i32,
        content: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        let pieces = super::ws_frame::split_for_wire(content);
        // **每一次发送都持锁**，不只是被切开的那种：另一个调用方的单帧推送 —— 一张卡片、
        // 同一条回答产出的文件、不支持类型的提示 —— 正是过去会插进第一段与第二段之间的
        // 东西；而同时有两条长回答在飞时，`(n/total)` 计数也对不回自己的正文。
        let guard = self.chats.acquire(chat_id, deadline).await?;
        for (index, piece) in pieces.iter().enumerate() {
            if let Err(error) = self
                .send_one_text(chat_id, chat_type_int, piece, deadline)
                .await
            {
                if index > 0 {
                    // 上游 `errPartiallySent`：前一段**已经被服务端接受**，所以"这次发送
                    // 什么都没写出去"这个问题的答案与单帧失败时不同。
                    return Err(SenderError::PartiallySent {
                        cause: error.to_string(),
                    });
                }
                return Err(error);
            }
        }
        drop(guard);
        Ok(())
    }

    /// 写**恰好一帧** `aibot_send_msg` 并读它的 ack（上游 `sendOneTextCtx`）。
    ///
    /// 这里什么都不许超过上限：[`SEND_MSG_CONTENT_LIMIT`] 之后唯一站着的东西就是
    /// [`super::ws_frame::split_for_wire`]。上游把配额度量与一次重试插在这一层
    /// （`rate_limit.go:182` 的 `sendMsgFrame`）—— 那属于 M7-20（`docs/32` §33 的 D4）。
    async fn send_one_text(
        &self,
        chat_id: &str,
        chat_type_int: i32,
        content: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        if content.len() > SEND_MSG_CONTENT_LIMIT {
            return Err(SenderError::FrameTooLarge {
                len: content.len(),
                limit: SEND_MSG_CONTENT_LIMIT,
            });
        }
        let body = super::ws_frame::send_msg_text_body(chat_id, chat_type_int, content)?;
        self.request(deadline, CMD_SEND_MSG, body).await.map(|_| ())
    }

    /// 写一帧、等它的回合，**该等多久就等多久**（上游 `write` 的 `context.Background()`）。
    async fn write_payload(&self, payload: Vec<u8>, deadline: Deadline) -> Result<(), SenderError> {
        let _permit = self.lock_writer(deadline).await?;
        self.write_payload_locked(payload, deadline).await
    }
    /// 推一帧**已经编码好**的帧。调用方持有写者槽（上游 `writeLocked`）。
    async fn write_payload_locked(
        &self,
        payload: Vec<u8>,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        if payload.len() > MAX_FRAME_BYTES {
            return Err(SenderError::FrameTooLarge {
                len: payload.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        {
            let mut seq = match self.seq.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *seq += 1;
        }
        // socket 的超时是**连接的写预算**与调用方自己给的预算里更早的那个
        // （上游 `writeLocked`）：一帧只有几 KB，所以一个在调用方预算内吃不下它的 socket
        // 是拥塞而不是忙，而 `Supervisor` 的重连正是为此设计的答案。
        let now = Instant::now();
        let mut write_deadline = now + WRITE_DEADLINE;
        if let Some(deadline) = deadline {
            write_deadline = write_deadline.min(deadline);
        }
        // 写下这一段要 `&mut`：sink 归本结构独占，而串行化已经由写者槽保证。
        self.sink
            .lock()
            .await
            .write_text(&payload, write_deadline)
            .await
            .map_err(|error| match error.failure {
                // 上游 `writeLocked`：**只有**过了 `WriteMessage` 那一点才包
                // `errWriteAttempted`。这个区别是调用方的：在此之前失败是**可证明的**本地
                // 失败，调用方可以报"确定没投递"；在此之后不行。
                SinkFailure::WriteAttempted => SenderError::WriteAttempted {
                    cause: error.message,
                },
                SinkFailure::BeforeWrite => SenderError::Sink(error),
            })
    }

    /// 拿写者槽，或 `deadline` 到点（上游 `lockWriter`）。**没有**截止时刻的调用方
    /// —— ping、握手、主动推送 —— 传 `None`，该等多久就等多久。
    async fn lock_writer(&self, deadline: Deadline) -> Result<OwnedSemaphorePermit, SenderError> {
        let semaphore = Arc::clone(&self.writer);
        if let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() {
            return Ok(permit);
        }
        match timeout_at(deadline_or_far(deadline), semaphore.acquire_owned()).await {
            Ok(Ok(permit)) => Ok(permit),
            _ => Err(SenderError::NotAttempted),
        }
    }

    /// 关掉 socket（上游 `wsConn.Close`）。
    ///
    /// # Errors
    ///
    /// [`SenderError`]。
    pub async fn close(&self) -> Result<(), SenderError> {
        self.sink
            .lock()
            .await
            .close()
            .await
            .map_err(SenderError::from)
    }
}

#[cfg(test)]
mod tests;
