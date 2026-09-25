//! 会话与帧循环用例的**脚手架**：脚本化内存 socket / 拨号器 / 引导端口 / 事件汇 + 帧构造。
//!
//! - **写者**：M7-11（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §28）。
//! - 拆出来是**门 ⑩**（800 行硬限）的要求，边界是"工具 ∥ 断言"：本文件里没有一条 `#[test]`。
//! - 与 `dingtalk/stream/tests.rs` 的三段式一致：**不睡真觉、不开真 socket**；
//!   唯一的真时间是"心跳 / 退避 / 读超时"那几条用毫秒级旋钮量出来的用例。
//!
//! 条目一律 `pub(super)`：`tests`（父模块）与 `tests::supervised`（退避重连 + 租约那一半）
//! 都要用它们。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use super::super::{
    Connector, EventEmitter, SessionKnobs, SessionOutcome, StopSignal, WsConnection, WsDialer,
    WsEvent,
};
use crate::channel::{ChannelError, ChannelResult};
use crate::lark::params::{AppSecret, InstallationCredentials};
use crate::lark::ws_endpoint::{EndpointFetcher, WsEndpoint};
use crate::lark::ws_frame::{
    Frame, FrameHeader, FRAME_HEADER_TYPE_KEY, FRAME_HEADER_TYPE_PING, FRAME_METHOD_CONTROL,
    FRAME_METHOD_DATA,
};
use crate::lark::ws_frame_decoder::{FrameDecoder, LarkInboundEvent, LarkJsonFrameDecoder};

// =====================================================================
// 脚本化的内存 socket
// =====================================================================

/// 会话**写过**的东西（用例断言的就是它）。
#[derive(Debug, Default)]
pub(super) struct SocketLog {
    pub(super) written: Vec<Frame>,
    pub(super) closes: usize,
    /// 第 N 次写返回错误（`None` = 永不失败，1 = 第一次写就失败，2 = 第二次…）。
    pub(super) fail_write_at: Option<usize>,
    pub(super) attempts: usize,
}

impl SocketLog {
    pub(super) fn pings(&self) -> Vec<&Frame> {
        self.written
            .iter()
            .filter(|frame| {
                frame.method == FRAME_METHOD_CONTROL && frame.frame_type() == FRAME_HEADER_TYPE_PING
            })
            .collect()
    }

    pub(super) fn acks(&self) -> Vec<&Frame> {
        self.written
            .iter()
            .filter(|frame| {
                frame.method == FRAME_METHOD_DATA
                    && frame.payload_type.is_empty()
                    && frame.payload.is_some()
            })
            .collect()
    }

    pub(super) fn ack_codes(&self) -> Vec<i64> {
        self.acks()
            .iter()
            .filter_map(|frame| {
                serde_json::from_slice::<serde_json::Value>(frame.payload_bytes())
                    .ok()
                    .and_then(|value| value["code"].as_i64())
            })
            .collect()
    }
}

/// 一条脚本化的会话：按顺序吐事件；脚本耗尽后**挂住**（除非 `end_after_script`）。
pub(super) struct ScriptedSocket {
    pub(super) events: VecDeque<ChannelResult<WsEvent>>,
    pub(super) end_after_script: bool,
    pub(super) log: Arc<Mutex<SocketLog>>,
}

#[async_trait]
impl WsConnection for ScriptedSocket {
    async fn next_event(&mut self) -> Option<ChannelResult<WsEvent>> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        if self.end_after_script {
            // 流结束 ⇒ 连接器读作"对端正常关闭"（`WsEvent::Closed` 那一支）。
            return None;
        }
        // 挂住：让用例去断言"停机 / ping / 读超时"这些**空闲**路径。
        std::future::pending().await
    }

    async fn send_binary(&mut self, bytes: &[u8]) -> ChannelResult<()> {
        let mut log = self.log.lock().expect("log");
        log.attempts += 1;
        if Some(log.attempts) == log.fail_write_at {
            return Err(ChannelError::Transport {
                message: "scripted write failure".to_string(),
            });
        }
        let frame = Frame::unmarshal(bytes).expect("会话写出的必须是合法帧");
        log.written.push(frame);
        Ok(())
    }

    async fn close(&mut self) {
        self.log.lock().expect("log").closes += 1;
    }
}

/// 脚本化的拨号器：按顺序交出 socket，并记下每次拨的**端点**。
pub(super) struct ScriptedDialer {
    pub(super) sockets: Mutex<VecDeque<ScriptedSocket>>,
    pub(super) endpoints: Mutex<Vec<WsEndpoint>>,
    pub(super) attempts: AtomicUsize,
    /// 每次拨号的时刻（用例拿它量退避：相邻两次拨号的间隔 ≥ `min_backoff`）。
    pub(super) attempts_at: Mutex<Vec<std::time::Instant>>,
    pub(super) fail: bool,
}

impl ScriptedDialer {
    pub(super) fn new() -> Self {
        Self {
            sockets: Mutex::new(VecDeque::new()),
            endpoints: Mutex::new(Vec::new()),
            attempts: AtomicUsize::new(0),
            attempts_at: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    pub(super) fn with_socket(
        log: &Arc<Mutex<SocketLog>>,
        events: Vec<ChannelResult<WsEvent>>,
        end: bool,
    ) -> Self {
        let dialer = Self::new();
        dialer.push(log, events, end);
        dialer
    }

    pub(super) fn push(
        &self,
        log: &Arc<Mutex<SocketLog>>,
        events: Vec<ChannelResult<WsEvent>>,
        end: bool,
    ) {
        self.sockets
            .lock()
            .expect("sockets")
            .push_back(ScriptedSocket {
                events: events.into_iter().collect(),
                end_after_script: end,
                log: Arc::clone(log),
            });
    }

    pub(super) fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    /// 相邻两次拨号的间隔（上一段 = 退避时长的一种可观测形态）。
    pub(super) fn attempt_gaps(&self) -> Vec<Duration> {
        let times = self.attempts_at.lock().expect("attempts_at");
        times.windows(2).map(|pair| pair[1] - pair[0]).collect()
    }

    pub(super) fn endpoints(&self) -> Vec<WsEndpoint> {
        self.endpoints.lock().expect("endpoints").clone()
    }
}

#[async_trait]
impl WsDialer for ScriptedDialer {
    async fn dial(&self, endpoint: &WsEndpoint) -> ChannelResult<Box<dyn WsConnection>> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.attempts_at
            .lock()
            .expect("attempts_at")
            .push(std::time::Instant::now());
        self.endpoints
            .lock()
            .expect("endpoints")
            .push(endpoint.clone());
        if self.fail {
            // 固定文案：真实实现**不得**把带 `device_id` 的地址透出来（见 `WsDialer` 的文档）。
            return Err(ChannelError::Transport {
                message: "lark ws: websocket handshake failed".to_string(),
            });
        }
        let socket = self.sockets.lock().expect("sockets").pop_front();
        Ok(Box::new(socket.unwrap_or_else(|| ScriptedSocket {
            events: VecDeque::new(),
            end_after_script: false,
            log: Arc::new(Mutex::new(SocketLog::default())),
        })))
    }
}

/// 固定端点（每次调用返回同一份：地址本身是一次性的，但**内容**由本假端口决定）。
pub(super) struct FixedFetcher {
    pub(super) endpoint: Mutex<WsEndpoint>,
    pub(super) calls: AtomicUsize,
    pub(super) fail: bool,
}

impl FixedFetcher {
    pub(super) fn new(ping_interval: Duration) -> Self {
        Self {
            endpoint: Mutex::new(WsEndpoint {
                url: "wss://lark.example/ws?device_id=dev-1&service_id=42".to_string(),
                service_id: 42,
                ping_interval,
                ..WsEndpoint::default()
            }),
            calls: AtomicUsize::new(0),
            fail: false,
        }
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EndpointFetcher for FixedFetcher {
    async fn endpoint(&self, _creds: &InstallationCredentials) -> ChannelResult<WsEndpoint> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(ChannelError::Transport {
                message: "lark ws endpoint: bootstrap failed with status 500".to_string(),
            });
        }
        Ok(self.endpoint.lock().expect("endpoint").clone())
    }
}

/// 记录式事件汇（可选：让第一条事件报基础设施错）。
pub(super) struct RecordingEmitter {
    pub(super) events: Mutex<Vec<LarkInboundEvent>>,
    pub(super) fail: bool,
}

impl RecordingEmitter {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            fail: false,
        })
    }

    pub(super) fn failing() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            fail: true,
        })
    }

    pub(super) fn message_ids(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .map(|event| event.message_id.clone())
            .collect()
    }

    pub(super) fn count(&self) -> usize {
        self.events.lock().expect("events").len()
    }
}

#[async_trait]
impl EventEmitter for RecordingEmitter {
    async fn emit(&self, event: LarkInboundEvent) -> ChannelResult<()> {
        if self.fail {
            return Err(ChannelError::Storage {
                message: "scripted infra failure".to_string(),
            });
        }
        self.events.lock().expect("events").push(event);
        Ok(())
    }
}

// =====================================================================
// 帧与载荷的构造
// =====================================================================

pub(super) fn receive_payload(message_id: &str) -> String {
    json!({
        "schema": "2.0",
        "header": {
            "event_id": format!("ev-{message_id}"),
            "event_type": "im.message.receive_v1",
            "app_id": "cli_app_x"
        },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_sender" } },
            "message": {
                "message_id": message_id,
                "chat_id": "oc-1",
                "chat_type": "p2p",
                "message_type": "text",
                "content": "{\"text\":\"hi\"}"
            }
        }
    })
    .to_string()
}

/// 一条 data 帧（Lark 发给我们的事件帧）。
pub(super) fn data_frame(payload: &str, message_id: &str) -> Vec<u8> {
    Frame {
        service: 42,
        method: FRAME_METHOD_DATA,
        headers: vec![
            FrameHeader::new(FRAME_HEADER_TYPE_KEY, "event"),
            FrameHeader::new("message_id", message_id),
        ],
        payload_encoding: "json".to_string(),
        payload_type: "im.message.receive_v1".to_string(),
        payload: Some(payload.as_bytes().to_vec()),
        ..Frame::default()
    }
    .marshal()
}

/// 一条**分片**的 data 帧（带 `sum` / `seq`）。
pub(super) fn chunk_frame(payload: &str, message_id: &str, sum: i32, seq: i32) -> Vec<u8> {
    Frame {
        service: 42,
        method: FRAME_METHOD_DATA,
        headers: vec![
            FrameHeader::new(FRAME_HEADER_TYPE_KEY, "event"),
            FrameHeader::new("message_id", message_id),
            FrameHeader::new("sum", sum.to_string()),
            FrameHeader::new("seq", seq.to_string()),
        ],
        payload: Some(payload.as_bytes().to_vec()),
        ..Frame::default()
    }
    .marshal()
}

/// 服务端的心跳帧（`Service` 故意给 0：pong 必须用**引导响应**里的 service id）。
pub(super) fn server_ping_frame() -> Vec<u8> {
    Frame {
        method: FRAME_METHOD_CONTROL,
        headers: vec![FrameHeader::new(
            FRAME_HEADER_TYPE_KEY,
            FRAME_HEADER_TYPE_PING,
        )],
        ..Frame::default()
    }
    .marshal()
}

pub(super) fn credentials() -> InstallationCredentials {
    InstallationCredentials::new("cli_app_x", AppSecret::new("secret-xyz"))
}

/// 装一台"一跑就结束"的会话（脚本耗尽即流结束）。
pub(super) struct Harness {
    pub(super) connector: Connector,
    pub(super) emitter: Arc<RecordingEmitter>,
    pub(super) log: Arc<Mutex<SocketLog>>,
    pub(super) dialer: Arc<ScriptedDialer>,
    pub(super) fetcher: Arc<FixedFetcher>,
}

impl Harness {
    pub(super) fn new(events: Vec<ChannelResult<WsEvent>>, knobs: SessionKnobs) -> Self {
        Self::with_ping_interval(events, knobs, Duration::from_secs(120))
    }

    pub(super) fn with_ping_interval(
        events: Vec<ChannelResult<WsEvent>>,
        knobs: SessionKnobs,
        ping_interval: Duration,
    ) -> Self {
        let log = Arc::new(Mutex::new(SocketLog::default()));
        let dialer = Arc::new(ScriptedDialer::with_socket(&log, events, true));
        let fetcher = Arc::new(FixedFetcher::new(ping_interval));
        let emitter = RecordingEmitter::new();
        let connector = Connector::new(
            Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
            Arc::clone(&dialer) as Arc<dyn WsDialer>,
            Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
        )
        .with_knobs(knobs);
        Self {
            connector,
            emitter,
            log,
            dialer,
            fetcher,
        }
    }

    pub(super) async fn run(&self) -> ChannelResult<SessionOutcome> {
        let (_signal, handle) = StopSignal::pair();
        self.connector
            .run_session(
                &credentials(),
                Arc::clone(&self.emitter) as Arc<dyn EventEmitter>,
                handle,
            )
            .await
    }

    pub(super) fn log(&self) -> std::sync::MutexGuard<'_, SocketLog> {
        self.log.lock().expect("log")
    }
}
