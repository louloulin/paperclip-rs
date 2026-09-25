//! ack 账本：`req_id` → 等待者，以及"这个 `req_id` 上服务端还欠不欠一次判决"。
//!
//! 本文件是 `ws_sender.rs` 的子模块：拆分的理由是 `docs/60-M7-PLAN.md` §6.3 要求把
//! 1,187 行的上游 `ws_frame.go` 按「帧编解码 / 帧路由」拆开，加上门 ⑩ 的 800 行硬限。
//! 逐条清单见 `docs/32` §33 的 D10。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};
use tokio::time::timeout_at;

use crate::wecom::stream_store::{STREAM_CLOSE_TIMEOUT, STREAM_MAX_AGE};
use serde_json::Value;

use crate::wecom::ws_frame::{FrameEnvelope, StreamError};

use super::{deadline_or_far, Deadline, SenderError, STREAM_ACKS_MAX};

// =====================================================================
// ack 账本
// =====================================================================

/// 一次服务端判决（上游 `ackResult`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckResult {
    pub code: i32,
    pub message: String,
}

/// 一次请求的完整应答（上游 `replyResult`）。`body` 对只带判决的 ack 是 `Value::Null`。
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyResult {
    pub code: i32,
    pub message: String,
    pub body: Value,
}

/// 一帧流帧的**standing** 请求（上游 `ackWaiter`）。
///
/// `seq` 是这一帧在它 `req_id` 的写序里的位置，在它上 wire 的那一刻打上去 ——
/// 见 [`StreamAcks`] 为什么判决必须被**配对**而不是直接交给"正在等的人"。
#[derive(Debug)]
struct AckWaiter {
    /// 这个等待者的身份。上游靠**指针**认等待者（`cur == w`），Rust 里拿不到一个稳定的
    /// 地址可以比 ⇒ 用一个单调递增的 id 做同一件事。
    id: u64,
    verdict: oneshot::Sender<AckResult>,
    /// 这个 `req_id` 上**先于**这一帧写出的每一帧，在它出去时**都已经有判决了** ——
    /// 这才让"到达的判决按位置"能识别它属于哪一帧。
    seq: u64,
    addressable: bool,
    /// 唯一被门放过去的、欠着判决的那种帧：**同一个收尾帧**重写一次。
    rewrite: bool,
    /// 一旦这个等待者离开表（判决或取消）就"关掉"。被它挡住的收尾帧在这上面等。
    done: watch::Sender<bool>,
}

impl AckWaiter {
    fn resolve(&self) {
        self.done.send_replace(true);
    }
}

/// 一个 `req_id` 的流帧进、判决出，并记住收尾帧什么时候走的（上游 `streamAcks`）。
///
/// 两半都存在，因为一个气泡在**一轮里会被写不止一次**。ack 帧除了 `req_id` 什么都不带 ——
/// 没有 stream id、没有序号 —— 所以一个判决只能按**位置**识别。
///
/// 上游 2026-09-02 对活 bot 实测：一个 `req_id` 有多帧在飞时，ack **不**按写序回来
/// （背靠背写的 24 帧按结果分组回）。所以位置配对只在**一个 `req_id` 上至多一帧在 wire 上**
/// 时才成立，而这就是 `await_ack` 现在对收尾帧也执行的规则。
#[derive(Debug)]
struct StreamAcks {
    sent: u64,
    acked: u64,
    sealed: HashSet<String>,
    at: Instant,
}

impl StreamAcks {
    fn new() -> Self {
        Self {
            sent: 0,
            acked: 0,
            sealed: HashSet::new(),
            at: Instant::now(),
        }
    }

    fn is_sealed(&self, stream_id: &str) -> bool {
        self.sealed.contains(stream_id)
    }
}

/// ack 账本（上游 `wsSender` 的 `ackMu` 保护区）。
///
/// 用 `std::sync::Mutex` 而不是 `tokio` 的：临界区里**没有** `await`（上游也是
/// `sync.Mutex`），而 tokio 的锁会把"持有跨 await"变成一个必须靠纪律避免的运行时错误。
#[derive(Debug)]
pub struct AckBook {
    inner: Mutex<AckBookInner>,
}

#[derive(Debug)]
struct AckBookInner {
    /// 我们自己铸的 `req_id` → 在等**整份应答**的调用方。
    replies: HashMap<String, oneshot::Sender<ReplyResult>>,
    /// 服务端自己的 `req_id`（回调的）→ 在等**判决**的流帧。
    waiters: HashMap<String, AckWaiter>,
    streams: HashMap<String, StreamAcks>,
    /// 等待者 id 的发放器（见 [`AckWaiter::id`]）。
    next_waiter: u64,
}

impl Default for AckBook {
    fn default() -> Self {
        Self::new()
    }
}

impl AckBook {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(AckBookInner {
                replies: HashMap::new(),
                waiters: HashMap::new(),
                streams: HashMap::new(),
                next_waiter: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AckBookInner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 把服务端的回帧交给在等它的人，并报告**有没有人接**（上游 `routeResponse`）。
    ///
    /// 读循环对**每一帧**回答我们写出的帧调用它；没人认领的 ack **不是**错误 ——
    /// 不等判决的那些推送共用这条连接。
    ///
    /// 顺序重要：先问在等 body 的请求，因为那些 `req_id` 是**我们的**，而流的 `req_id` 是
    /// **服务端的** —— 一次查表就把"这是哪种应答"定下来，不必让帧自己说。
    #[must_use]
    pub fn route_response(&self, envelope: &FrameEnvelope) -> bool {
        if self.deliver_reply(envelope) {
            return true;
        }
        self.deliver_ack(
            &envelope.headers.req_id,
            envelope.errcode,
            &envelope.error_message,
        );
        false
    }

    /// 把一次应答交给问了它的请求（上游 `deliverReply`）。
    #[must_use]
    pub fn deliver_reply(&self, envelope: &FrameEnvelope) -> bool {
        if envelope.headers.req_id.is_empty() {
            return false;
        }
        let waiter = {
            let mut book = self.lock();
            book.replies.remove(&envelope.headers.req_id)
        };
        let Some(waiter) = waiter else {
            return false;
        };
        // 一次性通道，且条目已在上面摘掉 ⇒ 这里既不阻塞也不会送两次。
        let _ = waiter.send(ReplyResult {
            code: envelope.errcode,
            message: envelope.error_message.clone(),
            body: envelope.body.clone(),
        });
        true
    }

    /// 把一次服务端判决交给它所属的流帧（上游 `deliverAck`）。
    ///
    /// "它所属的那一帧"才是重点，而且它**不等于**"正在等的那个人"：一个 `req_id` 承载一轮
    /// 的所有帧，它的 ack 不说是应答哪一帧的，所以**计数**说了算 —— 一个 `req_id` 上的第
    /// N 个判决属于它的第 N 帧。一个调用方已经放弃的帧的判决在这里被**丢掉**，
    /// 而不是交给下一帧。
    pub fn deliver_ack(&self, req_id: &str, code: i32, message: &str) {
        if req_id.is_empty() {
            return;
        }
        let waiter = {
            let mut book = self.lock();
            let Some(streams) = book.streams.get_mut(req_id) else {
                return; // 不是流帧的 ack
            };
            streams.acked += 1;
            let acked = streams.acked;
            let taken = book
                .waiters
                .get(req_id)
                .is_some_and(|waiter| waiter.addressable && waiter.seq == acked);
            if taken {
                book.waiters.remove(req_id)
            } else {
                None
            }
        };
        let Some(waiter) = waiter else {
            return;
        };
        // 先取走两个发送端再各自消费：`verdict` 被 send 部分移走后就不能再碰整个结构了。
        let AckWaiter { verdict, done, .. } = waiter;
        let _ = verdict.send(AckResult {
            code,
            message: message.to_owned(),
        });
        done.send_replace(true);
    }

    /// 登记对一个请求的整份应答的兴趣（上游 `awaitReply`）。
    ///
    /// `None` = 这个 `req_id` 已经被占了：自己铸的 id 上出现这种情况是**我们宁愿失败也
    /// 不想悄悄接错线**的碰撞。
    #[must_use]
    pub fn await_reply(&self, req_id: &str) -> Option<oneshot::Receiver<ReplyResult>> {
        let mut book = self.lock();
        if book.replies.contains_key(req_id) {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        book.replies.insert(req_id.to_owned(), sender);
        Some(receiver)
    }

    /// 退休一个等待者。**每一条退出路径**都要调用，包括顺利的那条 —— 一次请求是一帧一个
    /// 答案，条目永远不会有第二次用途，留着就是每次发送泄一个条目（上游 `cancelReply`）。
    pub fn cancel_reply(&self, req_id: &str) {
        let mut book = self.lock();
        book.replies.remove(req_id);
    }

    /// 这个聊/`req_id` 上服务端还欠着判决吗（上游 `awaitAck` 的"欠"条件）。
    fn owes_verdict(book: &AckBookInner, req_id: &str) -> bool {
        book.streams
            .get(req_id)
            .is_some_and(|streams| streams.acked < streams.sent)
    }

    /// 登记对**即将写出的那一帧**流帧判决的兴趣，并且是"一个 `req_id` 至多一帧在飞"这条
    /// 规则的执行处（上游 `awaitAck`）。
    ///
    /// 非收尾帧碰上在飞的一帧就让位（[`SenderError::StreamBusy`]）。收尾帧等它离开表 ——
    /// 无论是因为判决还是调用方自己的超时，所以等待由 `ackTimeout` 兜住 —— 才轮到它。
    ///
    /// # Errors
    ///
    /// [`SenderError::StreamBusy`]。
    pub async fn await_ack(
        &self,
        req_id: &str,
        finish: bool,
        rewrite: bool,
        ack_timeout: Duration,
        poll: Duration,
        deadline: Deadline,
    ) -> Result<AckWaiterHandle, SenderError> {
        let wait_start = Instant::now();
        loop {
            let mut wake: Option<watch::Receiver<bool>> = None;
            {
                let mut book = self.lock();
                let owing = Self::owes_verdict(&book, req_id) && !rewrite;
                let taken = book.waiters.contains_key(req_id);
                if !taken && !owing {
                    let (sender, receiver) = oneshot::channel();
                    let (done, _done_receiver) = watch::channel(false);
                    let id = book.next_waiter;
                    book.next_waiter += 1;
                    book.waiters.insert(
                        req_id.to_owned(),
                        AckWaiter {
                            id,
                            verdict: sender,
                            seq: 0,
                            addressable: false,
                            rewrite,
                            done,
                        },
                    );
                    return Ok(AckWaiterHandle { id, receiver });
                }
                if let Some(prev) = book.waiters.get(req_id) {
                    if taken {
                        wake = Some(prev.done.subscribe());
                    }
                }
            }

            if !finish {
                return Err(SenderError::StreamBusy);
            }
            if let Some(mut done) = wake {
                let _ = timeout_at(deadline_or_far(deadline), done.changed()).await;
                continue;
            }
            // **欠着但没人等**：被放弃的那一帧的判决还在路上，而它的到达没有信号 ⇒
            // 这是唯一一处"短轮询才是诚实机制"的地方。有界限，而且**故意**不用调用方的
            // 整份预算：一次 ack 等待之内没回来的判决就是不会回来了。
            if wait_start.elapsed() > ack_timeout {
                return Err(SenderError::StreamBusy);
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// 退休一个等待者：调用方不再等了（上游 `cancelAck`）。**不动计数**。
    ///
    /// 只有当表里坐着的**还是**这个等待者时才摘（上游 `cur == w`）。
    pub fn cancel_ack(&self, req_id: &str, waiter_id: u64) {
        let removed = {
            let mut book = self.lock();
            if book.waiters.get(req_id).is_some_and(|w| w.id == waiter_id) {
                book.waiters.remove(req_id)
            } else {
                None
            }
        };
        if let Some(waiter) = removed {
            waiter.resolve();
        }
    }

    /// 为即将写出的一帧预留它在 `req_id` 写序里的位置，并在收尾帧写过之后拒绝非收尾帧
    /// （上游 `beginStreamFrameLocked`）。调用方**持有写者槽** —— 正是这一点让这次拒绝
    /// 无懈可击：封口与它围住的写决定在**同一个**临界区里。
    #[must_use]
    pub fn begin_stream_frame(
        &self,
        req_id: &str,
        stream_id: &str,
        waiter: Option<&AckWaiterHandle>,
        finish: bool,
    ) -> bool {
        let mut book = self.lock();
        if !book.streams.contains_key(req_id) {
            Self::prune_streams(&mut book);
            book.streams.insert(req_id.to_owned(), StreamAcks::new());
        }
        let streams = book.streams.get_mut(req_id).expect("just inserted above");
        if streams.is_sealed(stream_id) && !finish {
            return false;
        }
        // 在自增**之前**读：每一帧更早的都有判决了，说明 `acked` 追上了 `sent`。
        let clean = streams.acked == streams.sent;
        streams.sent += 1;
        let seq = streams.sent;
        if finish {
            let _ = streams.sealed.insert(stream_id.to_owned());
        }
        if let Some(waiter) = waiter {
            if let Some(entry) = book.waiters.get_mut(req_id) {
                if entry.id == waiter.id {
                    entry.seq = seq;
                    entry.addressable = clean || entry.rewrite;
                }
            }
        }
        true
    }

    /// 把一帧**从未到达 socket** 的帧的位置还回去（上游 `abortStreamFrameLocked`）——
    /// 否则一次失败的写就会让这个 `req_id` 上所有后续判决错位。封口**不**还：一轮里收尾帧
    /// 失败，这一轮反正结束了。调用方持有写者槽。
    pub fn abort_stream_frame(&self, req_id: &str) {
        let mut book = self.lock();
        if let Some(streams) = book.streams.get_mut(req_id) {
            if streams.sent > 0 {
                streams.sent -= 1;
            }
        }
    }

    /// 退休协议已经忘掉的轮次（上游 `pruneStreamsLocked`）。
    ///
    /// **判据是"结清"而不是"封口"**：真正常要成立的是"这个 `req_id` 上不会再有判决来了"——
    /// `acked == sent` 时服务端什么都不欠，所以不会有东西晚到、来跟被重置的计数配对。
    /// 还欠着一次的条目**无论多老都留着**，因为那正是序号存在的理由。
    fn prune_streams(book: &mut AckBookInner) {
        if book.streams.len() < STREAM_ACKS_MAX {
            return;
        }
        let now = Instant::now();
        let settled = STREAM_MAX_AGE;
        let reachable = STREAM_MAX_AGE.saturating_add(STREAM_CLOSE_TIMEOUT);
        book.streams.retain(|_, streams| {
            let age = now.duration_since(streams.at);
            if streams.acked >= streams.sent && age > settled {
                return false;
            }
            // 条目不再保护任何东西的第二条路：`cancel_ack` **故意**留下欠账，所以一轮判决
            // 永不到来的轮次永远不会结清。结束它的不是年龄本身，而是**我们自己触达范围的
            // 尽头**：流存储在一个 `STREAM_MAX_AGE` 之后驱逐句柄，过了那个点就再没有什么能
            // 把收尾帧交给这个 `req_id`；而一个已经握着句柄的收尾器被 `STREAM_CLOSE_TIMEOUT`
            // 兜住。两者都过了，这里就不可能再写出帧，计数也就不保护任何东西。
            age <= reachable
        });
    }
}

/// [`AckBook::await_ack`] 交给调用方的一帧的等待者（上游 `*ackWaiter`）。
#[derive(Debug)]
pub struct AckWaiterHandle {
    /// 在表里的身份（[`AckBook::cancel_ack`] 与 [`AckBook::begin_stream_frame`] 认它）。
    pub id: u64,
    receiver: oneshot::Receiver<AckResult>,
}

impl AckWaiterHandle {
    /// 等判决。`None` = 等待者被取消了（或被别的路径抢先送走了判决）。
    ///
    /// # Errors
    ///
    /// 超时判为 [`SenderError::StreamAckTimeout`]；调用方的预算先到也判它
    /// （上游 `respondStreamFrame` 的两条分支都返回 `errStreamAckTimeout`）。
    pub async fn wait(
        mut self,
        ack_timeout: Duration,
        deadline: Deadline,
    ) -> Result<Result<(), StreamError>, SenderError> {
        let limit = match deadline {
            Some(deadline) => ack_timeout.min(deadline.saturating_duration_since(Instant::now())),
            None => ack_timeout,
        };
        match timeout_at((Instant::now() + limit).into(), &mut self.receiver).await {
            Ok(Ok(result)) => {
                if result.code == 0 {
                    Ok(Ok(()))
                } else {
                    Ok(Err(StreamError {
                        code: result.code,
                        message: result.message,
                    }))
                }
            }
            _ => Err(SenderError::StreamAckTimeout),
        }
    }
}
