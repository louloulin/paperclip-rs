//! 长连接监管：**端口** + 退避重连 + 租约（上游 `engine/supervisor.go`，996 行）。
//!
//! - **写者**：M7-0 建（anchor 只落端口与签名）；**M7-1 实现监管行为**（`docs/60` §3.3）。
//! - **端口实现**归后续片（`LeaseStore` 的进程内实现 = M7-2 的 `engine/lease.rs`；
//!   `InstallationStore` 的 DB 实现同一波落地）。本文件只**定义**端口并驱动它们。
//!
//! # 这份文件解决什么
//!
//! 每个**活跃安装**一条长连接：`sweep` 每 `poll_interval` 扫一遍活跃安装，对"本进程还没监管
//! 且租约不在别处"的行起一个监管任务；任务自己循环
//! **取租约 → 造 Channel → `connect`（阻塞跑接收循环）→ 并行续租 → 退出即释放租约 + 退避**。
//!
//! # 无 Redis ⇒ 进程内租约 + **单副本部署契约**（R-M7-1，`docs/60` §2.5）
//!
//! 上游 `redis_lease_store.go` 用 Redis 做租约 CAS，多副本下只有一个副本持有某 installation 的
//! 长连接。本仓**没有 Redis 且本波不引入** ⇒ 换部署形态（上游四处"无 Redis"路径都有等价降级
//! 语义）。**生产契约 = 单副本或"渠道连接只在一个副本上开"**。
//! `LeaseMetrics` 与租约的实现一起归 M7-2 的 `engine/lease.rs`。
//!
//! # 取消语义（`docs/60` §2.6 第 4 条 / issue 的"一条取消用例"）
//!
//! 上游靠 `ctx` 取消让阻塞的 `Connect` 返回 `nil`；本仓的等价物是**丢掉 `connect` 的 future**
//! （`select!` 在停机 / 租约丢失时选中另一支），随后 `disconnect`。契约不变：**取消不是错误**。
//! 用例见 `supervisor/tests.rs` 的 `shutdown_cancels_a_blocked_connection_without_an_error`。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mc_core::id::Id;
use mc_core::timestamp::Timestamp;
use tokio::sync::watch;

use crate::channel::{Channel, ChannelResult};
use crate::engine::resolvers::{EngineError, EngineResult};

pub mod backoff;
mod ports;
mod runtime;

use crate::message::SharedInboundHandler;
use crate::registry::Registry;
pub use backoff::Backoff;
pub use ports::{
    AcquireLeaseParams, Installation, InstallationStore, LeaseStore, ReleaseLeaseParams,
};
use runtime::{
    channel_config, elapsed, is_lease_held, lease_token, new_node_id, plus, renew_loop,
    sleep_cancellable, wait_stop,
};

// =====================================================================
// 配置
// =====================================================================

/// 当前时间的注入点（上游 `Config.Now func() time.Time`）。
pub type NowFn = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// 监管器的生命周期调参（上游 `Config`）。零值 = 未设置，见 [`Config::with_defaults`]。
#[derive(Clone)]
pub struct Config {
    /// 一次成功的取租约有效期。
    pub lease_ttl: Duration,
    /// 续租节奏；**必须**显著小于 `lease_ttl`，一次漏续不能丢租约。
    pub lease_renew_interval: Duration,
    /// 扫描新安装（或租约在别处过期的安装）的节奏。
    pub poll_interval: Duration,
    /// 续租传输错误后的快速重试节奏。
    pub lease_error_retry_interval: Duration,
    /// 从最后一次确认的 TTL 里减掉的安全余量：被分区的持有者要在后继者接管**之前**断开。
    pub lease_expiry_safety_margin: Duration,
    /// 每安装重连退避的下界 / 上界。
    pub min_backoff: Duration,
    pub max_backoff: Duration,
    /// 连接稳定运行多久后重置退避。
    pub reset_backoff_after: Duration,
    /// 一次 sweep 里有安装换凭据时，等**前任**放开租约的上限（整批共享一个截止时间）。
    pub rotation_wait_timeout: Duration,
    /// 单次释放租约 / 断开的时限（都在已取消的父上下文之外的新等待上有界执行）。
    pub lease_release_timeout: Duration,
    pub disconnect_timeout: Duration,
    /// 停机时等监管任务收尾的上限。
    pub shutdown_timeout: Duration,
    /// 当前时间（测试注入）。
    pub now: NowFn,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("lease_ttl", &self.lease_ttl)
            .field("lease_renew_interval", &self.lease_renew_interval)
            .field("poll_interval", &self.poll_interval)
            .field("min_backoff", &self.min_backoff)
            .field("max_backoff", &self.max_backoff)
            .finish_non_exhaustive()
    }
}

/// 默认租约有效期（上游 `DefaultLeaseTTL`）。
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(180);
/// 默认扫描间隔（也是"租约迁到别的副本要多久"）（上游 `DefaultPollInterval`）。
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// 默认停机收尾上限（上游 `DefaultShutdownTimeout`）。
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

impl Default for Config {
    fn default() -> Self {
        let now: NowFn = Arc::new(Timestamp::now);
        Self {
            lease_ttl: Duration::ZERO,
            lease_renew_interval: Duration::ZERO,
            poll_interval: Duration::ZERO,
            lease_error_retry_interval: Duration::ZERO,
            lease_expiry_safety_margin: Duration::ZERO,
            min_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            reset_backoff_after: Duration::ZERO,
            rotation_wait_timeout: Duration::ZERO,
            lease_release_timeout: Duration::ZERO,
            disconnect_timeout: Duration::ZERO,
            shutdown_timeout: Duration::ZERO,
            now,
        }
    }
}

impl Config {
    /// 填默认值（零值 = 未设置）。与上游 `withDefaults` 逐条对应。
    #[must_use]
    pub fn with_defaults(mut self) -> Self {
        if self.lease_ttl.is_zero() {
            self.lease_ttl = DEFAULT_LEASE_TTL;
        }
        if self.lease_renew_interval.is_zero() {
            self.lease_renew_interval = (self.lease_ttl / 3).min(Duration::from_secs(60));
        }
        if self.poll_interval.is_zero() {
            self.poll_interval = DEFAULT_POLL_INTERVAL.min(self.lease_renew_interval / 2);
        }
        if self.lease_error_retry_interval.is_zero() {
            self.lease_error_retry_interval =
                Duration::from_secs(5).min(self.lease_renew_interval / 4);
        }
        if self.lease_expiry_safety_margin.is_zero() {
            self.lease_expiry_safety_margin = Duration::from_secs(5).min(self.lease_ttl / 10);
        }
        if self.min_backoff.is_zero() {
            self.min_backoff = Duration::from_secs(2);
        }
        if self.max_backoff.is_zero() {
            self.max_backoff = Duration::from_secs(60);
        }
        if self.reset_backoff_after.is_zero() {
            self.reset_backoff_after = Duration::from_secs(60);
        }
        if self.lease_release_timeout.is_zero() {
            self.lease_release_timeout = Duration::from_secs(5);
        }
        if self.disconnect_timeout.is_zero() {
            self.disconnect_timeout = Duration::from_secs(5);
        }
        if self.rotation_wait_timeout.is_zero() {
            self.rotation_wait_timeout = self.disconnect_timeout + self.lease_release_timeout;
        }
        if self.shutdown_timeout.is_zero() {
            self.shutdown_timeout = DEFAULT_SHUTDOWN_TIMEOUT;
        }
        self
    }

    /// 时间不变式校验（上游 `Validate`）：`poll <= renew < ttl`，且安全余量留得下。
    ///
    /// # Errors
    ///
    /// 任一时间关系不成立时返回 `EngineError::Infra`（不许构造出会互相踩的监管器）。
    pub fn validate(&self) -> EngineResult<()> {
        if self.poll_interval.is_zero()
            || self.lease_renew_interval.is_zero()
            || self.lease_ttl.is_zero()
        {
            return Err(EngineError::infra(
                "channel engine: lease intervals must be positive",
            ));
        }
        if self.poll_interval > self.lease_renew_interval
            || self.lease_renew_interval >= self.lease_ttl
        {
            return Err(EngineError::infra(format!(
                "channel engine: require poll <= renew < ttl (poll={:?} renew={:?} ttl={:?})",
                self.poll_interval, self.lease_renew_interval, self.lease_ttl
            )));
        }
        if self.lease_error_retry_interval.is_zero() {
            return Err(EngineError::infra(
                "channel engine: lease error retry interval must be positive",
            ));
        }
        if self.lease_expiry_safety_margin.is_zero()
            || self.lease_expiry_safety_margin
                >= self
                    .lease_ttl
                    .checked_sub(self.lease_renew_interval)
                    .unwrap()
        {
            return Err(EngineError::infra(format!(
                "channel engine: lease expiry safety margin must be positive and less than ttl-renew (margin={:?})",
                self.lease_expiry_safety_margin
            )));
        }
        Ok(())
    }

    /// 填默认值 + 校验。
    ///
    /// # Errors
    ///
    /// 同 [`Config::validate`]。
    pub fn prepare(mut self) -> EngineResult<Self> {
        self = self.with_defaults();
        self.validate()?;
        Ok(self)
    }
}

// =====================================================================
// 监管器
// =====================================================================

struct Entry {
    stop_tx: watch::Sender<bool>,
    generation: u64,
    fingerprint: String,
}

/// 停机句柄：宿主用它停掉整条渠道连接面（停机链的第一步）。
pub struct SupervisorHandle {
    supervisor: Arc<Supervisor>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for SupervisorHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SupervisorHandle")
            .field("finished", &self.task.is_finished())
            .field("supervised", &self.supervisor.supervised().len())
            .finish()
    }
}

impl SupervisorHandle {
    /// 由 [`Supervisor::spawn`] 构造。
    pub fn new(supervisor: Arc<Supervisor>, task: tokio::task::JoinHandle<()>) -> Self {
        Self { supervisor, task }
    }

    /// 扫描任务是否已经结束（诊断用）。
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// 优雅停机：先请求取消（含每个在跑的连接）、**有界**地等收尾，再收掉扫描任务。
    ///
    /// "连接挂着不退"必须被消灭（R-M7-7）：所以取消之后要等每个监管任务断开并释放租约；
    /// 超时只打 warn（租约按 TTL 自然过期），然后 `abort` 扫描任务。
    pub async fn shutdown(self) {
        self.supervisor.stop_and_wait().await;
        self.task.abort();
        let _ = self.task.await;
    }
}

/// 长连接监管器（上游 `Supervisor`）。
pub struct Supervisor {
    installations: Arc<dyn InstallationStore>,
    leases: Arc<dyn LeaseStore>,
    registry: Arc<Registry>,
    handler: SharedInboundHandler,
    cfg: Config,
    node_id: String,
    entries: Mutex<HashMap<Id, Entry>>,
    generation: AtomicU64,
    stopping: AtomicBool,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Supervisor")
            .field("registry", &self.registry)
            .field("node_id", &self.node_id)
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// 装配（**不**起任何任务）；校验时间不变式。
    ///
    /// # Errors
    ///
    /// 配置时间关系非法时返回 `EngineError::Infra`。
    pub fn new(
        installations: Arc<dyn InstallationStore>,
        leases: Arc<dyn LeaseStore>,
        registry: Arc<Registry>,
        handler: SharedInboundHandler,
        cfg: Config,
    ) -> EngineResult<Self> {
        let cfg = cfg.prepare()?;
        Ok(Self {
            installations,
            leases,
            registry,
            handler,
            cfg,
            node_id: new_node_id(),
            entries: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
        })
    }

    /// 本进程的租约所有权令牌（运维把 DB 租约行对回运行的副本用）。
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// 配置快照。
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// 当前被监管的安装 id（**字典序**：诊断与测试都要确定性）。
    pub fn supervised(&self) -> Vec<Id> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ids: Vec<Id> = entries.keys().copied().collect();
        ids.sort_by_key(|id| id.as_string());
        ids
    }

    /// 起扫描任务并返回停机句柄：**立即**扫一遍（重启的服务器不该等满一个 `poll_interval`），
    /// 之后每 `poll_interval` 扫一遍。
    ///
    /// # Errors
    ///
    /// 配置非法（见 [`Supervisor::new`]）。
    pub fn spawn(self: Arc<Self>) -> ChannelResult<SupervisorHandle> {
        if let Err(error) = self.cfg.validate() {
            return Err(error.into_channel_error());
        }
        let supervisor = Arc::clone(&self);
        let task = tokio::spawn(async move {
            supervisor.sweep().await;
            let mut ticker = tokio::time::interval(supervisor.cfg.poll_interval);
            ticker.tick().await; // 第一次 tick 立即返回；首扫已经手动做过了。
            loop {
                ticker.tick().await;
                if supervisor.is_stopping() {
                    return;
                }
                supervisor.sweep().await;
            }
        });
        Ok(SupervisorHandle::new(self, task))
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// 停机：置停止位、通知每个监管任务，并**有界**地等它们退出。
    async fn stop_and_wait(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        {
            let entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for entry in entries.values() {
                let _ = entry.stop_tx.send(true);
            }
        }
        let deadline = tokio::time::Instant::now() + self.cfg.shutdown_timeout;
        while !self.supervised().is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let remaining = self.supervised().len();
        if remaining > 0 {
            tracing::warn!(
                remaining,
                "channel engine: supervisors did not stop within the shutdown timeout"
            );
        }
    }

    /// 一次扫描：枚举活跃安装 → 拆掉已消失的 → 起新的（租约不在别处）。
    async fn sweep(self: &Arc<Self>) {
        let rows = match self.installations.list_active().await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(
                    code = error.code_hint(),
                    "channel engine: list active installations failed"
                );
                return;
            }
        };

        let mut active: HashSet<Id> = HashSet::new();
        let mut candidates: Vec<Installation> = Vec::new();
        let mut rotations: Vec<Id> = Vec::new();
        for row in rows {
            // 没有注册工厂的 kind 直接跳过：否则每个 sweep 都会"取租约 → Build 失败 →
            // 释放 → 退避"，把租约与日志白白搅动一遍（上游同款守卫）。
            if self.registry.lookup(row.kind).is_none() {
                continue;
            }
            active.insert(row.id);
            if self.cancel_on_rotation(&row) {
                rotations.push(row.id);
            }
            if !self.is_supervised(row.id) {
                candidates.push(row);
            }
        }

        {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let gone: Vec<Id> = entries
                .keys()
                .copied()
                .filter(|id| !active.contains(id))
                .collect();
            for id in gone {
                if let Some(entry) = entries.remove(&id) {
                    let _ = entry.stop_tx.send(true);
                }
            }
        }

        if candidates.is_empty() {
            return;
        }
        // 先把换凭据的前任们等停：它们应当在同一个 sweep 里放开租约，
        // 让新连接立刻接上（否则要白等一个 TTL）。
        self.wait_for_rotations(&rotations).await;
        let ids: Vec<Id> = candidates.iter().map(|row| row.id).collect();
        let held = match self.leases.list_held(&ids).await {
            Ok(held) => held,
            Err(error) => {
                tracing::warn!(
                    code = error.code_hint(),
                    "channel engine: list held leases failed; acquisition sweep skipped"
                );
                return;
            }
        };
        for row in candidates {
            if held.contains(&row.id) {
                continue;
            }
            self.start_supervisor(row);
        }
    }

    fn is_supervised(&self, id: Id) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&id)
    }

    /// 凭据指纹变了 ⇒ 拆掉在跑的连接并返回 `true`（调用方随后等它放开租约）。
    fn cancel_on_rotation(&self, row: &Installation) -> bool {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rotate = entries
            .get(&row.id)
            .is_some_and(|entry| entry.fingerprint != row.fingerprint);
        if rotate {
            if let Some(entry) = entries.remove(&row.id) {
                tracing::info!(
                    installation_id = %row.id,
                    channel_type = row.kind.as_str(),
                    "channel engine: credentials rotated, restarting supervisor"
                );
                let _ = entry.stop_tx.send(true);
            }
        }
        rotate
    }

    /// 有界地等前任们**放开租约**。判据是租约而不是 map 条目：`cancel_on_rotation` 当场就把
    /// 条目摘了，条目状态不能证明前任已经收尾。
    async fn wait_for_rotations(&self, ids: &[Id]) {
        if ids.is_empty() {
            return;
        }
        let deadline = tokio::time::Instant::now() + self.cfg.rotation_wait_timeout;
        loop {
            match self.leases.list_held(ids).await {
                Ok(held) if held.is_empty() => return,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        code = error.code_hint(),
                        "channel engine: list held leases failed during rotation wait"
                    );
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    timeout = ?self.cfg.rotation_wait_timeout,
                    "channel engine: timed out waiting for rotated supervisors to release their leases"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    fn start_supervisor(self: &Arc<Self>, row: Installation) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (stop_tx, stop_rx) = watch::channel(false);
        {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries.contains_key(&row.id) {
                return;
            }
            entries.insert(
                row.id,
                Entry {
                    stop_tx,
                    generation,
                    fingerprint: row.fingerprint.clone(),
                },
            );
        }
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let outcome = supervisor.supervise(&row, generation, stop_rx).await;
            supervisor.finish_entry(row.id, generation, outcome);
        });
    }

    /// 清除 map 条目 —— `generation` 区分"这条是我"与"轮换路径已经换上了新任务"。
    fn finish_entry(&self, id: Id, generation: u64, outcome: EngineResult<()>) {
        {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries
                .get(&id)
                .is_some_and(|entry| entry.generation == generation)
            {
                entries.remove(&id);
            }
        }
        if let Err(error) = outcome {
            tracing::warn!(
                installation_id = %id,
                code = error.code_hint(),
                "channel engine: supervisor exited with error"
            );
        }
    }

    /// 一条安装的连接生命周期：取租约 → 造 → 跑 → 续租 → 退出即释放 + 退避 → 重复。
    async fn supervise(
        self: &Arc<Self>,
        row: &Installation,
        generation: u64,
        mut stop: watch::Receiver<bool>,
    ) -> EngineResult<()> {
        let token = lease_token(&self.node_id, generation);
        let mut backoff = Backoff::new(
            self.cfg.min_backoff,
            self.cfg.max_backoff,
            self.cfg.reset_backoff_after,
        );

        loop {
            if self.is_stopping() {
                return Ok(());
            }

            // 1. 取租约。拿不到 ⇒ 退出（下一轮 sweep 在租约消失后才重试，
            //    避免每个副本对每条安装盲目重试）。
            match self.try_acquire(row, &token).await {
                Ok(()) => {}
                Err(error) if is_lease_held(&error) => {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }

            // 2. 造 Channel（工厂拒配置 ⇒ 释放租约 + 退避重试，不是整个监管器死掉）。
            let channel = match self.registry.build(channel_config(row, &self.handler)) {
                Ok(channel) => channel,
                Err(error) => {
                    tracing::error!(
                        installation_id = %row.id,
                        channel_type = row.kind.as_str(),
                        code = error.code(),
                        "channel engine: build channel failed"
                    );
                    self.release(row.id, &token).await;
                    if sleep_cancellable(backoff.record_failure(), &mut stop).await {
                        return Ok(());
                    }
                    continue;
                }
            };

            // 3. 并行续租；租约丢 ⇒ 立即取消连接（否则会出现"两个副本同时消费同一条安装"）。
            let lease_lost = Arc::new(tokio::sync::Notify::new());
            let renew = tokio::spawn(renew_loop(
                Arc::clone(&self.leases),
                row.clone(),
                token.clone(),
                self.cfg.clone(),
                Arc::clone(&lease_lost),
                stop.clone(),
            ));

            let started = (self.cfg.now)();
            tokio::select! {
                biased;
                () = wait_stop(&mut stop) => {
                    tracing::info!(installation_id = %row.id, "channel engine: connection cancelled");
                }
                () = lease_lost.notified() => {
                    tracing::warn!(installation_id = %row.id, "channel engine: lease lost; tearing down connection");
                }
                result = channel.connect() => {
                    match result {
                        Ok(()) => tracing::info!(installation_id = %row.id, "channel engine: connection exited cleanly"),
                        Err(error) => {
                            let message = error.to_string();
                            tracing::warn!(installation_id = %row.id, error = %message, "channel engine: connection exited with error");
                        }
                    }
                }
            }
            renew.abort();
            let _ = renew.await;
            self.disconnect(&channel).await;
            self.release(row.id, &token).await;

            if self.is_stopping() {
                return Ok(());
            }

            let uptime = elapsed(&started, &(self.cfg.now)());
            backoff.record_uptime(uptime);
            if sleep_cancellable(backoff.record_failure(), &mut stop).await {
                return Ok(());
            }
        }
    }

    async fn try_acquire(&self, row: &Installation, token: &str) -> EngineResult<()> {
        let now = (self.cfg.now)();
        self.leases
            .try_acquire(AcquireLeaseParams {
                installation_id: row.id,
                kind: row.kind,
                token: token.to_string(),
                expires_at: plus(now, self.cfg.lease_ttl),
                ttl: self.cfg.lease_ttl,
            })
            .await
    }

    /// 释放跑在有界的新等待上（父上下文这时已经取消）：池子冻住不能拖死停机。
    async fn release(&self, id: Id, token: &str) {
        let service = Arc::clone(&self.leases);
        let params = ReleaseLeaseParams {
            installation_id: id,
            token: token.to_string(),
        };
        match tokio::time::timeout(self.cfg.lease_release_timeout, service.release(params)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(installation_id = %id, code = error.code_hint(), "channel engine: release lease failed");
            }
            Err(_) => {
                tracing::warn!(installation_id = %id, "channel engine: release lease timed out");
            }
        }
    }

    /// 断开（连接已经结束 ⇒ 尽力而为的资源清理），同样有界。
    async fn disconnect(&self, channel: &Arc<dyn Channel>) {
        match tokio::time::timeout(self.cfg.disconnect_timeout, channel.disconnect()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(error = %error, "channel engine: disconnect failed"),
            Err(_) => tracing::warn!("channel engine: disconnect timed out"),
        }
    }
}

#[cfg(test)]
mod tests;
