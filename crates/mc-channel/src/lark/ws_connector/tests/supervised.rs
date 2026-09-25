//! **退避重连与租约**：用**真的** `engine::Supervisor` 驱动一个包装本片 [`Connector`] 的
//! 测试 Channel（上游 `engine/supervisor.go` 的判据在本仓的落地检查）。
//!
//! 本片专属验收里"断开后按退避重连、且不重复投递（与 M7-2 的租约交互）"就落在这里，逐条：
//!
//! 1. **退避重连**：链路断（`Err`）之后 supervisor 退避再拨 —— 用拨号时刻的间隔量出来，
//!    并且**每次会话都重新引导**（地址是一次性的，见 `ws_endpoint` 的模块文档）；
//! 2. **不重复投递**：两次会话各投一条不同事件，第一条**不**在第二次会话里重投；
//! 3. **租约交互**：租约被**别的副本**持有时**一条会话都不起**（⇒ 不可能有两个副本同时
//!    消费同一条安装）；而在本副本重连期间租约**一直握在手里**（不在重连时释放），
//!    只有监管循环退出（停机）才释放一次。
//!
//! 三个假端口（`InstallationStore` / `LeaseStore` / `Channel`）是本文件自足的：不依赖
//! `engine::supervisor::tests` 的私有脚手架（那是 M7-1 的测试面）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::{InboundMessage, OutboundMessage, SendResult};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_core::timestamp::Timestamp;

use super::{credentials, data_frame, receive_payload, FixedFetcher, RecordingEmitter};
use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult};
use crate::engine::resolvers::{EngineError, EngineResult, PipelineError};
use crate::engine::supervisor::{
    AcquireLeaseParams, Config, Installation, InstallationStore, LeaseStore, ReleaseLeaseParams,
    Supervisor,
};
use crate::lark::ws_connector::{Connector, EventEmitter, SessionKnobs, StopSignal, WsDialer};
use crate::lark::ws_endpoint::EndpointFetcher;
use crate::lark::ws_frame_decoder::{FrameDecoder, LarkJsonFrameDecoder};
use crate::message::{InboundHandler, SharedInboundHandler};
use crate::registry::Registry;

// =====================================================================
// 假端口
// =====================================================================

/// 固定一行安装的 `InstallationStore`。
struct FakeInstallations {
    rows: Vec<Installation>,
}

#[async_trait]
impl InstallationStore for FakeInstallations {
    async fn list_active(&self) -> EngineResult<Vec<Installation>> {
        Ok(self.rows.clone())
    }
}

/// 进程内租约（比 M7-2 的 `InProcessLeaseStore` 简单：只够本用例用），带计数与"拒绝"开关。
struct FakeLeases {
    held: Mutex<HashMap<Id, String>>,
    deny: Mutex<Option<Id>>,
    acquired: AtomicU64,
    released: AtomicU64,
    held_now: AtomicU64,
}

impl FakeLeases {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            held: Mutex::new(HashMap::new()),
            deny: Mutex::new(None),
            acquired: AtomicU64::new(0),
            released: AtomicU64::new(0),
            held_now: AtomicU64::new(0),
        })
    }

    /// 从这一刻起，`id` 的租约由**别的副本**持有（本副本永远取不到）。
    fn deny(&self, id: Id) {
        *self.deny.lock().expect("deny") = Some(id);
    }

    fn acquired(&self) -> u64 {
        self.acquired.load(Ordering::SeqCst)
    }

    fn released(&self) -> u64 {
        self.released.load(Ordering::SeqCst)
    }

    fn held_now(&self) -> u64 {
        self.held_now.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LeaseStore for FakeLeases {
    async fn list_held(&self, _ids: &[Id]) -> EngineResult<std::collections::HashSet<Id>> {
        Ok(self.held.lock().expect("held").keys().copied().collect())
    }

    async fn try_acquire(&self, params: AcquireLeaseParams) -> EngineResult<()> {
        if *self.deny.lock().expect("deny") == Some(params.installation_id) {
            return Err(EngineError::Pipeline(PipelineError::LeaseNotAcquired));
        }
        let mut held = self.held.lock().expect("held");
        match held.get(&params.installation_id) {
            Some(token) if *token == params.token => Ok(()),
            Some(_) => Err(EngineError::Pipeline(PipelineError::LeaseNotAcquired)),
            None => {
                held.insert(params.installation_id, params.token.clone());
                self.acquired.fetch_add(1, Ordering::SeqCst);
                self.held_now.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
    }

    async fn renew(&self, params: AcquireLeaseParams) -> EngineResult<()> {
        self.try_acquire(params).await
    }

    async fn release(&self, params: ReleaseLeaseParams) -> EngineResult<()> {
        let mut held = self.held.lock().expect("held");
        if held.get(&params.installation_id) == Some(&params.token) {
            held.remove(&params.installation_id);
            self.released.fetch_add(1, Ordering::SeqCst);
            self.held_now.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// 共享的入站入口（本例只在端口签名里占位）。
struct NoopHandler;

#[async_trait]
impl InboundHandler for NoopHandler {
    async fn handle(&self, _message: InboundMessage) -> ChannelResult<()> {
        Ok(())
    }
}

/// 用本片的 [`Connector`] 实现的测试 Channel（生产实现是 M7-12 的 `feishu_channel.rs`）。
struct WsChannel {
    connector: Arc<Connector>,
    emitter: Arc<RecordingEmitter>,
    signal: Mutex<Option<StopSignal>>,
}

#[async_trait]
impl Channel for WsChannel {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Lark
    }

    async fn connect(&self) -> ChannelResult<()> {
        let (signal, handle) = StopSignal::pair();
        *self.signal.lock().expect("signal") = Some(signal);
        self.connector
            .run_session(
                &credentials(),
                Arc::clone(&self.emitter) as Arc<dyn EventEmitter>,
                handle,
            )
            .await
            .map(|_| ())
    }

    /// 拆链路 = 置停机位（与 `dingtalk` 侧同款：`connect` 的 future 被 supervisor 丢弃时，
    /// 这里把会话从内部收掉）。
    async fn disconnect(&self) -> ChannelResult<()> {
        if let Some(signal) = self.signal.lock().expect("signal").take() {
            signal.stop();
        }
        Ok(())
    }

    /// 本片**不**做出站（M7-13）：失败关闭。
    async fn send(&self, _out: OutboundMessage) -> ChannelResult<SendResult> {
        Err(ChannelError::Transport {
            message: "lark: outbound is not wired yet (M7-13)".to_string(),
        })
    }

    fn capabilities(&self) -> Capability {
        Capability::TEXT
    }
}

// =====================================================================
// 装配
// =====================================================================

struct Wiring {
    supervisor: Arc<Supervisor>,
    handle: Option<crate::engine::supervisor::SupervisorHandle>,
    emitter: Arc<RecordingEmitter>,
    dialer: Arc<super::ScriptedDialer>,
    fetcher: Arc<FixedFetcher>,
}

impl Wiring {
    fn build(installation: Id, leases: &Arc<FakeLeases>, sessions: &[(String, bool)]) -> Self {
        let log = Arc::new(Mutex::new(super::SocketLog::default()));
        let dialer = Arc::new(super::ScriptedDialer::new());
        for (message_id, ends_cleanly) in sessions {
            dialer.push(
                &log,
                vec![
                    Ok(super::WsEvent::Binary(data_frame(
                        &receive_payload(message_id),
                        message_id,
                    ))),
                    Err(ChannelError::Transport {
                        message: "scripted link failure".to_string(),
                    }),
                ],
                *ends_cleanly,
            );
        }
        let fetcher = Arc::new(FixedFetcher::new(Duration::from_millis(50)));
        let emitter = RecordingEmitter::new();
        let connector = Arc::new(
            Connector::new(
                Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
                Arc::clone(&dialer) as Arc<dyn WsDialer>,
                Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
            )
            .with_knobs(SessionKnobs {
                read_deadline: Duration::from_millis(500),
                ping_interval: Duration::from_millis(50),
                ..SessionKnobs::default()
            }),
        );

        let registry = Arc::new(Registry::new());
        registry.register(
            ChannelKind::Lark,
            Arc::new({
                let connector = Arc::clone(&connector);
                let emitter = Arc::clone(&emitter);
                move |_config: ChannelConfig| {
                    let channel = Arc::new(WsChannel {
                        connector: Arc::clone(&connector),
                        emitter: Arc::clone(&emitter),
                        signal: Mutex::new(None),
                    });
                    Ok(channel as Arc<dyn Channel>)
                }
            }),
        );

        let handler: SharedInboundHandler = Arc::new(NoopHandler);
        let supervisor = Supervisor::new(
            Arc::new(FakeInstallations {
                rows: vec![Installation {
                    id: installation,
                    kind: ChannelKind::Lark,
                    fingerprint: "fp-1".to_string(),
                    config: serde_json::json!({ "app_id": "cli_app_x" }),
                }],
            }),
            Arc::clone(leases) as Arc<dyn LeaseStore>,
            registry,
            handler,
            Config {
                lease_ttl: Duration::from_secs(60),
                lease_renew_interval: Duration::from_secs(20),
                poll_interval: Duration::from_millis(20),
                lease_error_retry_interval: Duration::from_millis(20),
                lease_expiry_safety_margin: Duration::from_secs(1),
                min_backoff: Duration::from_millis(30),
                max_backoff: Duration::from_millis(60),
                reset_backoff_after: Duration::from_secs(30),
                rotation_wait_timeout: Duration::from_millis(50),
                lease_release_timeout: Duration::from_millis(200),
                disconnect_timeout: Duration::from_millis(200),
                shutdown_timeout: Duration::from_millis(500),
                now: Arc::new(Timestamp::now),
            },
        )
        .expect("监管器装配");

        Self {
            supervisor: Arc::new(supervisor),
            handle: None,
            emitter,
            dialer,
            fetcher,
        }
    }

    fn spawn(&mut self) {
        self.handle = Some(Arc::clone(&self.supervisor).spawn().expect("起监管任务"));
    }

    /// 等一条断言成立（有界轮询；用例的唯一"等待原语"）。
    async fn wait_until(&self, label: &str, condition: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "等超时：{label}（连过的会话 {} 次，投递 {:?}）",
                self.dialer.attempts(),
                self.emitter.message_ids()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await;
        }
    }
}

// =====================================================================
// 一、租约在别处 ⇒ 一条会话都不起
// =====================================================================

/// **不重复投递的租约那一半**：租约由别的副本持有时，本副本**不建连、不投递**
/// （否则两个副本会各自消费同一条安装 ⇔ 同一条事件被投两次）。
#[tokio::test]
async fn a_lease_held_elsewhere_means_no_session_and_no_delivery() {
    let installation = Id::new();
    let leases = FakeLeases::new();
    leases.deny(installation);
    let mut wiring = Wiring::build(installation, &leases, &[("om-1".to_string(), true)]);

    wiring.spawn();
    // 给监管器几个扫描周期（poll_interval = 20ms）去尝试取租约。
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert_eq!(wiring.dialer.attempts(), 0, "拿不到租约就不该拨号");
    assert_eq!(
        wiring.fetcher.calls(),
        0,
        "更不该引导（省一次带凭据的出站）"
    );
    assert_eq!(wiring.emitter.count(), 0, "一条也不该投");
    assert_eq!(leases.acquired(), 0);
    assert_eq!(leases.released(), 0);
    wiring.shutdown().await;
}

// =====================================================================
// 二、链路断 ⇒ 退避重连；租约与重连的交互；事件不重投
// =====================================================================

/// 两条会话：第一次投 `om-1` 之后链路断（`Err`），supervisor 退避后重连并投 `om-2`。
#[tokio::test]
async fn a_dropped_link_reconnects_under_backoff_and_never_redelivers() {
    let installation = Id::new();
    let leases = FakeLeases::new();
    let mut wiring = Wiring::build(
        installation,
        &leases,
        &[("om-1".to_string(), false), ("om-2".to_string(), false)],
    );

    wiring.spawn();
    wiring
        .wait_until("第一次会话投出 om-1", || wiring.emitter.count() >= 1)
        .await;
    wiring
        .wait_until("退避后重连并投出 om-2", || {
            wiring.emitter.count() >= 2
        })
        .await;
    assert!(wiring.dialer.attempts() >= 2, "断链之后必须重拨");

    // ① 退避确实存在：相邻两次拨号的间隔 ≥ `min_backoff`（30ms）。
    let gaps = wiring.dialer.attempt_gaps();
    assert!(
        gaps.iter().all(|gap| *gap >= Duration::from_millis(20)),
        "断链后**立刻**重拨说明退避没生效：{gaps:?}"
    );

    // ② 每次会话都**重新引导**：地址是一次性的（`device_id` 每次轮换），复用会拿到一次
    //    "看起来像 lark 宕机"的鉴权拒绝。
    assert_eq!(
        wiring.fetcher.calls(),
        wiring.dialer.attempts(),
        "每次会话都要重新 POST /callback/ws/endpoint"
    );

    // ③ 不重复投递：两条事件各一次，没有因为重连被重投。
    assert_eq!(wiring.emitter.message_ids(), vec!["om-1", "om-2"]);

    // ④ **租约与重连的交互（实测口径，别按直觉写）**：本仓 `Supervisor::supervise` 每一圈
    //    循环都在**退避之前**释放租约（`release` → `record_failure` → `sleep`），下一圈开头
    //    再 CAS 重取 ⇒
    //
    //    - 每一次拨号之前都先持有过租约（`acquired >= attempts`）⇒ **不可能两个副本同时
    //      连一条安装**（这正是"不重复投递"的第一道闸）；
    //    - 但退避窗口里租约**不在手上** ⇒ 期间别的副本可以抢走它；抢不到的那一方不再建连
    //      （见 [`a_lease_held_elsewhere_means_no_session_and_no_delivery`]）。跨窗口的去重靠
    //      M7-2 的 `channel_inbound_message_dedup`（本片的 ACK 只在 emit 成功之后发）。
    assert!(
        leases.acquired() >= u64::try_from(wiring.dialer.attempts()).unwrap_or(u64::MAX),
        "每一次拨号之前都必须持有过租约：acquired={} attempts={}",
        leases.acquired(),
        wiring.dialer.attempts()
    );
    assert!(
        leases.released() >= 1,
        "本仓在退避之前就释放租约（实测口径）"
    );
    assert_eq!(
        leases.released() + leases.held_now(),
        leases.acquired(),
        "释放 + 在持 = 取到过的（没有丢失或重复释放）"
    );

    wiring.shutdown().await;
    assert_eq!(leases.held_now(), 0, "停机后不留租约");
}

/// 对端**正常关闭**（`Closed`）也算一次尝试结束 ⇒ supervisor 照样退避重连（上游逐字：
/// `Run` 返回 nil 时 Hub 仍会重拨，因为安装还是活跃的）。
#[tokio::test]
async fn a_clean_close_is_still_reconnected() {
    let installation = Id::new();
    let leases = FakeLeases::new();
    let mut wiring = Wiring::build(
        installation,
        &leases,
        &[("om-1".to_string(), true), ("om-2".to_string(), true)],
    );

    wiring.spawn();
    wiring
        .wait_until("两次会话都跑过", || wiring.dialer.attempts() >= 2)
        .await;
    assert_eq!(wiring.emitter.message_ids(), vec!["om-1", "om-2"]);
    // 干净收尾也要退避：两次拨号的间隔 ≥ `min_backoff`。
    let gaps = wiring.dialer.attempt_gaps();
    assert!(
        gaps.iter().all(|gap| *gap >= Duration::from_millis(20)),
        "干净收尾也必须退避：{gaps:?}"
    );
    wiring.shutdown().await;
}
