//! **一条 aibot 长连接的运行循环**：握手 → 心跳 → 读 → 分派（上游
//! `internal/integrations/wecom/wecom_channel.go` 的 `Connect` / `subscribe` / `dispatchFrame` /
//! `pingLoop`，约 **420 行**）。
//!
//! 拆出来是门 ⑩（800 行硬限）与"一格 = 一个面"的共同要求：本文件管**帧的流转**，归一化在
//! [`super::inbound`]，传输在 [`super::socket`]。
//!
//! # 三处不显然的设计（逐条来自上游，别"简化"）
//!
//! 1. **回调在自己的 worker 上跑**，不跑在读循环里：读循环是服务端判决的**唯一**投递者，所以任何
//!    就地处理回调的东西都无法同时等它自己写出的那帧的 ack —— 它在等自己。**一个** worker 而不是
//!    一个池：`WeCom` 按顺序投递一个聊的消息，而 engine 的去重与轮次批量假设那个顺序活着。
//! 2. **队列满了**阻塞**读循环**而不是丢帧。背压的代价是一次重连，丢帧的代价是一个用户的消息没了、
//!    且没有任何东西能说明。这只在还有人继续接收时成立 ⇒ 读循环那一次 `send` 同时盯着 worker 的
//!    存活（见下面的 `worker_gone`）。
//! 3. **读截止时刻在读之前武装，且只在那一处武装。** 它曾经在读返回**之后**武装，于是循环随后做的
//!    一切都在那个窗口里；而只有服务端的 pong 会在一个安静的机器人上重置截止时刻、我们的 ping 每
//!    30 秒才出去一次，所以在繁忙的池子上，下一次读可能在一个**完全健康**的 socket 上超时。
//!    空闲窗口该量的就是空闲。
//!
//! # 取消语义（本仓与上游的**唯一**形态差异，登记 `docs/32` §37 的 D6）
//!
//! 上游把 `ctx` 一路带进来，并用一个 watchdog goroutine 在 `ctx.Done()` 时 `conn.Close()` 把阻塞在
//! `ReadMessage` 上的读循环踢醒。本仓的取消由 **supervisor 对这个 `connect` 任务做 `abort`** 表达
//! （见 `crate::channel::Channel` 的模块文档），所以：
//!
//! - 读循环不需要 watchdog：`abort` 直接丢掉整个 future；
//! - 两个派生任务（心跳 / worker）由 [`AbortOnDrop`] 兜住 —— `connect` 的 future 被丢掉时它们
//!   **也**被 abort，不会留下两个还在读 / 写一条已死 socket 的任务；
//! - worker 死掉时不再"关 socket 把读循环踢醒"，而是让读循环 `select!` 在一份 `watch` 上 ——
//!   同一个效果，少一次对 socket 的操作。

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::channel::{ChannelError, ChannelResult};
use crate::message::SharedInboundHandler;
use crate::wecom::credentials::{classify_subscribe_ack, PlaintextSecret, ProbeError};
use crate::wecom::metrics::Metrics;
use crate::wecom::ws_frame::{
    aibot_chat_type_from_channel, decode_frame, frame_with, subscribe_body, AibotEventCallback,
    AibotMsgCallback, Frame, FrameEnvelope, CMD_EVENT_CALLBACK, CMD_MSG_CALLBACK, CMD_PONG,
    CMD_SERVER_PING, EVENT_DISCONNECTED, PING_INTERVAL,
};
use crate::wecom::ws_sender::{SenderError, WsSender};

use super::inbound::{channel_message_from_callback, UNSUPPORTED_MSG_TYPE_RECEIPT};
use super::socket::WsReader;
use super::{WeComChannel, SUBSCRIBE_TIMEOUT};

/// 回调队列的深度（上游 `callbackQueueDepth`）。
///
/// 超过它，读循环就停下等，socket 停止被排干，`WeCom` 注意到这点并换掉这条连接 —— 而这是正确的
/// 结局：一个跟不上节奏的副本应当把这个机器人交给跟得上的那个，而不是悄悄丢掉它够不着的消息。
pub const CALLBACK_QUEUE_DEPTH: usize = 64;

/// 读循环的空闲窗口（上游 `readDeadline`）：这一段里没有字节到达就认为 socket 死了。
///
/// 它**必须**比 [`PING_INTERVAL`] 宽出一截，否则一次稍晚的 pong 就会造成一次误判重连。
pub const READ_DEADLINE: Duration = Duration::from_secs(90);

/// 分派一帧需要的**平台上下文**（[`WeComChannel`] 里那些与连接无关的字段的一份拷贝）。
///
/// 单独成一个结构，是因为 worker 活在自己的任务上、而 `Channel::connect` 拿的是 `&self`：与其让
/// worker 借 `&WeComChannel`（那会把生命周期绑到 `connect` 的栈上），不如把它要的四件东西拷出来。
pub(super) struct WorkerContext {
    pub(super) bot_id: String,
    pub(super) bot_display_name: String,
    pub(super) handler: SharedInboundHandler,
    pub(super) metrics: &'static dyn Metrics,
}

/// 一个"被 drop 就 `abort`"的任务句柄。
///
/// 存在的唯一理由：`Channel::connect` 的取消是**任务 abort**，而 `tokio::spawn` 出来的任务是
/// **脱离**的 —— 不留这个兜底，一次停机之后会有两个任务继续持有一条已死 socket 的读写两半。
struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl AbortOnDrop {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(handle))
    }

    /// 等它自然结束（**不** abort），然后放弃兜底。
    async fn finish(mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// 登记表里那条"这个安装的 socket 是它"，以及它在任何退出路径上的撤除
/// （上游 `defer c.senders.clear(...)`）。
struct RegistryGuard {
    senders: Arc<dyn super::SenderRegistry>,
    installation_id: mc_core::id::Id,
    sender: Arc<WsSender>,
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        self.senders.clear(self.installation_id, &self.sender);
    }
}

/// 跑一条连接，直到链路结束（正常收尾 ⇒ `Ok(())`；不可本地恢复地断开 ⇒ `Err`）。
///
/// 这是 `Channel::connect` 的全部实现。
///
/// # Errors
///
/// [`ChannelError::InvalidConfig`]（缺 handler / 缺凭据 / URL 形态不对）、[`ChannelError::Auth`]
/// （凭据被明确拒）、[`ChannelError::Transport`]（拨号 / 读写 / 握手失败）。
pub(super) async fn run(channel: &WeComChannel) -> ChannelResult<()> {
    let Some(handler) = channel.handler.clone() else {
        return Err(ChannelError::InvalidConfig {
            kind: "wecom".to_string(),
            reason: "inbound handler not configured".to_string(),
        });
    };
    if channel.bot_id.is_empty() || channel.secret.is_empty() {
        return Err(ChannelError::InvalidConfig {
            kind: "wecom".to_string(),
            reason: "bot_id / secret not configured".to_string(),
        });
    }
    let ws_url = if channel.ws_url.is_empty() {
        super::DEFAULT_WS_URL.to_string()
    } else {
        channel.ws_url.clone()
    };
    if !(ws_url.starts_with("wss://") || ws_url.starts_with("ws://")) {
        return Err(ChannelError::InvalidConfig {
            kind: "wecom".to_string(),
            reason: "the websocket url must be ws:// or wss://".to_string(),
        });
    }

    let dialed = channel.dialer.dial(&ws_url).await?;
    let sender = Arc::new(WsSender::new(dialed.sink));
    let mut reader = dialed.reader;

    // ---- 握手 ----
    if let Err(error) = subscribe(&channel.bot_id, &channel.secret, &sender, &mut reader).await {
        classify_connect_failure(channel.metrics(), &error);
        return Err(error);
    }

    // ---- 装上发送者（出站回复器 / 中继靠它找这条活着的 socket）----
    let registry_guard = match (channel.installation_id, channel.senders.as_ref()) {
        (Some(installation_id), Some(senders)) => {
            senders.set(installation_id, Arc::clone(&sender));
            Some(RegistryGuard {
                senders: Arc::clone(senders),
                installation_id,
                sender: Arc::clone(&sender),
            })
        }
        _ => None,
    };

    // ---- 心跳 ----
    let ping_task = AbortOnDrop::new(tokio::spawn(ping_loop(Arc::clone(&sender))));

    // ---- 回调 worker ----
    let context = WorkerContext {
        bot_id: channel.bot_id.clone(),
        bot_display_name: channel.bot_display_name.clone(),
        handler,
        metrics: channel.metrics(),
    };
    let (callbacks_tx, callbacks_rx) = mpsc::channel::<Vec<u8>>(CALLBACK_QUEUE_DEPTH);
    let (worker_gone_tx, worker_gone_rx) = watch::channel(false);
    let worker_error: Arc<std::sync::Mutex<Option<ChannelError>>> =
        Arc::new(std::sync::Mutex::new(None));
    let worker = AbortOnDrop::new(tokio::spawn(worker_loop(
        context,
        Arc::clone(&sender),
        callbacks_rx,
        worker_gone_tx,
        Arc::clone(&worker_error),
    )));

    // ---- 读循环 ----
    let mut worker_gone_rx = worker_gone_rx.clone();
    let outcome = pump(&sender, &mut reader, &callbacks_tx, &mut worker_gone_rx).await;

    // ---- 收尾：先关上队列、让 worker 把手上那条做完，再取它的错 ----
    drop(callbacks_tx);
    worker.finish().await;
    drop(ping_task);
    drop(registry_guard);

    outcome?;
    // worker 的错才是**真原因**，随后那个读错误只是我们为了走到这里而关掉的（或本来就已经断了的）
    // socket。
    //
    // 只在没有别的错误时上报：一次停机（任务 abort）撞上一条正在飞的线程是**寻常的停止**，把那条
    // 线程的错误升上来会报一条假的"连接带着错误退出"。
    if let Some(error) = worker_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        return Err(error);
    }
    Ok(())
}

/// 握手 + 等 ack（上游 `subscribe`）。
///
/// # 为什么这里要**同时**读
///
/// 上游的 `subscribe` 直接在 `conn` 上 `ReadMessage` 等 ack，而 `sender` 只负责写。本仓的
/// [`WsSender::subscribe`] 写完之后在它自己的 ack 账本上等判决，而那个账本**由读循环喂**
/// （`route_response` / `route_raw`）—— 握手阶段还没有读循环 ⇒ 这里临时跑一个"喂 ack"的读，
/// 与 `subscribe` 并发，握手有结果即随 `select!` 一起丢掉。
///
/// # Errors
///
/// [`ChannelError::Auth`] / [`ChannelError::Transport`] / [`ChannelError::InvalidConfig`]。
async fn subscribe(
    bot_id: &str,
    secret: &PlaintextSecret,
    sender: &Arc<WsSender>,
    reader: &mut Box<dyn WsReader>,
) -> ChannelResult<()> {
    let deadline = Instant::now() + SUBSCRIBE_TIMEOUT;
    let body = subscribe_body(bot_id, secret);
    let subscribe_future = sender.subscribe(Some(deadline), body);
    let feeder = async {
        loop {
            match reader.next_message().await {
                Ok(Some(raw)) => {
                    // 大小 / 形态不对的帧在握手阶段被丢掉（上游的 `continue`）。
                    let _ = sender.route_raw(&raw);
                }
                Ok(None) => {
                    return Err(ChannelError::Transport {
                        message: "wecom: link closed during subscribe".to_string(),
                    });
                }
                Err(error) => return Err(error),
            }
        }
    };
    tokio::pin!(feeder);

    let result = tokio::select! {
        result = subscribe_future => result,
        reason = &mut feeder => return Err(reason.unwrap_or_else(|error| error)),
    };
    match result {
        Ok(_) => {
            tracing::info!(bot_id = %bot_id, "wecom: subscribe ok");
            Ok(())
        }
        Err(error) => Err(map_subscribe_error(error)),
    }
}

/// 把一次握手失败翻成链路错误，并且**只用 `errcode`** 说话。
///
/// 上游这一条的理由逐字：同一个码在两处必须得到同一个答案 —— 安装期的凭据探针与重连握手共用
/// [`classify_subscribe_ack`]，所以"限频，等等"不会在一处是 `Unverifiable`、在另一处变成
/// "去修这个安装"。
fn map_subscribe_error(error: SenderError) -> ChannelError {
    match error {
        SenderError::Api { code, message, .. } => {
            if let Err(ProbeError::Rejected { errcode }) = classify_subscribe_ack(code) {
                ChannelError::Auth {
                    message: format!("wecom: subscribe rejected (errcode {errcode})"),
                }
            } else {
                // `errmsg` 进日志（它可能含 bot 名）、**不进**错误值（错误值会被 HTTP 层序列化）。
                tracing::warn!(errcode = code, errmsg = %message, "wecom: subscribe not verified");
                ChannelError::Transport {
                    message: format!("wecom: could not verify this bot (errcode {code})"),
                }
            }
        }
        SenderError::Sink(_) | SenderError::WriteAttempted { .. } => ChannelError::Transport {
            message: "wecom: subscribe write failed".to_string(),
        },
        SenderError::AckTimeout | SenderError::AckAbandoned { .. } | SenderError::NotAttempted => {
            ChannelError::Transport {
                message: "wecom: subscribe ack did not arrive".to_string(),
            }
        }
        SenderError::Frame(_) | SenderError::FrameTooLarge { .. } | SenderError::Body(_) => {
            ChannelError::InvalidConfig {
                kind: "wecom".to_string(),
                reason: "the subscribe frame was rejected locally".to_string(),
            }
        }
        _ => ChannelError::Transport {
            message: "wecom: subscribe failed".to_string(),
        },
    }
}

/// 一次连接失败的分类（上游 `subscribe` 里那两处 `mx()`）。
fn classify_connect_failure(metrics: &'static dyn Metrics, error: &ChannelError) {
    match error {
        ChannelError::Auth { .. } => metrics.record_auth_failure(),
        _ => metrics.record_connect_failure(),
    }
}

/// 心跳（上游 `pingLoop`）：每 [`PING_INTERVAL`] 写一帧 `ping`。
///
/// 写失败只记日志、不自己拆掉循环 —— 它会以"下一次读失败"的形式浮出来（上游逐字）。
async fn ping_loop(sender: Arc<WsSender>) {
    let mut ticker = tokio::time::interval(PING_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval` 的第一次 tick 立刻完成 —— 先消费掉它，否则第一条 ping 会在握手刚结束时抢跑。
    ticker.tick().await;
    loop {
        ticker.tick().await;
        if let Err(error) = sender.ping().await {
            tracing::warn!(error = %error, "wecom: ping write failed");
        }
    }
}

/// 回调 worker（上游 `Connect` 里那个 `go func() { for env := range callbacks … }`）。
async fn worker_loop(
    context: WorkerContext,
    sender: Arc<WsSender>,
    mut callbacks: mpsc::Receiver<Vec<u8>>,
    gone: watch::Sender<bool>,
    error: Arc<std::sync::Mutex<Option<ChannelError>>>,
) {
    while let Some(raw) = callbacks.recv().await {
        if let Err(failure) = dispatch_callback(&context, &sender, &raw).await {
            *error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(failure);
            break;
        }
    }
    // 通知读循环：这个 worker 已经走了，它那一次 `send` 不会再有接收者。
    let _ = gone.send(true);
}

/// 读循环（上游 `Connect` 里那个 `for`）。
async fn pump(
    sender: &Arc<WsSender>,
    reader: &mut Box<dyn WsReader>,
    callbacks: &mpsc::Sender<Vec<u8>>,
    worker_gone: &mut watch::Receiver<bool>,
) -> ChannelResult<()> {
    loop {
        // 只有在**没有**字节到达这一段窗口时才认为 socket 死了。武装在读**之前**、且只在这里武装
        // （见模块文档第 3 条）。
        let frame = tokio::select! {
            // worker 停了 ⇒ 它那一次 `send` 不再有接收者，而队列可能还是满的。停在这里，把错误
            // 交给收尾那一步去取（worker 的错才是真原因）。
            _ = worker_gone.changed() => return Ok(()),
            result = tokio::time::timeout_at(
                (Instant::now() + READ_DEADLINE).into(),
                reader.next_message(),
            ) => match result {
                Err(_elapsed) => return Err(ChannelError::Transport {
                    message: "wecom: no frame arrived within the read deadline".to_string(),
                }),
                // 对端正常关闭 ⇒ 这条链路**正常**结束，不是失败。
                Ok(Ok(None)) => return Ok(()),
                Ok(Ok(Some(raw))) => raw,
                Ok(Err(error)) => return Err(error),
            },
        };

        // `tokio-tungstenite` 会自动消化的 ping / pong 在这里以一个空帧出现（见 `socket.rs`）。
        if frame.is_empty() {
            continue;
        }
        // 一帧的分类靠 M7-16 的 `decode_frame`；一个坏帧**不**撕掉 socket（上游逐字：只有传输 /
        // handler 错误才升级）。
        let is_callback = match decode_frame(&frame) {
            Ok(Frame::MsgCallback { .. } | Frame::EventCallback { .. }) => true,
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(error = %error, size = frame.len(), "wecom: bad frame");
                continue;
            }
        };
        if !is_callback {
            // ack、ping、pong 与不认识的帧留在读循环上：它们是 worker 自己写出的帧在等的东西。
            dispatch_control(sender, &frame).await?;
            continue;
        }

        // 先试一次不排队（绝大多数情况）；满了再阻塞 —— 而阻塞是**故意**的（见模块文档第 2 条）。
        match callbacks.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
            Err(mpsc::error::TrySendError::Full(frame)) => {
                // worker 落后了。阻塞是刻意的选择，而它也正是运维想知道的事 —— 从这里开始这条
                // socket 不再被排干，而如果它持续下去，`WeCom` 会换掉这条连接。
                tokio::select! {
                    result = callbacks.send(frame) => {
                        if result.is_err() {
                            return Ok(());
                        }
                    }
                    _ = worker_gone.changed() => return Ok(()),
                }
            }
        }
    }
}

/// worker 那一侧的分派：解帧 → 计数 → [`dispatch_callback`]。
async fn dispatch_callback(
    context: &WorkerContext,
    sender: &Arc<WsSender>,
    raw: &[u8],
) -> ChannelResult<()> {
    context.metrics.record_callback_queued();
    let Ok(envelope) = serde_json::from_slice::<FrameEnvelope>(raw) else {
        tracing::warn!(size = raw.len(), "wecom: bad frame envelope");
        return Ok(());
    };
    match envelope.cmd.as_str() {
        CMD_MSG_CALLBACK => {
            let Ok(callback) = serde_json::from_value::<AibotMsgCallback>(envelope.body.clone())
            else {
                tracing::warn!("wecom: bad aibot_msg_callback body");
                return Ok(());
            };
            dispatch_message(context, sender, &envelope.headers.req_id, &callback).await
        }
        CMD_EVENT_CALLBACK => {
            let Ok(event) = serde_json::from_value::<AibotEventCallback>(envelope.body.clone())
            else {
                tracing::warn!("wecom: bad aibot_event_callback body");
                return Ok(());
            };
            dispatch_event(&event)
        }
        // 读循环按理只把回调交给 worker；万一不是，控制帧在这里也能被正确处理。
        _ => dispatch_envelope(sender, &envelope).await,
    }
}

/// 读循环那一侧的分派（回调以外的帧）。
async fn dispatch_control(sender: &Arc<WsSender>, raw: &[u8]) -> ChannelResult<()> {
    let Ok(envelope) = serde_json::from_slice::<FrameEnvelope>(raw) else {
        tracing::warn!(size = raw.len(), "wecom: bad frame envelope");
        return Ok(());
    };
    if envelope.cmd == CMD_EVENT_CALLBACK {
        let Ok(event) = serde_json::from_value::<AibotEventCallback>(envelope.body.clone()) else {
            tracing::warn!("wecom: bad aibot_event_callback body");
            return Ok(());
        };
        return dispatch_event(&event);
    }
    dispatch_envelope(sender, &envelope).await
}

/// 上游 `dispatchFrame` 的 `aibot_event_callback` 那一支。
fn dispatch_event(event: &AibotEventCallback) -> ChannelResult<()> {
    if event.event.eventtype == EVENT_DISCONNECTED {
        // 另一条连接把我们顶掉了。返回，好让 supervisor 退避后重连（它接着会再顶掉**那**一条
        // —— 最后写的赢）。
        return Err(ChannelError::Transport {
            message: "wecom: superseded by another connection".to_string(),
        });
    }
    tracing::debug!(event_type = %event.event.eventtype, "wecom: event");
    Ok(())
}

/// 上游 `dispatchFrame` 的 `default` 分支：服务端主动 ping / pong / 匿名 ack / 不认识的命令。
async fn dispatch_envelope(sender: &Arc<WsSender>, envelope: &FrameEnvelope) -> ChannelResult<()> {
    match envelope.cmd.as_str() {
        // 服务端主动 ping（按文档罕见，但防御性地处理）。
        CMD_SERVER_PING => sender
            .write(&frame_with(
                &envelope.headers.req_id,
                CMD_PONG,
                serde_json::Value::Null,
            ))
            .await
            .map_err(|_| ChannelError::Transport {
                message: "wecom: pong write failed".to_string(),
            }),
        // 我们自己的 ping 的 ack —— no-op。
        CMD_PONG => Ok(()),
        _ => {
            // 匿名 ack（cmd 为空）与不认识的命令走同一条路 —— 先把它交给正在等它的人；没人等且
            // `errcode` 非零就记一条 WARN，好让一次失败的出站不必抓包就能看见。
            if sender.route_response(envelope) {
                return Ok(());
            }
            if envelope.errcode != 0 {
                tracing::warn!(
                    errcode = envelope.errcode,
                    errmsg = %envelope.error_message,
                    req_id = %envelope.headers.req_id,
                    "wecom: server ack error"
                );
            }
            Ok(())
        }
    }
}

/// 一条 `aibot_msg_callback` 的完整处置（上游 `dispatchFrame` 的第一个 `case`）。
async fn dispatch_message(
    context: &WorkerContext,
    sender: &Arc<WsSender>,
    req_id: &str,
    callback: &AibotMsgCallback,
) -> ChannelResult<()> {
    let (text, readable) = callback.own_text();
    // 用**解出来的**正文记日志，不是 `callback.text.content`：对每一条语音 / 媒体 / 图文混排回调，
    // 那个字段都是空的，所以记它会在**恰好是运维打开 tracing 想看的那些消息**上打印 len=0。
    // （逐帧埋点归 M7-20 的 `trace.rs`；这里只留一条不含正文的 debug。）
    tracing::debug!(
        msg_type = %callback.msgtype,
        msg_id = %callback.msgid,
        text_runes = text.chars().count(),
        "wecom: inbound callback"
    );
    let message = channel_message_from_callback(
        &context.bot_id,
        &context.bot_display_name,
        callback,
        &text,
        req_id,
    );
    if !readable {
        // 这条消息里没有任何能读的东西：adapter 不认识的一种（一张位置卡），或者一种认识的类型
        // 到来时缺了让它可用的那**一个**字段（一条识别结果为空的语音、一条不带 url 的图片回调）。
        // 沉默读起来像一个坏掉的机器人，所以回到**同一个**聊里说一句一行回执并停下。
        // 尽力而为：一次发送失败降级成从前那条静默丢弃。
        tracing::debug!(
            msg_type = %callback.msgtype,
            msg_id = %callback.msgid,
            "wecom: unsupported message kind, replying with a receipt"
        );
        let chat_type = aibot_chat_type_from_channel(message.source.chat_type);
        if let Err(error) = sender
            .send_text(
                &message.source.chat_id,
                chat_type,
                UNSUPPORTED_MSG_TYPE_RECEIPT,
                None,
            )
            .await
        {
            tracing::debug!(error = %error, msg_id = %callback.msgid, "wecom: receipt send failed");
        }
        return Ok(());
    }
    context.handler.handle(message).await
}
