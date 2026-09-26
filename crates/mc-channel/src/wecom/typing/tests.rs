//! `typing` 的用例：上游 `typing_indicator_test.go` 的等价面。
//!
//! # 替身纪律
//!
//! **轮次表是真的**（[`StreamStore`]），发送面 / task 面 / 投递面 / 语言面 / 中继面都是注入的替身
//! —— 但那五个都是**端口**，而"这一个气泡到底开没开、开在哪一轮、收在哪一轮"这一半全由真代码决定。
//! 与 `stream_store/tests.rs` 的脚本化收尾器、`dingtalk/ack/tests.rs` 的替身同款。
//!
//! 时钟注入不了（`StreamStore::with_max_age` 是要用的那一格），但本文件**不需要**它：所有断言都在
//! "开/绑/取/封"这条同步链上，唯一涉及时间的是 [`crate::wecom::stream_store::PENDING_MAX_AGE`]，
//! 而它按 30s 计。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use mc_core::channel::installation::InstallationStatus;
use mc_core::channel::message::{ChatType, InboundMessage, Source};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;

use super::ports::{
    failure_text, origin_of, task_failed_content, LanguageLookup, OriginVerdict, TaskEvent,
    TaskQueries, TASK_FAILED_PREFIX,
};
use super::*;
use crate::wecom::outbound::{
    AgentTask, DeliveryLookup, InstallationRecord, Outbound, OutboundQueries, TaskDelivery,
};
use crate::wecom::relay::{RelayFrame, RelayKind, SEAL_REASON_CANCELLED};
use crate::wecom::stream_store::{rounds_task_ids, sealed_streams, RootResolver, StreamSender};
use crate::wecom::strings::{copy_for, Locale};
use crate::wecom::wecom_channel::inbound::WeComInboundMessage;
use crate::wecom::ws_frame::{
    aibot_chat_type_from_channel, CHAT_TYPE_GROUP_INT, STREAM_THINKING_PLACEHOLDER,
};
use crate::wecom::ws_sender::SenderError;

// =====================================================================
// 替身：发送面
// =====================================================================

#[derive(Default)]
struct FakeSenders {
    /// `(handle, text, finish)` —— 记**整个句柄**，于是 `req_id` / `stream_id` / `locale` /
    /// `created_at` 都能在断言里读到，而不必给存储加一个诊断出口。
    streams: Mutex<Vec<(StreamHandle, String, bool)>>,
    /// `(chat_id, chat_type, text)`。
    texts: Mutex<Vec<(String, i32, String)>>,
    opened: AtomicUsize,
    endings: AtomicUsize,
    stream_error: Mutex<Option<SenderError>>,
    text_error: Mutex<Option<SenderError>>,
    socket: AtomicBool,
}

impl FakeSenders {
    fn with_socket() -> Self {
        let senders = Self::default();
        senders.socket.store(true, Ordering::SeqCst);
        senders
    }

    fn fail_streams_with(&self, error: SenderError) {
        match self.stream_error.lock() {
            Ok(mut guard) => *guard = Some(error),
            Err(poisoned) => *poisoned.into_inner() = Some(error),
        }
    }

    fn fail_texts_with(&self, error: SenderError) {
        match self.text_error.lock() {
            Ok(mut guard) => *guard = Some(error),
            Err(poisoned) => *poisoned.into_inner() = Some(error),
        }
    }

    fn stream_frames(&self) -> Vec<(StreamHandle, String, bool)> {
        match self.streams.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn plain_texts(&self) -> Vec<(String, i32, String)> {
        match self.texts.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    fn endings(&self) -> usize {
        self.endings.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl StreamSender for FakeSenders {
    async fn stream(
        &self,
        handle: &StreamHandle,
        text: &str,
        finish: bool,
    ) -> Result<(), SenderError> {
        if let Some(error) = self.stream_error.lock().expect("lock").take() {
            return Err(error);
        }
        self.streams
            .lock()
            .expect("lock")
            .push((handle.clone(), text.to_string(), finish));
        Ok(())
    }

    async fn stream_rewrite(
        &self,
        handle: &StreamHandle,
        text: &str,
        finish: bool,
    ) -> Result<(), SenderError> {
        self.stream(&handle.clone(), text, finish).await
    }

    fn record_ending(&self, _error: Option<&SenderError>) {
        self.endings.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl RoundSenders for FakeSenders {
    async fn send_text(
        &self,
        _installation_id: Id,
        chat_id: &str,
        chat_type: i32,
        content: &str,
        _deadline: crate::wecom::ws_sender::Deadline,
    ) -> Result<(), SenderError> {
        if let Some(error) = self.text_error.lock().expect("lock").take() {
            return Err(error);
        }
        self.texts.lock().expect("lock").push((
            chat_id.to_string(),
            chat_type,
            content.to_string(),
        ));
        Ok(())
    }

    fn record_opened(&self) {
        self.opened.fetch_add(1, Ordering::SeqCst);
    }

    fn has_socket(&self, _installation_id: Id) -> bool {
        self.socket.load(Ordering::SeqCst)
    }
}

// =====================================================================
// 替身：task 面 / 投递面 / 语言面 / 中继面
// =====================================================================

#[derive(Default)]
struct FakeTasks {
    task: Mutex<Option<AgentTask>>,
    ingested: AtomicBool,
    fail: AtomicBool,
    /// 批次的 `channel_ingested` 那一次读被问了几次（钉"NULL 所有者 ⇒ 不花第二次读"这条短路）。
    ingested_reads: AtomicUsize,
}

impl FakeTasks {
    fn with_task(task: AgentTask, ingested: bool) -> Self {
        let fake = Self::default();
        *fake.task.lock().expect("lock") = Some(task);
        fake.ingested.store(ingested, Ordering::SeqCst);
        fake
    }
}

#[async_trait]
impl TaskQueries for FakeTasks {
    async fn get_agent_task(&self, _task_id: Id) -> Result<Option<AgentTask>, String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("pool is busy".to_string());
        }
        Ok(self.task.lock().expect("lock").clone())
    }

    async fn task_has_channel_ingested_messages(&self, _task_id: Id) -> Result<bool, String> {
        self.ingested_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err("pool is busy".to_string());
        }
        Ok(self.ingested.load(Ordering::SeqCst))
    }
}

#[derive(Default)]
struct FakeDeliveries {
    row: Mutex<Option<TaskDelivery>>,
    installation: Mutex<Option<InstallationRecord>>,
}

impl FakeDeliveries {
    fn with_live_row(installation: Id, chat_id: &str) -> Self {
        let fake = Self::default();
        *fake.row.lock().expect("lock") = Some(delivery(installation, chat_id, "wecom"));
        *fake.installation.lock().expect("lock") = Some(InstallationRecord {
            id: installation,
            status: InstallationStatus::Active,
        });
        fake
    }

    fn with_revoked_row(installation: Id) -> Self {
        let fake = Self::default();
        *fake.row.lock().expect("lock") = Some(delivery(installation, "room", "wecom"));
        *fake.installation.lock().expect("lock") = Some(InstallationRecord {
            id: installation,
            status: InstallationStatus::Revoked,
        });
        fake
    }

    fn with_foreign_row() -> Self {
        let fake = Self::default();
        *fake.row.lock().expect("lock") = Some(delivery(Id::new(), "room", "slack"));
        fake
    }
}

#[async_trait]
impl DeliveryLookup for FakeDeliveries {
    async fn task_delivery(&self, _task_id: Id) -> Result<Option<TaskDelivery>, String> {
        Ok(self.row.lock().expect("lock").clone())
    }

    async fn installation_record(
        &self,
        _installation_id: Id,
    ) -> Result<Option<InstallationRecord>, String> {
        Ok(*self.installation.lock().expect("lock"))
    }
}

/// 语言面：只对 1:1 生效的那个"个人档案"（群聊一律部署语言，与真实现同款）。
struct DirectMessageEnglish;

impl LanguageLookup for DirectMessageEnglish {
    fn locale_for(&self, _installation_id: Id, chat_type: i32, _sender_id: &str) -> Locale {
        if chat_type == aibot_chat_type_from_channel(ChatType::P2p) {
            Locale::En
        } else {
            Locale::ZhHans
        }
    }
}

#[derive(Default)]
struct FakeRouter {
    published: Mutex<Vec<(RelayKind, String, String, String)>>,
}

impl FakeRouter {
    fn frames(&self) -> Vec<(RelayKind, String, String, String)> {
        match self.published.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl NoticeRouter for FakeRouter {
    fn publish(&self, frame: &RelayFrame, event_id: &str) -> bool {
        self.published.lock().expect("lock").push((
            frame.kind,
            frame.seal_reason.clone(),
            frame.task_id.clone(),
            event_id.to_string(),
        ));
        true
    }
}

// =====================================================================
// 夹具
// =====================================================================

/// 一个**真的能解成 `Id`** 的 task id 字面量。
///
/// 夹具必须真的是 UUID：`parse_task_id` 就是 UUID 解析，而 `origin_of` / `session_for` /
/// `address_for_task` 三个入口都以它开头 —— 用 `TASK_ID` 那样的字符串会让整条门**静默地**
/// 走到 `Unknown`（那正是这条注释存在的理由：第一版就是用 `TASK_ID` 写的，九个用例一起红）。
const TASK_ID: &str = "11111111-1111-4111-8111-111111111111";

/// 第二条 task id（`.with_issue(...)` 那一类用）。
const TASK_ISSUE: &str = "33333333-3333-4333-8333-333333333333";

fn delivery(installation: Id, chat_id: &str, channel: &str) -> TaskDelivery {
    TaskDelivery {
        task_id: Id::new(),
        binding_id: Id::new(),
        installation_id: installation,
        channel_type: channel.to_string(),
        channel_chat_id: chat_id.to_string(),
        chat_type: "group".to_string(),
        channel_message_id: Some("msg-1".to_string()),
        channel_thread_id: None,
        route_revision: 1,
        config: serde_json::json!({}),
    }
}

fn task_row(id: Id, root: Option<Id>, session: Option<Id>) -> AgentTask {
    AgentTask {
        id,
        chat_input_task_id: root,
        chat_session_id: session,
        batch_has_channel_ingested_messages: false,
    }
}

/// 一条**群聊**入站消息，它的 `raw` 带着回调的 `req_id` 与 chat id（`on_ingested` 读它）。
fn inbound(req_id: &str, chat_id: &str) -> InboundMessage {
    let raw = WeComInboundMessage {
        bot_id: "BOTID".to_string(),
        msg_id: "MSG-1".to_string(),
        msg_type: "text".to_string(),
        chat_type: "group".to_string(),
        chat_id: chat_id.to_string(),
        sender_user_id: "SENDER".to_string(),
        content: "hello".to_string(),
        req_id: req_id.to_string(),
        media: Vec::new(),
    };
    InboundMessage {
        event_id: "evt-1".to_string(),
        message_id: "MSG-1".to_string(),
        source: Source {
            channel_type: ChannelKind::WeCom,
            chat_id: chat_id.to_string(),
            chat_type: ChatType::Group,
            sender_id: "SENDER".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: mc_core::channel::message::MessageKind::Text,
        text: "hello".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: raw.to_raw_value(),
    }
}

fn installation(id: Id) -> ResolvedInstallation {
    ResolvedInstallation {
        id,
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        active: true,
        kind: ChannelKind::WeCom,
        platform: None,
    }
}

/// 一台挂齐全的指示器 + 它的替身们。
struct Harness {
    indicator: TypingIndicator,
    streams: Arc<StreamStore>,
    senders: Arc<FakeSenders>,
    tasks: Arc<FakeTasks>,
    router: Arc<FakeRouter>,
    installation_id: Id,
    session: Id,
}

impl Harness {
    fn builder() -> HarnessBuilder {
        HarnessBuilder::default()
    }

    /// 开一轮并把 `task_id` 绑上去（真实走过的两条路：`open` + `bind_next`）。
    ///
    /// 交回来的句柄取自**发送面替身记下的那一帧**（它记的是整个 [`StreamHandle`]）⇒ 断言可以逐字
    /// 读到 `req_id` / `chat_id` / `locale` / `created_at`，而不必给存储加一个诊断出口。
    async fn open_round(&self, task_id: &str) -> StreamHandle {
        self.indicator
            .on_ingested_now(
                &installation(self.installation_id),
                &inbound("req-1", "room"),
                self.session,
            )
            .await;
        let handle = self
            .senders
            .stream_frames()
            .pop()
            .expect("开场之后必须有一轮")
            .0;
        if !task_id.is_empty() {
            self.indicator
                .handle_task_queued(&TaskEvent::queued(task_id, Some(self.session.to_string())));
        }
        handle
    }
}

#[derive(Default)]
struct HarnessBuilder {
    tasks: Option<Arc<FakeTasks>>,
    deliveries: Option<Arc<FakeDeliveries>>,
    languages: Option<Arc<dyn LanguageLookup>>,
    router: Option<Arc<FakeRouter>>,
    roots: Option<Arc<dyn RootResolver>>,
    socket: bool,
}

impl HarnessBuilder {
    fn tasks(mut self, tasks: FakeTasks) -> Self {
        self.tasks = Some(Arc::new(tasks));
        self
    }

    fn deliveries(mut self, deliveries: FakeDeliveries) -> Self {
        self.deliveries = Some(Arc::new(deliveries));
        self
    }

    fn languages(mut self, languages: Arc<dyn LanguageLookup>) -> Self {
        self.languages = Some(languages);
        self
    }

    fn router(mut self, router: Arc<FakeRouter>) -> Self {
        self.router = Some(router);
        self
    }

    fn roots(mut self, roots: Arc<dyn RootResolver>) -> Self {
        self.roots = Some(roots);
        self
    }

    fn socket(mut self, has_socket: bool) -> Self {
        self.socket = has_socket;
        self
    }

    fn build(self) -> Harness {
        let streams = Arc::new(StreamStore::new());
        let senders = Arc::new(if self.socket {
            FakeSenders::with_socket()
        } else {
            FakeSenders::default()
        });
        let tasks = self.tasks.unwrap_or_default();
        let deliveries = self.deliveries.unwrap_or_default();
        let router = self.router;
        let mut indicator = TypingIndicator::new()
            .with_streams(Arc::clone(&streams))
            .with_senders(Arc::clone(&senders) as Arc<dyn RoundSenders>)
            .with_tasks(Arc::clone(&tasks) as Arc<dyn TaskQueries>)
            .with_deliveries(Arc::clone(&deliveries) as Arc<dyn DeliveryLookup>);
        if let Some(languages) = self.languages {
            indicator = indicator.with_languages(languages);
        }
        if let Some(router) = router.as_ref() {
            indicator = indicator.with_relay(Arc::clone(router) as Arc<dyn NoticeRouter>);
        }
        if let Some(roots) = self.roots {
            indicator = indicator.with_roots(roots);
        }
        Harness {
            indicator,
            streams,
            senders,
            tasks,
            router: router.unwrap_or_default(),
            installation_id: Id::new(),
            session: Id::new(),
        }
    }
}

// =====================================================================
// 开场
// =====================================================================

// =====================================================================
// 用例（按门类拆到子模块：门 ⑩ 的 800 行硬限）
// =====================================================================

mod addressing;
mod endings;
mod opening;
mod seam;
