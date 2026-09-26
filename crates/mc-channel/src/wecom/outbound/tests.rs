//! `outbound/tests.rs` 的**替身与装备**（只替端口：库 / 发送者 / 中继 / 附件）。
//!
//! 用例按面分在两个子模块里：`pipeline`（判决链与记账）与 `attachments`（附件准入、收件箱、
//! 纯函数）。拆分的依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

mod attachments;
mod pipeline;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::channel::InstallationStatus;
use mc_core::id::Id;

use crate::wecom::metrics::Metrics;
use crate::wecom::relay::{NoticeRouter, RelayFrame};
use crate::wecom::stream_store::{Clock, OpenVerdict, StreamHandle, StreamSender, StreamStore};
use crate::wecom::strings::Locale;
use crate::wecom::types::CHANNEL_TYPE;
use crate::wecom::ws_frame::{aibot_chat_type_from_channel, CHAT_TYPE_SINGLE_INT};
use crate::wecom::ws_sender::{Deadline, SenderError};

use super::*;

// =====================================================================
// 替身（只替端口）
// =====================================================================

#[derive(Default)]
struct Counting {
    delivered: AtomicUsize,
    dropped: AtomicUsize,
    skipped: AtomicUsize,
    unconfirmed: AtomicUsize,
    attachment_delivered: AtomicUsize,
    attachment_dropped: AtomicUsize,
    attachment_shed: AtomicUsize,
    labels: Mutex<Vec<String>>,
}

impl Counting {
    fn labels(&self) -> Vec<String> {
        self.labels.lock().expect("lock").clone()
    }
}

impl Metrics for Counting {
    fn record_connect_failure(&self) {}
    fn record_auth_failure(&self) {}
    fn record_callback_queued(&self) {}
    fn record_callback_queue_blocked(&self) {}
    fn record_stream_finished(&self) {}
    fn record_stream_fell_back(&self) {}
    fn record_stream_opened(&self) {}
    fn record_outbound_delivered(&self) {
        self.delivered.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_dropped(&self, reason: &str) {
        self.labels.lock().expect("lock").push(reason.to_string());
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_skipped(&self, reason: &str) {
        self.labels.lock().expect("lock").push(reason.to_string());
        self.skipped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_delivered(&self) {
        self.attachment_delivered.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_dropped(&self, reason: &str) {
        self.labels.lock().expect("lock").push(reason.to_string());
        self.attachment_dropped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_delivery_shed(&self) {
        self.attachment_shed.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_unconfirmed(&self, reason: &str) {
        self.labels.lock().expect("lock").push(reason.to_string());
        self.unconfirmed.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_unconfirmed(&self, reason: &str) {
        self.labels.lock().expect("lock").push(reason.to_string());
    }
    fn record_relay_shed(&self, _kind: &str) {}
}

/// 一条记下来的发送。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    chat_id: String,
    chat_type: i32,
    text: String,
}

/// 一个会回答的活发送者。
#[derive(Default)]
struct RecordingSender {
    sent: Mutex<Vec<Sent>>,
    outcome: Mutex<Option<SenderError>>,
}

impl RecordingSender {
    fn failing(error: SenderError) -> Arc<Self> {
        let sender = Arc::new(Self::default());
        *sender.outcome.lock().expect("lock") = Some(error);
        sender
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().expect("lock").clone()
    }
}

#[async_trait]
impl LiveSender for RecordingSender {
    async fn send_text(
        &self,
        chat_id: &str,
        chat_type: i32,
        text: &str,
        _deadline: Deadline,
    ) -> Result<(), SenderError> {
        self.sent.lock().expect("lock").push(Sent {
            chat_id: chat_id.to_string(),
            chat_type,
            text: text.to_string(),
        });
        match self.outcome.lock().expect("lock").clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// 一个按脚本回答的收尾发送者。
#[derive(Default)]
struct RecordingStreamSender {
    script: Mutex<Vec<Option<SenderError>>>,
    /// 每次尝试都交出去的那个错误（`None` = 每次都成功）。
    always: Mutex<Option<SenderError>>,
    endings: AtomicUsize,
}

impl RecordingStreamSender {
    fn endings(&self) -> usize {
        self.endings.load(Ordering::SeqCst)
    }
}

impl RecordingStreamSender {
    /// **每一次**尝试都失败（收尾会按 `STREAM_CLOSE_RETRIES` 重试，所以"只失败一次"的替身
    /// 第二次会成功，把一条本该"结局未知"的路径走成成功 —— 那正是上游那条重试策略的形状）。
    fn failing(error: SenderError) -> Arc<Self> {
        let sender = Arc::new(Self::default());
        *sender.always.lock().expect("lock") = Some(error);
        sender
    }
}

#[async_trait]
impl StreamSender for RecordingStreamSender {
    async fn stream(
        &self,
        _handle: &StreamHandle,
        _text: &str,
        _finish: bool,
    ) -> Result<(), SenderError> {
        if let Some(error) = self.always.lock().expect("lock").clone() {
            return Err(error);
        }
        match self.script.lock().expect("lock").pop() {
            Some(Some(error)) => Err(error),
            Some(None) | None => Ok(()),
        }
    }

    async fn stream_rewrite(
        &self,
        handle: &StreamHandle,
        text: &str,
        finish: bool,
    ) -> Result<(), SenderError> {
        self.stream(handle, text, finish).await
    }

    fn record_ending(&self, _error: Option<&SenderError>) {
        self.endings.fetch_add(1, Ordering::SeqCst);
    }
}

/// 一个发送者注册表替身（可选带一条流面）。
#[derive(Default)]
struct FakeSenders {
    by_installation: Mutex<HashMap<uuid::Uuid, Arc<dyn LiveSender>>>,
    stream: Option<Arc<dyn StreamSender>>,
}

impl FakeSenders {
    fn with(installation: Id, sender: Arc<dyn LiveSender>) -> Self {
        let senders = Self::default();
        senders
            .by_installation
            .lock()
            .expect("lock")
            .insert(installation.0, sender);
        senders
    }

    fn with_stream(mut self, stream: Arc<dyn StreamSender>) -> Self {
        self.stream = Some(stream);
        self
    }
}

impl SenderLookup for FakeSenders {
    fn get(&self, installation_id: Id) -> Option<Arc<dyn LiveSender>> {
        self.by_installation
            .lock()
            .expect("lock")
            .get(&installation_id.0)
            .cloned()
    }

    fn stream_sender(&self) -> Option<&dyn StreamSender> {
        // 借用活不过 `&self` ⇒ 泄露一份克隆（用例进程里数量有限，故意）。
        self.stream
            .as_ref()
            .map(|stream| &**Box::leak(Box::new(Arc::clone(stream))))
    }
}

/// 一个按脚本回答的库端口。
#[derive(Default)]
struct FakeQueries {
    delivery: Mutex<Option<TaskDelivery>>,
    task: Mutex<Option<AgentTask>>,
    ingested: bool,
    installation: Mutex<Option<InstallationRecord>>,
    binding: Mutex<Option<MemberBinding>>,
    slug: Option<String>,
}

impl FakeQueries {
    fn with_task(task: AgentTask) -> Arc<Self> {
        let queries = Arc::new(Self {
            ingested: true,
            ..Self::default()
        });
        *queries.task.lock().expect("lock") = Some(task);
        queries
    }

    fn with_delivery(
        self: &Arc<Self>,
        delivery: TaskDelivery,
        installation: InstallationRecord,
    ) -> Arc<Self> {
        *self.delivery.lock().expect("lock") = Some(delivery);
        *self.installation.lock().expect("lock") = Some(installation);
        Arc::clone(self)
    }
}

#[async_trait]
impl OutboundQueries for FakeQueries {
    async fn get_task_delivery(&self, _task_id: Id) -> Result<Option<TaskDelivery>, String> {
        Ok(self.delivery.lock().expect("lock").clone())
    }
    async fn get_agent_task(&self, _task_id: Id) -> Result<Option<AgentTask>, String> {
        Ok(self.task.lock().expect("lock").clone())
    }
    async fn task_has_channel_ingested_messages(&self, _task_id: Id) -> Result<bool, String> {
        Ok(self.ingested)
    }
    async fn get_installation(
        &self,
        _installation_id: Id,
    ) -> Result<Option<InstallationRecord>, String> {
        Ok(*self.installation.lock().expect("lock"))
    }
    async fn find_binding_for_member(
        &self,
        _workspace_id: Id,
        _multica_user_id: Id,
    ) -> Result<Option<MemberBinding>, String> {
        Ok(self.binding.lock().expect("lock").clone())
    }
    async fn workspace_slug(&self, _workspace_id: Id) -> Result<Option<String>, String> {
        Ok(self.slug.clone())
    }
}

/// 一个记下路由的中继。
#[derive(Default)]
struct FakeRelay {
    published: Mutex<Vec<(RelayFrame, String)>>,
    accept: bool,
}

impl NoticeRouter for FakeRelay {
    fn publish(&self, frame: &RelayFrame, event_id: &str) -> bool {
        self.published
            .lock()
            .expect("lock")
            .push((frame.clone(), event_id.to_string()));
        self.accept
    }
}

/// 一个记下附件投递的端口。
///
/// 🔴 写集勘误（`docs/32` §35 的 D12）：这个替身跟着 `AttachmentDelivery` 的新形状走 ——
/// `async`，而且多记两个 id（投递是**为哪条消息**做的）。
#[derive(Default)]
struct FakeAttachments {
    delivered: Mutex<Vec<(String, String, AttachmentTarget, bool)>>,
}

#[async_trait::async_trait]
impl AttachmentDelivery for FakeAttachments {
    async fn deliver(
        &self,
        message_id: &str,
        workspace_id: &str,
        target: AttachmentTarget,
        carries_the_reply: bool,
    ) {
        self.delivered.lock().expect("lock").push((
            message_id.to_owned(),
            workspace_id.to_owned(),
            target,
            carries_the_reply,
        ));
    }
}

// =====================================================================
// 装备
// =====================================================================

fn session() -> Id {
    Id::new()
}

fn chat_done(task: Id, session_id: Id, content: &str) -> ChatDone {
    ChatDone {
        chat_session_id: session_id.0.to_string(),
        envelope_task_id: task.0.to_string(),
        payload_task_id: task.0.to_string(),
        content: content.to_string(),
        message_id: "msg-1".to_string(),
        workspace_id: "ws-1".to_string(),
        event_type: "chat:done".to_string(),
    }
}

fn task(task_id: Id) -> AgentTask {
    AgentTask {
        id: task_id,
        chat_input_task_id: Some(task_id),
        chat_session_id: None,
        batch_has_channel_ingested_messages: true,
    }
}

fn delivery(installation: Id) -> TaskDelivery {
    TaskDelivery {
        task_id: Id::new(),
        binding_id: Id::new(),
        installation_id: installation,
        channel_type: CHANNEL_TYPE.to_string(),
        channel_chat_id: "chat-1".to_string(),
        chat_type: "p2p".to_string(),
        channel_message_id: Some("msg-1".to_string()),
        channel_thread_id: None,
        route_revision: 1,
        config: serde_json::json!({}),
    }
}

fn active(installation: Id) -> InstallationRecord {
    InstallationRecord {
        id: installation,
        status: InstallationStatus::Active,
    }
}

/// 一个有一轮**开着气泡**的存储（`open` + `bind_next` 就是 ingest 干的那两件事）。
fn store_with_bubble(
    stream_id: &str,
    installation: Id,
    task_id: Id,
    locale: Locale,
) -> (Arc<StreamStore>, Id) {
    let store = Arc::new(
        StreamStore::new()
            .with_close_retry_delay(Duration::ZERO)
            .with_clock(Arc::new(Instant::now) as Clock),
    );
    let session = session();
    let handle = StreamHandle {
        req_id: "req-1".to_string(),
        stream_id: stream_id.to_string(),
        installation_id: Some(installation),
        chat_id: "chat-1".to_string(),
        chat_type: CHAT_TYPE_SINGLE_INT,
        locale,
        created_at: Instant::now(),
    };
    let (_, verdict) = store.open(session, handle);
    assert_eq!(verdict, OpenVerdict::Opened);
    store.bind_next(session, &task_id.0.to_string());
    (store, session)
}

// =====================================================================
// 判决链
// =====================================================================
