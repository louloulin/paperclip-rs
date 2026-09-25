//! adapter 装配面与连接生命周期的用例（`mod.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 三组：
//!
//! 1. **注册 / 工厂**：失败关闭的凭据面、四条配置校验、以及"队列按 `AppKey` 跨装配复用"；
//! 2. **连接生命周期**：正常收尾**不**收口队列；被 supervisor 丢弃（模拟）**要**收口 ——
//!    这条判决就是上游 `stopDispatch` 的等价物（见 `mod.rs` 的模块文档）；
//! 3. **入站闭环**：帧 ⇒ 队列 ⇒ 归一化 ⇒ `SharedInboundHandler`，以及凭据的脱敏面。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;

use super::{
    factory, factory_with_slots, kind, origin_type, register, BotNameSource, Decrypter,
    DingTalkChannel, DingTalkDeps, NoBotName, StreamInstallConfig, KIND,
};
use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult, Factory};
use crate::dingtalk::dispatch::{
    DispatchLimits, DispatchSlotRegistry, Dispatcher, InboundJob, JobHandler,
};
use crate::dingtalk::inbound::{BotCallbackData, ORIGIN_DINGTALK_CHAT, TYPE_DINGTALK};
use crate::dingtalk::stream::{
    AppSecret, CallbackSink, ConnectionOpener, StreamKnobs, WsConnection, WsDialer, WsEvent,
    BOT_MESSAGE_TOPIC, FRAME_TYPE_CALLBACK, FRAME_TYPE_SYSTEM, SYSTEM_TOPIC_DISCONNECT,
};
use crate::message::{InboundHandler, SharedInboundHandler};
use crate::registry::Registry;

// =====================================================================
// 替身
// =====================================================================

#[derive(Debug, Default)]
struct FakeLog {
    written: Vec<String>,
    closed: bool,
}

/// 一条脚本事件（`deadline` 只定一次 ⇒ 被取消的读不会把它往后推，见 `stream/tests.rs`）。
#[derive(Debug)]
struct ScriptEntry {
    delay: Duration,
    deadline: Option<tokio::time::Instant>,
    event: Option<ChannelResult<WsEvent>>,
}

type Script = Arc<Mutex<VecDeque<ScriptEntry>>>;

fn script_of(events: Vec<(Duration, Option<ChannelResult<WsEvent>>)>) -> Script {
    Arc::new(Mutex::new(
        events
            .into_iter()
            .map(|(delay, event)| ScriptEntry {
                delay,
                deadline: None,
                event,
            })
            .collect(),
    ))
}

struct ScriptConnection {
    script: Script,
    log: Arc<Mutex<FakeLog>>,
}

#[async_trait]
impl WsConnection for ScriptConnection {
    async fn next_event(&mut self) -> Option<ChannelResult<WsEvent>> {
        let deadline = {
            let mut script = self.script.lock().expect("lock");
            let front = script.front_mut()?;
            *front
                .deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + front.delay)
        };
        tokio::time::sleep_until(deadline).await;
        self.script
            .lock()
            .expect("lock")
            .pop_front()
            .and_then(|entry| entry.event)
    }

    async fn send_text(&mut self, value: &str) -> ChannelResult<()> {
        self.log
            .lock()
            .expect("lock")
            .written
            .push(value.to_string());
        Ok(())
    }

    async fn send_ping(&mut self) -> ChannelResult<()> {
        Ok(())
    }

    async fn close(&mut self) {
        self.log.lock().expect("lock").closed = true;
    }
}

struct FakeDialer {
    script: Script,
    log: Arc<Mutex<FakeLog>>,
}

#[async_trait]
impl WsDialer for FakeDialer {
    async fn dial(&self, _dial_url: &str) -> ChannelResult<Box<dyn WsConnection>> {
        Ok(Box::new(ScriptConnection {
            script: Arc::clone(&self.script),
            log: Arc::clone(&self.log),
        }))
    }
}

struct FakeOpener;

#[async_trait]
impl ConnectionOpener for FakeOpener {
    async fn open(&self, _app_key: &str, _app_secret: &str) -> ChannelResult<String> {
        Ok("wss://gateway.example.com/stream?ticket=ticket-1".to_string())
    }
}

#[derive(Default)]
struct RecordingInbound {
    seen: Mutex<Vec<InboundMessage>>,
    calls: AtomicUsize,
}

#[async_trait]
impl InboundHandler for RecordingInbound {
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().expect("lock").push(message);
        Ok(())
    }
}

fn callback_frame(payload: &serde_json::Value) -> String {
    serde_json::json!({
        "type": FRAME_TYPE_CALLBACK,
        "headers": {"topic": BOT_MESSAGE_TOPIC, "messageId": "frame-1"},
        "data": payload.to_string(),
    })
    .to_string()
}

fn disconnect_frame() -> String {
    serde_json::json!({
        "type": FRAME_TYPE_SYSTEM,
        "headers": {"topic": SYSTEM_TOPIC_DISCONNECT, "messageId": "bye"},
        "data": "",
    })
    .to_string()
}

fn text(value: &str) -> (Duration, Option<ChannelResult<WsEvent>>) {
    (Duration::ZERO, Some(Ok(WsEvent::Text(value.to_string()))))
}

/// 造一条 channel：脚本化的 socket + 记录型 handler（+ 可注入的 bot 名来源）。
fn channel(
    handler: Option<SharedInboundHandler>,
    events: Vec<(Duration, Option<ChannelResult<WsEvent>>)>,
) -> (Arc<DingTalkChannel>, Arc<Mutex<FakeLog>>) {
    let script = script_of(events);
    let log = Arc::new(Mutex::new(FakeLog::default()));
    let dispatcher = Arc::new(Dispatcher::with_limits(
        Arc::new(JobHandlerForTest),
        DispatchLimits::default(),
    ));
    let slots = Arc::new(DispatchSlotRegistry::new());
    // 真正的作业体由工厂装配；这里的用例直接驱动 `CallbackJobHandler` 的那条路径时走工厂，
    // 所以本地构造的 dispatcher 只服务"生命周期判决"那一组用例。
    let channel = DingTalkChannel::new(
        "app-key",
        AppSecret::new("app-secret"),
        handler,
        Arc::new(FakeOpener),
        Arc::new(FakeDialer {
            script,
            log: Arc::clone(&log),
        }),
        dispatcher,
        slots,
    )
    .with_knobs(StreamKnobs {
        ping_interval: Duration::from_secs(3600),
        read_deadline: Duration::from_secs(5),
        write_timeout: Duration::from_secs(1),
    });
    (Arc::new(channel), log)
}

struct JobHandlerForTest;

#[async_trait]
impl JobHandler for JobHandlerForTest {
    async fn handle(&self, _job: InboundJob) {}
}

fn plain_config(app_id: &str) -> ChannelConfig {
    ChannelConfig {
        kind: TYPE_DINGTALK,
        raw: serde_json::json!({"app_id": app_id, "app_secret": "plain-secret"}),
        installation_id: None,
        handler: None,
    }
}

// =====================================================================
// 注册 / 工厂
// =====================================================================

/// 注册面：`register` 让品台进入注册表；`kind` / `origin_type` 是逐字常量。
#[test]
fn register_puts_the_platform_into_the_registry() {
    let registry = Registry::new();
    let deps = DingTalkDeps::default();
    register(
        &registry,
        &crate::engine::ChannelDeps::new(
            Arc::new(router()),
            Arc::new(NoInstallations),
            Arc::new(NoLeases),
        ),
    );
    assert_eq!(registry.kinds(), vec![TYPE_DINGTALK]);
    assert_eq!(kind(), TYPE_DINGTALK);
    assert_eq!(KIND, TYPE_DINGTALK);
    assert_eq!(origin_type(), ORIGIN_DINGTALK_CHAT);
    assert_eq!(origin_type(), "dingtalk_chat");
    // 失败关闭的默认值：解密器拒绝、没有 bot 名来源。
    assert_eq!(deps.decrypt.label(), "fail-closed");
    assert!(matches!(
        deps.decrypt.decrypt("CIPHER"),
        Err(ChannelError::InvalidConfig { .. })
    ));
    let _ = NoBotName.bot_name("app-key", "chat");
    assert!(NoBotName.bot_name("app-key", "chat").is_none());
}

/// 工厂的四条配置校验（上游 `newDingTalkFactory` 的拒装配面）。
#[test]
fn the_factory_refuses_half_built_configurations() {
    let factory = factory(&DingTalkDeps::default());
    for raw in [
        serde_json::json!("not-an-object"),
        serde_json::json!({"app_secret": "s"}),
        serde_json::json!({"app_id": "k"}),
        serde_json::json!({"app_id": "k", "app_secret_encrypted": "CIPHER"}),
    ] {
        let config = ChannelConfig {
            kind: TYPE_DINGTALK,
            raw,
            installation_id: None,
            handler: None,
        };
        let error = factory(config).err().expect("必须拒装配");
        assert!(matches!(error, ChannelError::InvalidConfig { .. }));
        assert!(
            !error.to_string().contains("CIPHER"),
            "错误回显了密文：{error}"
        );
    }

    // 明文 AppSecret（本地 / 用例形态）⇒ 装配成功。
    let channel = factory(plain_config("app-key")).expect("合法配置");
    assert_eq!(channel.kind(), TYPE_DINGTALK);
}

/// 接线好的解密器：密文列被解成明文再装配。
#[test]
fn a_wired_decrypter_opens_the_encrypted_configuration() {
    let decrypter = Decrypter::new(
        "test-secretbox",
        Arc::new(|ciphertext: &str| Ok(format!("{ciphertext}-plaintext"))),
    );
    assert_eq!(decrypter.label(), "test-secretbox");
    let deps = DingTalkDeps::default().with_decrypter(decrypter.clone());
    let built = (*factory(&deps))(ChannelConfig {
        kind: TYPE_DINGTALK,
        raw: serde_json::json!({"app_id": "k", "app_secret_encrypted": "CIPHER"}),
        installation_id: None,
        handler: None,
    })
    .expect("解密后装配成功");
    assert_eq!(built.kind(), TYPE_DINGTALK);
    // 解密器自身的 `Debug` 不含任何密文 / 明文。
    let rendered = format!("{decrypter:?}");
    assert!(!rendered.contains("CIPHER"), "{rendered}");
    assert!(rendered.contains("test-secretbox"));
}

/// 队列按 `AppKey` 跨装配复用（上游 `dispatchSlot`：重连不让两轮并发）。
#[tokio::test]
async fn the_factory_reuses_the_dispatch_queue_by_app_key() {
    let deps = DingTalkDeps::default();
    let slots = Arc::new(DispatchSlotRegistry::with_limits(DispatchLimits::default()));
    let built: Factory = factory_with_slots(&deps, Arc::clone(&slots));
    let first = (*built)(plain_config("app-key")).expect("第一次装配");
    let second = (*built)(plain_config("app-key")).expect("第二次装配（同一安装）");
    // 两个 channel 对象不同，但队列是**同一条**（`Arc` 相同）。
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(slots.created(), 1);
    let _ = (*built)(plain_config("other-key")).expect("另一个 AppKey 装配成功");
    assert_eq!(slots.created(), 2);
}

/// `StreamInstallConfig` 的 `Debug` 与 `robot_code` 的退路。
#[test]
fn the_install_config_debug_redacts_both_secret_columns() {
    let config: StreamInstallConfig = serde_json::from_value(serde_json::json!({
        "app_id": "app-key",
        "robot_code": "",
        "app_secret": "PLAIN",
        "app_secret_encrypted": "CIPHER",
        "future_field": 1,
    }))
    .expect("未知字段被忽略");
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("PLAIN"), "{rendered}");
    assert!(!rendered.contains("CIPHER"), "{rendered}");
    assert!(rendered.contains("<redacted>"));
    assert_eq!(config.robot_code_or_app_id(), "app-key");
    let explicit: StreamInstallConfig = serde_json::from_value(serde_json::json!({
        "app_id": "app-key",
        "robot_code": "robot-1",
    }))
    .expect("合法配置");
    assert_eq!(explicit.robot_code_or_app_id(), "robot-1");
    assert!(format!("{explicit:?}").contains("<empty>"));
}

// =====================================================================
// 连接生命周期
// =====================================================================

/// `connect` 的失败关闭：没有 handler / 没有 `AppSecret` 都拒（不是"连上一个发不出消息的东西"）。
#[tokio::test]
async fn connect_is_fail_closed_without_a_handler_or_a_secret() {
    let (no_handler, _log) = channel(None, vec![]);
    assert!(matches!(
        no_handler.connect().await,
        Err(ChannelError::InvalidConfig { .. })
    ));

    let dispatcher = Arc::new(Dispatcher::with_limits(
        Arc::new(JobHandlerForTest),
        DispatchLimits::default(),
    ));
    let empty_secret = DingTalkChannel::new(
        "app-key",
        AppSecret::default(),
        None,
        Arc::new(FakeOpener),
        Arc::new(FakeDialer {
            script: script_of(Vec::new()),
            log: Arc::new(Mutex::new(FakeLog::default())),
        }),
        dispatcher,
        Arc::new(DispatchSlotRegistry::new()),
    );
    assert!(matches!(
        empty_secret.connect().await,
        Err(ChannelError::InvalidConfig { .. })
    ));
}

/// 出站位是**失败关闭**的（实现归 M7-8），且自报的能力位照上游声明。
#[tokio::test]
async fn send_is_fail_closed_and_capabilities_match_upstream() {
    let (channel, _log) = channel(None, vec![]);
    let error = channel
        .send(mc_core::channel::message::OutboundMessage {
            chat_id: "chat".to_string(),
            text: "hi".to_string(),
            thread_id: String::new(),
            reply_to: String::new(),
        })
        .await
        .expect_err("出站未接线");
    assert!(error.to_string().contains("M7-8"), "{error}");
    assert_eq!(
        channel.capabilities(),
        Capability::TEXT.union(Capability::ATTACHMENT)
    );
    // 凭据面：channel 的 `Debug` 不回显 AppSecret。
    let rendered = format!("{channel:?}");
    assert!(!rendered.contains("app-secret"), "{rendered}");
}

/// 网关要求重连（自己干净返回）⇒ **不**收口队列（跨重连的会话顺序优先）。
#[tokio::test]
async fn a_gateway_redial_keeps_the_queue_alive() {
    let (channel, log) = channel(
        Some(Arc::new(RecordingInbound::default())),
        vec![text(&disconnect_frame()), (Duration::ZERO, None)],
    );
    channel.connect().await.expect("干净返回");
    assert!(!channel.relinquished(), "自己返回 ≠ 生命周期停机");
    assert!(log.lock().expect("lock").closed, "收尾要关连接");
    assert!(!channel.dispatcher().is_closed(), "队列必须活着");
    channel.disconnect().await.expect("no-op");
    assert!(!channel.dispatcher().is_closed());
}

/// 被 supervisor 丢弃（`connect` 的 future 在会话中途被 drop）⇒ 判为生命周期停机 ⇒
/// `disconnect` 收口队列并摘掉槽。
#[tokio::test]
async fn a_dropped_connect_future_is_a_lifecycle_stop() {
    let dispatcher = Arc::new(Dispatcher::with_limits(
        Arc::new(JobHandlerForTest),
        DispatchLimits::default(),
    ));
    let slots = Arc::new(DispatchSlotRegistry::new());
    let _ = slots.acquire("app-key", Arc::new(JobHandlerForTest));
    let script = script_of(vec![(Duration::from_secs(30), Some(Ok(WsEvent::Pong)))]);
    let channel = Arc::new(DingTalkChannel::new(
        "app-key",
        AppSecret::new("app-secret"),
        Some(Arc::new(RecordingInbound::default())),
        Arc::new(FakeOpener),
        Arc::new(FakeDialer {
            script,
            log: Arc::new(Mutex::new(FakeLog::default())),
        }),
        Arc::clone(&dispatcher),
        Arc::clone(&slots),
    ));
    let running = Arc::clone(&channel);
    let task = tokio::spawn(async move { running.connect().await });
    // 让会话真的跑起来（脚本里的第一个事件在 30s 之后）。
    tokio::time::sleep(Duration::from_millis(30)).await;
    task.abort();
    assert!(task.await.is_err(), "取消 ⇒ JoinError::Cancelled");
    assert!(channel.relinquished(), "future 被 drop ⇒ 生命周期停机");
    channel.disconnect().await.expect("收口");
    assert!(dispatcher.is_closed());
    // 槽被摘掉 ⇒ 下一代会建新队列。
    let replacement = slots.acquire("app-key", Arc::new(JobHandlerForTest));
    assert!(!Arc::ptr_eq(&replacement, &dispatcher));
}

/// 收口预算是**有界**的：一个卡住的作业不会让 `disconnect` 无限等。
#[tokio::test]
async fn disconnect_is_bounded_by_the_drain_budget() {
    struct Stuck;

    #[async_trait]
    impl JobHandler for Stuck {
        async fn handle(&self, _job: InboundJob) {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }

    let dispatcher = Arc::new(Dispatcher::with_limits(
        Arc::new(Stuck),
        DispatchLimits {
            job_timeout: Duration::from_secs(30),
            ..DispatchLimits::default()
        },
    ));
    let channel = DingTalkChannel::new(
        "app-key",
        AppSecret::new("app-secret"),
        Some(Arc::new(RecordingInbound::default())),
        Arc::new(FakeOpener),
        Arc::new(FakeDialer {
            script: script_of(Vec::new()),
            log: Arc::new(Mutex::new(FakeLog::default())),
        }),
        Arc::clone(&dispatcher),
        Arc::new(DispatchSlotRegistry::new()),
    );
    // 手工把判决置成"生命周期停机"（等价于 future 被 drop）。
    channel.relinquish_for_test();
    let callback: BotCallbackData = serde_json::from_value(serde_json::json!({
        "senderStaffId": "staff", "conversationId": "chat", "msgId": "m",
        "msgtype": "text", "text": {"content": "hi"},
    }))
    .expect("回调");
    dispatcher.enqueue("chat", InboundJob::new("app-key", callback));
    let started = std::time::Instant::now();
    let outcome = channel.disconnect().await;
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "收口必须落在预算内"
    );
    assert!(matches!(outcome, Err(ChannelError::Shutdown)));
}

// =====================================================================
// 入站闭环（帧 ⇒ 队列 ⇒ 归一化 ⇒ handler）
// =====================================================================

/// 一整条入站回路：回调帧 ⇒ ACK + 入队 ⇒ 归一化 ⇒ `SharedInboundHandler`。
#[tokio::test]
async fn a_callback_frame_reaches_the_shared_handler() {
    let handler = Arc::new(RecordingInbound::default());
    let callback = serde_json::json!({
        "senderStaffId": "staff",
        "conversationId": "chat",
        "conversationType": "1",
        "msgId": "msg-1",
        "msgtype": "text",
        "text": {"content": "hello"},
    });
    let dispatcher = Arc::new(Dispatcher::with_limits(
        crate::dingtalk::jobs::callback_job_handler(
            "app-key",
            Some(Arc::clone(&handler) as SharedInboundHandler),
            Arc::new(NoBotName),
        ),
        DispatchLimits::default(),
    ));
    // 跑一条脚本化的会话：先回调、再 disconnect。
    let script = script_of(vec![
        text(&callback_frame(&callback)),
        text(&disconnect_frame()),
        (Duration::ZERO, None),
    ]);
    let log = Arc::new(Mutex::new(FakeLog::default()));
    let wired = DingTalkChannel::new(
        "app-key",
        AppSecret::new("plain-secret"),
        Some(Arc::clone(&handler) as SharedInboundHandler),
        Arc::new(FakeOpener),
        Arc::new(FakeDialer {
            script,
            log: Arc::clone(&log),
        }),
        Arc::clone(&dispatcher),
        Arc::new(DispatchSlotRegistry::new()),
    );
    wired.connect().await.expect("干净返回");
    // ACK 已经写了（帧循环立刻回），作业还在队列里 ⇒ 收口来等它。
    assert_eq!(log.lock().expect("lock").written.len(), 1);
    assert!(
        Arc::clone(&dispatcher)
            .drain_and_close(Duration::from_secs(2))
            .await
    );

    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    let seen = handler.seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1);
    let message = &seen[0];
    assert_eq!(message.message_id, "msg-1");
    assert_eq!(message.text, "hello");
    assert_eq!(message.source.chat_id, "chat");
    assert!(message.addressed_to_bot, "直聊恒为真");
    // 路由键由**连接**盖章（回调本身不带 robot code）。
    let raw = crate::dingtalk::decode_dingtalk_raw(message).expect("raw is ours");
    assert_eq!(raw.app_id, "app-key");
}

/// 没有发送者的回调在**入队之前**就被丢掉（不进 engine、不进队列）。
#[tokio::test]
async fn a_sender_less_callback_never_enters_the_queue() {
    let handler = Arc::new(RecordingInbound::default());
    let callback = serde_json::json!({
        "conversationId": "chat",
        "msgId": "msg-1",
        "msgtype": "text",
        "text": {"content": "hello"},
    });
    let dispatcher = Arc::new(Dispatcher::with_limits(
        Arc::new(JobHandlerForTest),
        DispatchLimits::default(),
    ));
    let sink = super::DispatchSink {
        app_key: "app-key".to_string(),
        dispatcher: Arc::clone(&dispatcher),
    };
    let payload: BotCallbackData = serde_json::from_value(callback).expect("回调");
    sink.on_callback(payload).await.expect("丢弃不是错误");
    assert_eq!(dispatcher.pending(), 0);
    assert_eq!(dispatcher.queued_total(), 0);
    assert_eq!(handler.calls.load(Ordering::SeqCst), 0);
}

// ---- Router / 安装行 / 租约的极简替身（只为本文件的 `register` 调用服务） ----

fn router() -> crate::engine::Router {
    crate::engine::Router::new(
        Arc::new(crate::engine::NoCommands),
        Arc::new(NoTrigger),
        Arc::new(NoReader),
        Arc::new(NoIssues),
        crate::engine::RouterConfig::default(),
    )
}

struct NoTrigger;

#[async_trait]
impl crate::engine::RunTriggerer for NoTrigger {
    async fn schedule_chat_run(
        &self,
        _params: crate::engine::ChatRunParams,
    ) -> crate::engine::EngineResult<()> {
        Ok(())
    }

    async fn drain(&self) -> crate::engine::EngineResult<()> {
        Ok(())
    }
}

struct NoReader;

#[async_trait]
impl crate::engine::SessionReader for NoReader {
    async fn workspace_identity(
        &self,
        _workspace_id: mc_core::id::Id,
    ) -> crate::engine::EngineResult<crate::engine::WorkspaceIdentity> {
        Ok(crate::engine::WorkspaceIdentity::default())
    }
}

struct NoIssues;

#[async_trait]
impl crate::engine::IssueCreator for NoIssues {
    async fn create_issue(
        &self,
        _params: crate::engine::ChannelIssueParams,
    ) -> crate::engine::EngineResult<crate::engine::ChannelIssueOutcome> {
        Ok(crate::engine::ChannelIssueOutcome {
            issue: crate::engine::ChannelIssue {
                id: mc_core::id::Id::new(),
                number: 1,
                title: "t".to_string(),
            },
            duplicate: false,
            assigned_task_id: None,
        })
    }
}

struct NoInstallations;

#[async_trait]
impl crate::engine::InstallationStore for NoInstallations {
    async fn list_active(&self) -> crate::engine::EngineResult<Vec<crate::engine::Installation>> {
        Ok(Vec::new())
    }
}

struct NoLeases;

#[async_trait]
impl crate::engine::LeaseStore for NoLeases {
    async fn list_held(
        &self,
        _ids: &[mc_core::id::Id],
    ) -> crate::engine::EngineResult<std::collections::HashSet<mc_core::id::Id>> {
        Ok(std::collections::HashSet::new())
    }

    async fn try_acquire(
        &self,
        _params: crate::engine::AcquireLeaseParams,
    ) -> crate::engine::EngineResult<()> {
        Ok(())
    }

    async fn renew(
        &self,
        _params: crate::engine::AcquireLeaseParams,
    ) -> crate::engine::EngineResult<()> {
        Ok(())
    }

    async fn release(
        &self,
        _params: crate::engine::ReleaseLeaseParams,
    ) -> crate::engine::EngineResult<()> {
        Ok(())
    }
}
