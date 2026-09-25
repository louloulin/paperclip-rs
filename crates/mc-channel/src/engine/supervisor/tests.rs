use super::*;
use crate::capability::Capability;
use crate::channel::{ChannelConfig, ChannelError};
use crate::engine::resolvers::PipelineError;
use async_trait::async_trait;
use mc_core::channel::message::{InboundMessage, OutboundMessage, SendResult};
use mc_core::channel::ChannelKind;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 退避序列：翻倍、触顶、稳定后重置（纯状态机，不碰时钟）。
#[test]
fn backoff_doubles_caps_and_resets() {
    let mut backoff = Backoff::new(
        Duration::from_secs(2),
        Duration::from_secs(10),
        Duration::from_secs(60),
    );
    assert_eq!(backoff.current(), Duration::from_secs(2));
    assert_eq!(backoff.record_failure(), Duration::from_secs(2));
    assert_eq!(backoff.record_failure(), Duration::from_secs(4));
    assert_eq!(backoff.record_failure(), Duration::from_secs(8));
    assert_eq!(backoff.record_failure(), Duration::from_secs(10), "触顶");
    assert_eq!(backoff.record_failure(), Duration::from_secs(10), "停在顶");
    assert_eq!(backoff.failures(), 5);

    backoff.record_uptime(Duration::from_secs(60));
    assert_eq!(backoff.current(), Duration::from_secs(2));
    assert_eq!(backoff.failures(), 0);

    backoff.record_failure();
    backoff.record_uptime(Duration::from_secs(59));
    assert_eq!(backoff.current(), Duration::from_secs(4), "不够久 ⇒ 不重置");
}

/// 配置默认值 + 时间不变式（`poll <= renew < ttl`、保护余量留得下）。
#[test]
fn config_defaults_and_invariants() {
    let cfg = Config::default().with_defaults();
    assert_eq!(cfg.lease_ttl, DEFAULT_LEASE_TTL);
    assert_eq!(
        cfg.poll_interval,
        DEFAULT_POLL_INTERVAL.min(cfg.lease_renew_interval / 2)
    );
    assert!(cfg.poll_interval <= cfg.lease_renew_interval);
    assert!(cfg.lease_renew_interval < cfg.lease_ttl);
    assert_eq!(cfg.shutdown_timeout, DEFAULT_SHUTDOWN_TIMEOUT);
    cfg.validate().expect("默认值必须合法");

    let bad_renew = Config {
        lease_ttl: Duration::from_secs(10),
        lease_renew_interval: Duration::from_secs(20),
        ..Config::default()
    };
    assert!(bad_renew.prepare().is_err(), "renew >= ttl 必须被拒");

    let bad_margin = Config {
        lease_ttl: Duration::from_secs(30),
        lease_renew_interval: Duration::from_secs(10),
        lease_expiry_safety_margin: Duration::from_secs(25),
        ..Config::default()
    };
    assert!(bad_margin.prepare().is_err(), "保护余量必须小于 ttl-renew");
}

/// 租约令牌：同进程不同任务（generation）拿到不同令牌。
#[test]
fn lease_tokens_differ_per_generation() {
    assert_ne!(lease_token("node", 1), lease_token("node", 2));
    assert_eq!(lease_token("n", 7), "n-g7");
    assert!(!new_node_id().is_empty());
}

/// 无工厂的 kind 被跳过：不取租约、不起连接。
#[tokio::test]
async fn sweep_skips_kinds_without_a_factory() {
    let h = Harness::new(vec![row(ChannelKind::Slack)]);
    h.supervisor.sweep().await;
    assert!(h.supervisor.supervised().is_empty(), "没有工厂 ⇒ 不起连接");
    assert_eq!(h.leases.acquired(), 0);
}

/// 取到租约 → 造 Channel → connect 阻塞 → 停机时断开并释放租约。
#[tokio::test]
async fn supervise_connects_and_releases_on_shutdown() {
    let h = Harness::new(vec![row(ChannelKind::Lark)]);
    h.register("connect-blocked");
    let handle = h.supervisor.clone().spawn().expect("spawn");
    h.wait_for_connects(1).await;
    assert_eq!(h.supervisor.supervised().len(), 1);
    assert_eq!(h.leases.acquired(), 1);
    assert_eq!(h.leases.released(), 0, "连接还在跑 ⇒ 租约没释放");

    handle.shutdown().await;
    assert_eq!(h.leases.released(), 1, "停机必须释放租约");
    assert_eq!(h.channels.disconnects(), 1, "停机必须断开");
    assert!(h.supervisor.supervised().is_empty());
}

/// **取消用例**（issue 的专属验收）：连接阻塞到被丢弃为止；取消后 `disconnect` 跑完，
/// 且不产生错误（取消不是错误）。
#[tokio::test]
async fn shutdown_cancels_a_blocked_connection_without_an_error() {
    let h = Harness::new(vec![row(ChannelKind::Lark)]);
    h.register("connect-blocked");
    let handle = h.supervisor.clone().spawn().expect("spawn");
    h.wait_for_connects(1).await;
    handle.shutdown().await;
    assert_eq!(h.channels.disconnects(), 1, "被取消的连接必须断开");
    assert_eq!(h.leases.released(), 1);
    assert!(h.supervisor.supervised().is_empty());
}

/// 租约在别处 ⇒ 不连、不建（`list_held` 命中）。
#[tokio::test]
async fn lease_held_elsewhere_skips_the_connection() {
    let install = row(ChannelKind::Lark);
    let h = Harness::new(vec![install.clone()]);
    h.register("connect-ok");
    h.leases.mark_held(install.id).await;
    h.supervisor.sweep().await;
    assert!(h.supervisor.supervised().is_empty());
    assert_eq!(h.leases.acquired(), 0);
    assert_eq!(h.channels.connects(), 0);
}

/// 取租约时被告知"别处持有" ⇒ 任务干净退出（不连、不重试）。
#[tokio::test]
async fn contended_acquire_exits_the_supervisor() {
    let install = row(ChannelKind::Lark);
    let h = Harness::new(vec![install.clone()]);
    h.register("connect-ok");
    h.leases.deny_acquire(install.id).await;
    let handle = h.supervisor.clone().spawn().expect("spawn");
    tokio::time::sleep(Duration::from_millis(40)).await;
    handle.shutdown().await;
    assert_eq!(h.channels.connects(), 0);
}

/// 凭据指纹漂移 ⇒ 拆掉在跑的连接并重建（重装渠道必须被拾起）。
#[tokio::test]
async fn credential_rotation_restarts_the_connection() {
    let mut install = row(ChannelKind::Lark);
    let h = Harness::new(vec![install.clone()]);
    h.register("connect-blocked");
    h.supervisor.sweep().await;
    h.wait_for_connects(1).await;

    install.fingerprint = "rotated".into();
    h.installs.replace(install.clone()).await;
    h.supervisor.sweep().await;
    h.wait_for_connects(2).await;
    assert_eq!(h.supervisor.supervised().len(), 1, "轮换后仍是一个任务");
}

/// 配置非法 ⇒ 构造被拒（不 panic，也不起任务）。
#[tokio::test]
async fn new_rejects_invalid_config() {
    let result = Supervisor::new(
        Arc::new(RecordingInstallations::new(Vec::new())),
        Arc::new(RecordingLeases::default()),
        Arc::new(Registry::new()),
        Arc::new(NullHandler),
        Config {
            lease_ttl: Duration::from_secs(1),
            lease_renew_interval: Duration::from_secs(9),
            ..Config::default()
        },
    );
    assert!(result.is_err(), "renew >= ttl 必须被拒");
}

// ------------------------------------------------------------------
// 测试替身
// ------------------------------------------------------------------

fn row(kind: ChannelKind) -> Installation {
    Installation {
        id: Id::new(),
        kind,
        fingerprint: format!("fp-{}", kind.as_str()),
        config: serde_json::json!({ "app_id": "test" }),
    }
}

/// 一条用例一个 harness：store / lease / registry / 观测替身都在这里。
struct Harness {
    supervisor: Arc<Supervisor>,
    leases: Arc<RecordingLeases>,
    installs: Arc<RecordingInstallations>,
    channels: Arc<StubChannels>,
}

impl Harness {
    fn new(rows: Vec<Installation>) -> Self {
        let installs = Arc::new(RecordingInstallations::new(rows));
        let leases = Arc::new(RecordingLeases::default());
        let channels = Arc::new(StubChannels::default());
        let supervisor = Arc::new(
            Supervisor::new(
                installs.clone(),
                leases.clone(),
                Arc::new(Registry::new()),
                Arc::new(NullHandler),
                Config {
                    lease_ttl: Duration::from_millis(300),
                    lease_renew_interval: Duration::from_millis(100),
                    poll_interval: Duration::from_millis(10),
                    lease_error_retry_interval: Duration::from_millis(10),
                    lease_expiry_safety_margin: Duration::from_millis(20),
                    min_backoff: Duration::from_millis(1),
                    max_backoff: Duration::from_millis(4),
                    reset_backoff_after: Duration::from_millis(50),
                    rotation_wait_timeout: Duration::from_millis(60),
                    lease_release_timeout: Duration::from_millis(50),
                    disconnect_timeout: Duration::from_millis(50),
                    shutdown_timeout: Duration::from_millis(50),
                    ..Config::default()
                },
            )
            .expect("config"),
        );
        Self {
            supervisor,
            leases,
            installs,
            channels,
        }
    }

    /// 在注册表里放一个返回 `StubChannel` 的工厂。
    fn register(&self, mode: &'static str) {
        let channels = Arc::clone(&self.channels);
        self.supervisor.registry.register(
            ChannelKind::Lark,
            Arc::new(move |_config: ChannelConfig| {
                Ok(Arc::new(StubChannel {
                    mode,
                    channels: Arc::clone(&channels),
                }) as Arc<dyn Channel>)
            }),
        );
    }

    async fn wait_for_connects(&self, want: u64) {
        for _ in 0..200 {
            if self.channels.connects() >= want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("连接数没有达到 {want}");
    }
}

#[derive(Default)]
struct StubChannels {
    connects: AtomicU64,
    disconnects: AtomicU64,
}

impl StubChannels {
    fn connects(&self) -> u64 {
        self.connects.load(Ordering::SeqCst)
    }
    fn disconnects(&self) -> u64 {
        self.disconnects.load(Ordering::SeqCst)
    }
}

struct StubChannel {
    mode: &'static str,
    channels: Arc<StubChannels>,
}

#[async_trait]
impl Channel for StubChannel {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Lark
    }
    async fn connect(&self) -> ChannelResult<()> {
        self.channels.connects.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            "connect-blocked" => {
                std::future::pending::<()>().await;
                Ok(())
            }
            _ => Ok(()),
        }
    }
    async fn disconnect(&self) -> ChannelResult<()> {
        self.channels.disconnects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn send(&self, _out: OutboundMessage) -> ChannelResult<SendResult> {
        Err(ChannelError::Shutdown)
    }
    fn capabilities(&self) -> Capability {
        Capability::TEXT
    }
}

struct NullHandler;

#[async_trait]
impl crate::message::InboundHandler for NullHandler {
    async fn handle(&self, _message: InboundMessage) -> ChannelResult<()> {
        Ok(())
    }
}

struct RecordingInstallations {
    rows: tokio::sync::Mutex<Vec<Installation>>,
}

impl RecordingInstallations {
    fn new(rows: Vec<Installation>) -> Self {
        Self {
            rows: tokio::sync::Mutex::new(rows),
        }
    }
    async fn replace(&self, row: Installation) {
        let mut rows = self.rows.lock().await;
        if let Some(slot) = rows.iter_mut().find(|existing| existing.id == row.id) {
            *slot = row;
        }
    }
}

#[async_trait]
impl InstallationStore for RecordingInstallations {
    async fn list_active(&self) -> EngineResult<Vec<Installation>> {
        Ok(self.rows.lock().await.clone())
    }
}

#[derive(Default)]
struct RecordingLeases {
    held: tokio::sync::Mutex<HashSet<Id>>,
    deny: tokio::sync::Mutex<HashSet<Id>>,
    acquired: AtomicU64,
    released: AtomicU64,
}

impl RecordingLeases {
    async fn mark_held(&self, id: Id) {
        self.held.lock().await.insert(id);
    }
    async fn deny_acquire(&self, id: Id) {
        self.deny.lock().await.insert(id);
    }
    fn acquired(&self) -> u64 {
        self.acquired.load(Ordering::SeqCst)
    }
    fn released(&self) -> u64 {
        self.released.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LeaseStore for RecordingLeases {
    async fn list_held(&self, ids: &[Id]) -> EngineResult<HashSet<Id>> {
        let held = self.held.lock().await;
        Ok(ids.iter().copied().filter(|id| held.contains(id)).collect())
    }
    async fn try_acquire(&self, params: AcquireLeaseParams) -> EngineResult<()> {
        if self.deny.lock().await.contains(&params.installation_id) {
            return Err(PipelineError::LeaseNotAcquired.into());
        }
        self.acquired.fetch_add(1, Ordering::SeqCst);
        self.held.lock().await.insert(params.installation_id);
        Ok(())
    }
    async fn renew(&self, params: AcquireLeaseParams) -> EngineResult<()> {
        if !self.held.lock().await.contains(&params.installation_id) {
            return Err(PipelineError::LeaseNotAcquired.into());
        }
        Ok(())
    }
    async fn release(&self, params: ReleaseLeaseParams) -> EngineResult<()> {
        self.released.fetch_add(1, Ordering::SeqCst);
        self.held.lock().await.remove(&params.installation_id);
        Ok(())
    }
}
