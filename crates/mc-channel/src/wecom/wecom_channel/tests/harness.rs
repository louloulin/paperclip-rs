//! **端到端回路的替身**：一个"假平台"WS 对端 + 一张会把活 socket 交给出站面的登记表。
//!
//! `docs/60-M7-PLAN.md` §4.2 的替身纪律第一条：**只替平台 wire，不替业务路径**。本文件替的是
//! `WeCom` 那侧的 socket —— 它记下客户端写出的每一帧、按 `req_id` 自动回 ack（真实平台的行为），
//! 并允许用例把入站帧注入进来。归一化、Router、回复器、中继、收尾判决**全部是真代码**。
//!
//! 为什么不是真 WS 服务端：`tokio-tungstenite` 在本仓**没有** `handshake` feature（`docs/60` §3.1
//! 的依赖面一次接死）⇒ 起不了一个真实的 WS 服务端。这与 `dingtalk` / `lark` 的端到端用例是同一条
//! 约束、同一个答案。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;
use tokio::sync::watch;

use crate::channel::ChannelResult;
use crate::message::{InboundHandler, SharedInboundHandler};
use crate::wecom::outbound::{LiveSender, SenderLookup};
use crate::wecom::wecom_channel::socket::{DialedConnection, WsDialer, WsReader};
use crate::wecom::wecom_channel::SenderRegistry;
use crate::wecom::ws_sender::{SinkError, WsSender, WsSink};

// =====================================================================
// 假的平台对端
// =====================================================================

/// 对端的状态（读与写在同一把锁下 —— 真实平台也是"一条连接的两半在同一个进程里"）。
#[derive(Default)]
struct PeerState {
    /// 待投给客户端（替身"推"下来的帧）。
    inbound: VecDeque<Vec<u8>>,
    /// 客户端写出的帧（替身收到的）。
    written: Vec<serde_json::Value>,
    closed: bool,
}

/// 一个脚本化的 aibot 对端。
pub struct FakePeer {
    state: Mutex<PeerState>,
    changes: watch::Sender<u64>,
    /// 自动回 ack 的开关（真实平台对我们写出的每一帧都回 ack）。
    auto_ack: bool,
}

impl FakePeer {
    /// 造一个对端；`auto_ack` = 是否按 `req_id` 自动回 `errcode 0`（真实平台的行为）。
    pub fn new(auto_ack: bool) -> Arc<Self> {
        let (changes, _) = watch::channel(0_u64);
        Arc::new(Self {
            state: Mutex::new(PeerState::default()),
            changes,
            auto_ack,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PeerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn bump(&self) {
        let next = self.changes.borrow().wrapping_add(1);
        let _ = self.changes.send(next);
    }

    /// 注入一帧（入站方向）。
    ///
    /// `needless_pass_by_value`：帧在这里**被序列化之后即丢**，所以按值收下的确"没消费"它 ——
    /// 但调用点写 `push(msg_callback(...))` 比写 `push(&msg_callback(...))` 更贴近"造一帧、投进去"
    /// 这件事，而多的一次 `&` 只是噪音。
    #[allow(clippy::needless_pass_by_value)]
    pub fn push(&self, frame: serde_json::Value) {
        let bytes = serde_json::to_vec(&frame).expect("frame");
        self.lock().inbound.push_back(bytes);
        self.bump();
    }

    /// 注入一段**原始**字节（用来喂坏帧）。
    pub fn push_raw(&self, bytes: Vec<u8>) {
        self.lock().inbound.push_back(bytes);
        self.bump();
    }

    /// 替身已经收到的帧。
    pub fn written(&self) -> Vec<serde_json::Value> {
        self.lock().written.clone()
    }

    /// 等一帧**符合条件的**客户端写出帧；等到或超时。
    pub async fn wait_for_written<F>(
        &self,
        mut matches: F,
        within: std::time::Duration,
    ) -> Option<serde_json::Value>
    where
        F: FnMut(&serde_json::Value) -> bool,
    {
        let mut revision = self.changes.subscribe();
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if let Some(found) = self.written().into_iter().find(|frame| matches(frame)) {
                return Some(found);
            }
            if revision.changed().await.is_err() {
                return None;
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// 对端关闭（读半随后返回 `None` = 正常收尾）。
    pub fn close(&self) {
        self.lock().closed = true;
        self.bump();
    }

    /// 从替身上拨一个连接出来（`Dialer` 的替身入口）。
    pub fn dialer(self: &Arc<Self>) -> Arc<dyn WsDialer> {
        Arc::new(FakeDialer {
            peer: Arc::clone(self),
        })
    }
}

struct FakeDialer {
    peer: Arc<FakePeer>,
}

#[async_trait]
impl WsDialer for FakeDialer {
    async fn dial(&self, url: &str) -> ChannelResult<DialedConnection> {
        // 拨号到"假平台"也需要一个地址；用一个固定文案替代真 socket（**不**回显输入 —— 见
        // `socket.rs` 的凭据纪律：拨号错误不回显 URL）。
        let _ = url;
        Ok(DialedConnection {
            sink: Box::new(FakeSink {
                peer: Arc::clone(&self.peer),
            }),
            reader: Box::new(FakeReader {
                peer: Arc::clone(&self.peer),
                revision: self.peer.changes.subscribe(),
            }),
        })
    }
}

struct FakeSink {
    peer: Arc<FakePeer>,
}

#[async_trait]
impl WsSink for FakeSink {
    async fn write_text(&mut self, payload: &[u8], _deadline: Instant) -> Result<(), SinkError> {
        let frame: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|_| SinkError::before_write("frame is not json"))?;
        let req_id = frame
            .get("headers")
            .and_then(|headers| headers.get("req_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        {
            let mut state = self.peer.lock();
            if state.closed {
                return Err(SinkError::write_attempted("the fake peer is closed"));
            }
            state.written.push(frame);
            if self.peer.auto_ack && !req_id.is_empty() {
                // 真实平台对我们写出的每一帧都回一个带同样 `req_id` 的 ack。
                let ack = serde_json::json!({
                    "headers": { "req_id": req_id },
                    "errcode": 0,
                    "errmsg": "",
                });
                state
                    .inbound
                    .push_back(serde_json::to_vec(&ack).expect("ack"));
            }
        }
        self.peer.bump();
        Ok(())
    }

    async fn close(&mut self) -> Result<(), SinkError> {
        self.peer.lock().closed = true;
        self.peer.bump();
        Ok(())
    }
}

struct FakeReader {
    peer: Arc<FakePeer>,
    revision: watch::Receiver<u64>,
}

#[async_trait]
impl WsReader for FakeReader {
    async fn next_message(&mut self) -> ChannelResult<Option<Vec<u8>>> {
        loop {
            {
                let mut state = self.peer.lock();
                if let Some(frame) = state.inbound.pop_front() {
                    return Ok(Some(frame));
                }
                if state.closed {
                    return Ok(None);
                }
            }
            if self.revision.changed().await.is_err() {
                return Ok(None);
            }
        }
    }

    async fn close(&mut self) {
        self.peer.lock().closed = true;
        self.peer.bump();
    }
}

// =====================================================================
// 端口的替身
// =====================================================================

/// 记账型入站入口（`Router` 的替身，用来单独验"一条帧变成了哪条归一化消息"）。
#[derive(Default)]
pub struct RecordingHandler {
    /// 收到的消息。
    pub seen: Mutex<Vec<InboundMessage>>,
    /// 被调用了几次。
    pub calls: AtomicUsize,
}

#[async_trait]
impl InboundHandler for RecordingHandler {
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message);
        Ok(())
    }
}

/// 共享的入站入口，方便 `WeComChannel::new` 与断言两边各持一份。
pub fn shared(handler: Arc<RecordingHandler>) -> SharedInboundHandler {
    handler
}

/// 同时实现**写**面（[`SenderRegistry`]）与**读**面（[`SenderLookup`]）的登记表。
///
/// M7-20 的生产实现（`senders.rs`）就是这一张表的两个面；本片的用例先给出一个最小版本，好让
/// "出站回复走的是**同一条**活 socket"这件事在回路里是真的。
#[derive(Default)]
pub struct TestSenders {
    live: Mutex<HashMap<Id, Arc<WsSender>>>,
}

impl TestSenders {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 这条安装现在有没有活 socket（诊断 / 断言用）。
    pub fn has(&self, installation_id: Id) -> bool {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&installation_id)
    }
}

impl SenderRegistry for TestSenders {
    fn set(&self, installation_id: Id, sender: Arc<WsSender>) {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(installation_id, sender);
    }

    fn clear(&self, installation_id: Id, sender: &Arc<WsSender>) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // 令牌语义：**只**在表里仍是这把 socket 时撤（晚到的 `clear` 不许抹掉新连接）。
        if live
            .get(&installation_id)
            .is_some_and(|current| Arc::ptr_eq(current, sender))
        {
            live.remove(&installation_id);
        }
    }
}

impl SenderLookup for TestSenders {
    fn get(&self, installation_id: Id) -> Option<Arc<dyn LiveSender>> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&installation_id)
            .map(|sender| Arc::clone(sender) as Arc<dyn LiveSender>)
    }
}

// =====================================================================
// 帧的构造
// =====================================================================

/// 一条 `aibot_msg_callback`（入站方向）。
///
/// `needless_pass_by_value`：理由与 [`FakePeer::push`] 同（body 被搬进一个 `json!` 字面量）。
#[allow(clippy::needless_pass_by_value)]
pub fn msg_callback(body: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "cmd": crate::wecom::ws_frame::CMD_MSG_CALLBACK,
        "headers": { "req_id": "req-1" },
        "body": body,
    })
}

/// 一条 `aibot_event_callback`（入站方向）。
pub fn event_callback(event_type: &str) -> serde_json::Value {
    serde_json::json!({
        "cmd": crate::wecom::ws_frame::CMD_EVENT_CALLBACK,
        "headers": { "req_id": "req-event" },
        "body": { "event": { "eventtype": event_type } },
    })
}

/// 一条单聊文本消息的 body。
pub fn text_message(msg_id: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "msgid": msg_id,
        "aibotid": "bot_1",
        "chatid": "user_1",
        "chattype": "single",
        "from": { "userid": "user_1" },
        "msgtype": "text",
        "text": { "content": content },
    })
}

/// 从一段原样文本里取 `markdown.content`（出站断言用）。
pub fn markdown_of(frame: &serde_json::Value) -> String {
    frame
        .get("body")
        .and_then(|body| body.get("markdown"))
        .and_then(|markdown| markdown.get("content"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 一帧出站帧的 `cmd`。
pub fn cmd_of(frame: &serde_json::Value) -> &str {
    frame
        .get("cmd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// 一帧出站帧是不是某个 `cmd` 的推送。
pub fn is_cmd(frame: &serde_json::Value, cmd: &str) -> bool {
    cmd_of(frame) == cmd
}

/// 该帧里 `body.chatid` / `body.chat_type`（地址断言用）。
pub fn address_of(frame: &serde_json::Value) -> (String, i64) {
    let body = frame.get("body").cloned().unwrap_or_default();
    (
        body.get("chatid")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        body.get("chat_type")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
    )
}
