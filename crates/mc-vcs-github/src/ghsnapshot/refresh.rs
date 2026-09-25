//! PR 快照刷新的出站队列与后台宿主 —— 上游 `ghsnapshot/refresh.go`（562 行）
//! （M8-0 anchor 建桩，**M8-5 落地**）。
//!
//! # 这一片解决什么问题
//!
//! webhook 与页面访问只是**门铃**：它们不携带可用于展示的 CI / 可合并性信息（Plan C，
//! MUL-5265）。权威值只有一个来源 —— 这条管道用 installation token 打 GraphQL，把
//! `statusCheckRollup` 归一化后**原子地**写进 PR 行与逐 check 行（迁移 `222`/`223`）。
//!
//! 四件事（`docs/61` §2.6 / R-M8-5）：
//!
//! 1. **去重 + 单地址串行**：同一个 `(installation, owner, repo, number)` 同时**只有一单在飞**，
//!    在飞期间来的那一次留成一条 `trailing` 边，当前抓取结束后**回放一次**（换 head 时这条边
//!    很重要：旧 head 的响应被 head-SHA 守卫丢弃，新 head 立刻重抓）；
//! 2. **有界并发**：worker 池默认 12，队列有界（默认 2048，满了**丢弃并让 TTL sweep 兜底**，
//!    绝不阻塞调用方）；
//! 3. **三级退避**：`RateLimitError` ⇒ `rate_limit_pause`（installation 级暂停，**不占 worker**）
//!    / `defer_active`（暂停期间地址保持 active，重复事件继续合并）/ `schedule_retry`（chase 窗口）；
//! 4. **有界追捕**：快照未决（CI 还在跑 / 可合并性未知）时按 `30s → 1m → 2m → 5m → 5m…`
//!    重抓，最多 [`Tuning::max_chase_attempts`] 次；另有 TTL sweeper 兜底（默认 10 分钟一轮，
//!    只扫 open/draft 且陈旧且未决的行）。
//!
//! **无 Redis ⇒ 单副本部署契约**（与 M7 的 R-M7-1 同源，登记 `docs/32` §21.2）。
//!
//! # 宿主与停机链
//!
//! `Manager::start` 由 `apps/mc-server/src/integrations.rs` 调用；停机顺序固定为
//! 「先停渠道连接 → **再停 PR 刷新** → 再停调度器 → 最后停 actor」（`docs/61` §2.6）。
//! 停机后 worker 必须在 `Tuning::shutdown_grace`（默认 5s）内退出 —— 由 `Manager::shutdown`
//! 的 `timeout` + join 保证，并有一条真用例钉住它。
//!
//! # 三条**注入缝**（`DoD` 要求「注入 `Now`，不许 sleep 真实时间」）
//!
//! | 缝 | 默认 | 测试怎么用 |
//! | --- | --- | --- |
//! | `clock`（`() -> i64`，unix 秒） | [`system_now_unix`] | 推进假时钟 ⇒ 限流截止、view TTL 都可测 |
//! | `timer`（[`Timer`]） | [`TokioTimer`] | 记录型定时器：**捕获退避序列**（30s/1m/2m/5m…）并手动触发，零真实等待 |
//! | `fetcher`（[`SnapshotFetcher`]） | [`HttpSnapshotFetcher`] | 替身返回指定快照 / `RateLimited` / 错误 |
//!
//! # 存储是**端口**，宿主给实现（`docs/61` §3.3 的落点裁定）
//!
//! 本 crate 的依赖边**没有** `sqlx` / `mc-db`（anchor 冻结，`Cargo.toml` 逐字「此后
//! M8-1/4/5 的写者不得再新增三方依赖」）⇒ 四个存储操作（解析 installation、列地址下的 PR 行、
//! head-SHA 守卫的原子批次替换、TTL sweep 候选）由 [`SnapshotStore`] 定义，实现在
//! `apps/mc-server/src/integrations.rs`（直连 SQL，与 M5-9 的 `scheduler/*_port.rs` 同手法）。
//! 替身实现让管道本身可以在**零数据库**下逐条钉住。
//!
//! # 与上游形状的三处偏离（逐条登记 `docs/32` §21.2）
//!
//! | # | 偏离 | 理由 |
//! | --- | --- | --- |
//! | **D2** | 上游 `Enqueue(installationID, …)` 的定位键是**地址**；本仓端口（anchor 冻结形状）的定位键是 `(workspace_id, owner, repo, number)` ⇒ `installation_id` 只能在 **worker 里**从 PR 行解析（一次索引查询） | 端口形状不含 installation id；请求→地址的解析必须 async，而 `enqueue` 不得阻塞 ACK 路径。**执行侧的单地址串行与上游逐字相同**（`active` / `in_flight` / `trailing` 三个集合都按地址键控），只多了「同一地址的重复请求会各做一次解析查询」 |
//! | **D3** | 页面访问的 **view TTL 判定**（上游由 handler 传入 `fetchedAt` / `hasFetched`）挪到解析阶段做 | 端口形状不带这两个字段（M8-4 的 `issue_pr.rs` 只传 `PrRefreshRequest`）⇒ 只好在解析时顺带读一次 `snapshot_fetched_at`。语义等价：TTL 内**不抓** |
//! | **D4** | `Manager::start` 是**同步**的（anchor 桩写的是 `async fn start`）；`Manager::new` 多一个 `store` 实参 | 只 spawn worker + sweeper，没有可 `await` 的东西；而 anchor **冻结**的 `apps/mc-server/src/integrations.rs::start(keys)` 是同步函数 ⇒ 异步的 `start` 在同步装配点里只能再 spawn 一层（把启动错误变成不可见的日志）。多一个 `store` 实参是因为 [`SnapshotStore`] 是宿主的注入点 |
//!
//! # `pull_request:updated` 广播：**本片不接线**（登记缺口）
//!
//! 上游 `NewManager` 的 `onApplied` 回调让「快照真的写进去了」能广播一条 `pull_request:updated`
//! （M8-4 从 HTTP 层广播的是 webhook 镜像那条路径）。本片的宿主入口
//! `integrations::start(keys)` 拿不到 `realtime` 句柄（`main.rs` 是 anchor 冻结文件）⇒
//! [`Manager::with_options`] 里的 `on_applied` 缝留出来了（默认 `None`），由 M8-7 决定是否在
//! `main.rs` 里多传一个句柄。**在那之前**：页面访问触发的刷新写进库了，但客户端不会收到推送
//! （下次打开卡片才看到新值）—— 这是**登记过的缺口**，不是遗漏。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use mc_core::id::Id;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::ghsnapshot::Client;
use crate::port::PrRefreshRequest;
use crate::rest::GithubError;

mod ports;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use ports::{
    async_trait, Address, HttpSnapshotFetcher, ManagerOptions, PrRowRef, PrSnapshot,
    ResolvedTarget, SnapshotFetcher, SnapshotStore, StoreError, Timer, TokioTimer, Tuning,
};

/// 队列里的一件活：端口的请求（要先解析地址，偏离 D2）或一个**已知地址**（sweep / 回放）。
#[derive(Debug, Clone)]
enum Job {
    Request(PrRefreshRequest),
    Address(Address),
}

/// 端口的请求定位键（`installation_id` 不在里面 —— 见偏离 D2）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RequestKey {
    workspace_id: Id,
    owner: String,
    repo: String,
    number: i32,
}

impl RequestKey {
    fn of(request: &PrRefreshRequest) -> Self {
        Self {
            workspace_id: request.workspace_id,
            owner: request.repo_owner.clone(),
            repo: request.repo_name.clone(),
            number: request.pr_number,
        }
    }
}

/// 管道状态（一个 `Mutex` 收口；所有临界区都是**纯内存**、无 `.await`）。
#[derive(Debug, Default)]
struct State {
    /// 端口的入队去重：同一请求已在队列里（尚未解析）⇒ 不再入队。
    pending_requests: HashSet<RequestKey>,
    /// **排队中或在飞**的地址（上游 `active`）：入队与执行两侧的合并键。
    active: HashSet<Address>,
    /// 正在被某个 worker 处理的地址（上游 `inFlight`）。
    in_flight: HashSet<Address>,
    /// 在飞期间来的那一次（上游 `trailing`）：当前抓取结束后**回放一次**。
    trailing: HashSet<Address>,
    /// chase 计数（上游 `attempts`）。
    attempts: HashMap<Address, usize>,
    /// installation 级限流截止（unix 秒；上游 `rateUntil` 的 `time.Time`）。
    rate_until: HashMap<i64, i64>,
    /// 上一次 sweep 的末地址（上游 `sweepAfter`）：游标 + 回绕，一个固定的首页不会被饿死。
    sweep_after: Address,
}

struct Inner {
    client: Arc<Client>,
    store: Arc<dyn SnapshotStore>,
    options: ManagerOptions,
    queue_tx: mpsc::Sender<Job>,
    queue_rx: tokio::sync::Mutex<Option<mpsc::Receiver<Job>>>,
    state: Mutex<State>,
    cancel_tx: watch::Sender<bool>,
    /// ⚠️ **常驻接收者**：`watch::Sender::send` 在没有接收者时返回 `Err` 且**值不更新**
    /// （`docs/32` §19.5 的 lesson）⇒ 这里留一个，保证 `send(true)` 一定生效。
    cancel_keeper: watch::Receiver<bool>,
    started: AtomicBool,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Inner {
    /// 停机的**唯一判据**（定时器回调与 worker 都读它）。
    fn is_cancelled(&self) -> bool {
        *self.cancel_keeper.borrow()
    }

    /// 释放一个地址的全部记账（`active` / `in_flight` / `trailing`）。
    fn release(&self, address: &Address) {
        let mut state = lock(&self.state);
        state.active.remove(address);
        state.in_flight.remove(address);
        state.trailing.remove(address);
    }
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ghsnapshot::Manager")
            .field("client", &self.client)
            .field("concurrency", &self.options.tuning.concurrency)
            .field("started", &self.started.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// 后台刷新的宿主句柄（`start` 的返回值；`shutdown` 进停机链）。
///
/// 句柄是 `Clone` 的（内部 `Arc`）⇒ worker 与定时器回调都拿同一份状态，宿主侧仍可以自由
/// `clone` 一份给 `mc-http` 的 `set_pr_refresh_port`。
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl Manager {
    /// 构造（**不启动** worker）：注入客户端与存储端口，其余缝取生产默认值。
    #[must_use]
    pub fn new(client: Arc<Client>, store: Arc<dyn SnapshotStore>) -> Self {
        Self::with_options(client, store, ManagerOptions::default())
    }

    /// 构造（全缝可注入）。
    #[must_use]
    pub fn with_options(
        client: Arc<Client>,
        store: Arc<dyn SnapshotStore>,
        options: ManagerOptions,
    ) -> Self {
        let (queue_tx, queue_rx) = mpsc::channel(options.tuning.queue_capacity.max(1));
        let (cancel_tx, cancel_keeper) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                client,
                store,
                options,
                queue_tx,
                queue_rx: tokio::sync::Mutex::new(Some(queue_rx)),
                state: Mutex::new(State::default()),
                cancel_tx,
                cancel_keeper,
                started: AtomicBool::new(false),
                tasks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// 客户端（诊断/装配用）。
    #[must_use]
    pub fn client(&self) -> &Arc<Client> {
        &self.inner.client
    }

    /// 功能是否真的会做事（上游 `Enabled`：`client.Enabled()`）。缺 App 私钥 ⇒ `false`，
    /// 所有触发方法都退化为 no-op（**正常**路径，不是错误）。
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.inner.client.enabled()
    }

    /// 启动 worker 池 + TTL sweeper。重复调用是 no-op（上游 `started` 标志）。
    ///
    /// # Errors
    ///
    /// 当前**没有**失败路径（`tokio::spawn` 不返回 `Result`）；保留 `Result` 是为了对齐
    /// anchor 的既有形状，以及将来真正的运行时初始化（专用线程池等）。
    pub fn start(&self) -> Result<(), GithubError> {
        if !self.enabled() {
            return Ok(());
        }
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let mut tasks = lock(&self.inner.tasks);
        for worker in 0..self.inner.options.tuning.concurrency {
            let manager = self.clone();
            let cancel = self.inner.cancel_keeper.clone();
            tasks.push(tokio::spawn(async move {
                manager.worker_loop(worker, cancel).await;
            }));
        }
        let manager = self.clone();
        let cancel = self.inner.cancel_keeper.clone();
        tasks.push(tokio::spawn(async move {
            manager.sweep_loop(cancel).await;
        }));
        Ok(())
    }

    /// 停机（`apps/mc-server` 的停机链调用）：置停机标志，然后等 worker 在
    /// `Tuning::shutdown_grace` 内退出。幂等。
    pub async fn shutdown(&self) {
        // `send` 需要至少一个接收者 —— `cancel_keeper` 就是那个常驻接收者。
        let _ = self.inner.cancel_tx.send(true);
        let tasks = std::mem::take(&mut *lock(&self.inner.tasks));
        if tasks.is_empty() {
            return;
        }
        let grace = self.inner.options.tuning.shutdown_grace;
        let joined = tokio::time::timeout(grace, join_all(tasks)).await;
        if joined.is_err() {
            tracing::warn!(
                grace_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX),
                "ghsnapshot: workers did not stop within the shutdown grace window"
            );
        }
    }

    /// 入队一个**已知地址**（TTL sweep 与 trailing 回放用）—— 上游 `Enqueue`。
    ///
    /// **绝不阻塞调用方**：队列满 ⇒ 丢弃并留给 TTL sweep / 下一次事件兜底；停机后是 no-op。
    pub fn enqueue_address(&self, address: Address) {
        if !self.enabled() || self.inner.is_cancelled() {
            return;
        }
        {
            let mut state = lock(&self.inner.state);
            if state.active.contains(&address) {
                // 已在排队或在飞：在飞时留一条 trailing 边（上游语义）。
                if state.in_flight.contains(&address) {
                    state.trailing.insert(address);
                }
                return;
            }
            state.active.insert(address.clone());
        }
        if self
            .inner
            .queue_tx
            .try_send(Job::Address(address.clone()))
            .is_err()
        {
            self.inner.release(&address);
            tracing::warn!("ghsnapshot: refresh queue full, dropping enqueue");
        }
    }

    /// 入队一个**端口请求**（`workspace_id` 定位；地址在 worker 里解析 —— 偏离 D2）。
    ///
    /// 返回 `true` = 已受理入队。**不是**「一定会抓」：解析阶段可能发现没有镜像行、或页面访问的
    /// view TTL 还没过（偏离 D3），那时这一次请求被丢弃（上游 `MaybeEnqueueOnView` 同判）。
    /// 停机后恒 `false`。
    pub fn enqueue_request(&self, request: &PrRefreshRequest) -> bool {
        if !self.enabled() || self.inner.is_cancelled() {
            return false;
        }
        let key = RequestKey::of(request);
        {
            let mut state = lock(&self.inner.state);
            if !state.pending_requests.insert(key.clone()) {
                return false;
            }
        }
        if self
            .inner
            .queue_tx
            .try_send(Job::Request(request.clone()))
            .is_err()
        {
            lock(&self.inner.state).pending_requests.remove(&key);
            tracing::warn!("ghsnapshot: refresh queue full, dropping enqueue");
            return false;
        }
        true
    }

    /// 页面访问触发的**有节制**入队（上游 `MaybeEnqueueOnView`）：同一请求已在排队 ⇒ `false`。
    ///
    /// ⚠️ view TTL 判定（上游由 handler 传入 `fetchedAt` / `hasFetched`）在解析阶段做
    /// —— 端口形状不带这两个字段（偏离 D3）⇒ 这里返回 `true` 只表示「受理了」。
    pub fn maybe_enqueue_request_on_view(&self, request: &PrRefreshRequest) -> bool {
        self.enqueue_request(request)
    }

    /// 跑一轮 TTL sweep（上游 `sweepOnce`）：把「陈旧**且**未决」的 open/draft PR 地址入队。
    ///
    /// 本轮批次的末地址写进游标，下一轮从它之后开始（并回绕）⇒ 早期反复失败的地址不会永远
    /// 占着首页（上游逐字）。`sweep_loop` 是它的定时壳子；单测直接调本函数。
    pub async fn sweep_once(&self) {
        if !self.enabled() {
            return;
        }
        let now = (self.inner.options.clock)();
        let after = lock(&self.inner.state).sweep_after.clone();
        let older_than = now - self.inner.options.tuning.sweep_ttl_secs;
        let rows = match self
            .inner
            .store
            .list_stale_undecided(older_than, &after, self.inner.options.tuning.sweep_max_rows)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "ghsnapshot: sweep query failed");
                return;
            }
        };
        if let Some(last) = rows.last() {
            lock(&self.inner.state).sweep_after = last.clone();
        }
        for address in rows {
            self.enqueue_address(address);
        }
    }

    // ── 内部：两个循环 ───────────────────────────────────────────────────────

    async fn sweep_loop(&self, mut cancel: watch::Receiver<bool>) {
        let interval = self.inner.options.tuning.sweep_interval;
        loop {
            if *cancel.borrow() {
                return;
            }
            tokio::select! {
                _ = cancel.changed() => return,
                () = tokio::time::sleep(interval) => self.sweep_once().await,
            }
        }
    }

    async fn worker_loop(&self, _worker: usize, mut cancel: watch::Receiver<bool>) {
        loop {
            if *cancel.borrow() {
                return;
            }
            let job = {
                let mut guard = self.inner.queue_rx.lock().await;
                let Some(receiver) = guard.as_mut() else {
                    return;
                };
                tokio::select! {
                    _ = cancel.changed() => return,
                    job = receiver.recv() => match job {
                        Some(job) => job,
                        None => return,
                    },
                }
            };
            if *cancel.borrow() {
                self.discard_job(job);
                return;
            }
            self.handle_job(job, &cancel).await;
        }
    }

    /// 一件活：解析（若需要）→ 限流门 → 单地址串行 → 抓取 → 写 → 收口。
    async fn handle_job(&self, job: Job, cancel: &watch::Receiver<bool>) {
        let Some(address) = self.resolve_job(&job).await else {
            return;
        };
        // 限流门：被暂停的 installation 在 worker 池**外面**等（上游 `worker` 的 pause 分支）
        // —— 暂停期间地址保持 active，重复事件继续合并，一个租户不会占满全局 worker。
        let pause = self.rate_limit_pause(address.installation_id);
        if pause > 0 {
            self.defer_active(address, pause);
            return;
        }
        {
            let mut state = lock(&self.inner.state);
            if state.in_flight.contains(&address) {
                // 同一地址已有 worker 在跑 ⇒ 合并成一条 trailing 边（绝不并发抓同一个地址）。
                state.trailing.insert(address);
                return;
            }
            state.in_flight.insert(address.clone());
        }
        let jitter = (self.inner.options.jitter)();
        if !jitter.is_zero() && !sleep_or_cancel(jitter, cancel).await {
            self.inner.release(&address);
            return;
        }
        self.process(&address).await;
        self.finish(&address);
    }

    /// 把一件活解析成一个地址。返回 `None` = 这一次请求被丢弃（并已清理状态）。
    async fn resolve_job(&self, job: &Job) -> Option<Address> {
        let address = match job {
            Job::Address(address) => address.clone(),
            Job::Request(request) => {
                let key = RequestKey::of(request);
                lock(&self.inner.state).pending_requests.remove(&key);
                let resolved = self
                    .inner
                    .store
                    .resolve_installation(
                        request.workspace_id,
                        &request.repo_owner,
                        &request.repo_name,
                        request.pr_number,
                    )
                    .await;
                match resolved {
                    Ok(Some(target)) => {
                        // 页面访问的 view TTL（偏离 D3）：比 TTL 新就不抓。
                        if request.reason == crate::port::RefreshReason::PageView {
                            let now = (self.inner.options.clock)();
                            if let Some(fetched_at) = target.snapshot_fetched_at {
                                if now - fetched_at < self.inner.options.tuning.view_ttl_secs {
                                    return None;
                                }
                            }
                        }
                        Address {
                            installation_id: target.installation_id,
                            owner: request.repo_owner.clone(),
                            repo: request.repo_name.clone(),
                            number: request.pr_number,
                        }
                    }
                    Ok(None) => return None,
                    Err(error) => {
                        tracing::warn!(%error, "ghsnapshot: resolve address failed");
                        return None;
                    }
                }
            }
        };
        Some(address)
    }

    /// 一次抓取 + 扇出写。**失败一律保留上一条快照**（卡片宁可显示陈旧的真值，不显示错的）。
    async fn process(&self, address: &Address) {
        let now = (self.inner.options.clock)();
        let snapshot = match self
            .inner
            .options
            .fetcher
            .fetch(&self.inner.client, address, now)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(GithubError::RateLimited { retry_after_secs }) => {
                // installation 级暂停：**不**建无界重试环，交回 TTL sweep / 下一次事件。
                self.extend_rate_limit(address.installation_id, retry_after_secs);
                return;
            }
            Err(error) => {
                // 不回显任何凭据：`GithubError` 只带原因名与状态码（`docs/61` §2.4）。
                tracing::warn!(
                    owner = %address.owner,
                    repo = %address.repo,
                    number = address.number,
                    error = %error,
                    "ghsnapshot: fetch failed"
                );
                return;
            }
        };
        let rows = match self.inner.store.list_rows(address).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "ghsnapshot: list rows failed");
                return;
            }
        };
        let mut any_applied = false;
        let mut any_open_applied = false;
        for row in rows {
            match self
                .inner
                .store
                .apply_snapshot(row.id, &snapshot, now)
                .await
            {
                Ok(true) => {
                    any_applied = true;
                    if row.is_open_or_draft() {
                        any_open_applied = true;
                    }
                    if let Some(on_applied) = &self.inner.options.on_applied {
                        on_applied(row.id);
                    }
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "ghsnapshot: apply snapshot failed"),
            }
        }
        // 只在「未决 **且** 真的有一条 open/draft 行被写」时追捕：head 前进由新 head 的
        // webhook 接手；行没了就没有可刷的东西；副本延迟漏掉的行由 TTL sweep 兜底。
        if any_applied && any_open_applied && !snapshot.decided() {
            self.schedule_chase(address);
        } else {
            lock(&self.inner.state).attempts.remove(address);
        }
    }

    // ── 内部：状态收口 ───────────────────────────────────────────────────────

    /// 一件活跑完：把一条合并进来的 trailing 边变成下一次入队，否则释放地址。
    fn finish(&self, address: &Address) {
        let replay = {
            let mut state = lock(&self.inner.state);
            state.in_flight.remove(address);
            state.trailing.remove(address)
        };
        if replay {
            // `active` 保持置位（回放期间继续合并重复事件），直接把地址推回队列。
            if self
                .inner
                .queue_tx
                .try_send(Job::Address(address.clone()))
                .is_ok()
            {
                return;
            }
            tracing::warn!("ghsnapshot: refresh queue full, dropping trailing enqueue");
        }
        self.inner.release(address);
    }

    /// 停机时把一件已取出的活直接丢掉（不再重排任何东西）。
    fn discard_job(&self, job: Job) {
        match job {
            Job::Request(request) => {
                lock(&self.inner.state)
                    .pending_requests
                    .remove(&RequestKey::of(&request));
            }
            Job::Address(address) => self.inner.release(&address),
        }
    }

    /// 限流暂停剩余秒数（上游 `rateLimitPause`）：`<= 0` ⇒ 顺手清掉这一条。
    fn rate_limit_pause(&self, installation_id: i64) -> i64 {
        let mut state = lock(&self.inner.state);
        let Some(until) = state.rate_until.get(&installation_id).copied() else {
            return 0;
        };
        let pause = until - (self.inner.options.clock)();
        if pause <= 0 {
            state.rate_until.remove(&installation_id);
            return 0;
        }
        pause
    }

    /// 延长 installation 的暂停截止（上游 `extendRateLimit`：**只延不缩**）。
    fn extend_rate_limit(&self, installation_id: i64, retry_after_secs: i64) {
        let until = (self.inner.options.clock)() + retry_after_secs;
        let mut state = lock(&self.inner.state);
        let entry = state.rate_until.entry(installation_id).or_insert(i64::MIN);
        if until > *entry {
            *entry = until;
        }
    }

    /// 被限流的地址交回队列（上游 `deferActive`）：地址在延迟期间保持 `active`，
    /// **不占 worker**；延迟到点后重新入队，队列满或停机则释放。
    fn defer_active(&self, address: Address, delay_secs: i64) {
        let inner = self.inner.clone();
        let delay = Duration::from_secs(u64::try_from(delay_secs).unwrap_or(0));
        self.inner.options.timer.schedule(
            delay,
            Box::new(move || {
                if inner.is_cancelled() {
                    inner.release(&address);
                    return;
                }
                if inner
                    .queue_tx
                    .try_send(Job::Address(address.clone()))
                    .is_err()
                {
                    inner.release(&address);
                    tracing::warn!("ghsnapshot: refresh queue full, dropping rate-limited enqueue");
                }
            }),
        );
    }

    /// chase：按当前退避档位重排一次；到上限或退避序列为空则**停止并清账**
    /// （上游 `scheduleChase`）。
    fn schedule_chase(&self, address: &Address) {
        let tuning = self.inner.options.tuning.clone();
        if tuning.chase_backoff.is_empty() || tuning.max_chase_attempts == 0 {
            lock(&self.inner.state).attempts.remove(address);
            return;
        }
        let attempt = {
            let mut state = lock(&self.inner.state);
            let attempt = state.attempts.get(address).copied().unwrap_or(0);
            if attempt >= tuning.max_chase_attempts {
                state.attempts.remove(address);
                return;
            }
            state.attempts.insert(address.clone(), attempt + 1);
            attempt
        };
        let index = attempt.min(tuning.chase_backoff.len() - 1);
        let delay = tuning.chase_backoff[index];
        self.schedule_retry(address, delay);
    }

    /// 延迟重排一次（上游 `scheduleRetry`）；停机后**不再**排。
    fn schedule_retry(&self, address: &Address, delay: Duration) {
        let manager = self.clone();
        let address = address.clone();
        self.inner.options.timer.schedule(
            delay,
            Box::new(move || {
                if manager.inner.is_cancelled() {
                    return;
                }
                manager.enqueue_address(address);
            }),
        );
    }

    // ── 内部：测试可见的记账读数（`DoD` 的「worker 池并发上限 + TTL sweep 单地址串行」） ──

    /// 排队中的活数量（`capacity - 剩余容量`）—— 只用于「入队后还没起 worker」的断言。
    #[cfg(test)]
    fn queued(&self) -> usize {
        self.inner
            .options
            .tuning
            .queue_capacity
            .saturating_sub(self.inner.queue_tx.capacity())
    }

    /// 停机后 worker 句柄是否已全部收口（`DoD` 的停机链断言）。
    #[cfg(test)]
    fn workers_joined(&self) -> bool {
        lock(&self.inner.tasks).is_empty()
    }

    #[cfg(test)]
    fn pending_requests(&self) -> usize {
        lock(&self.inner.state).pending_requests.len()
    }

    #[cfg(test)]
    fn active_addresses(&self) -> usize {
        lock(&self.inner.state).active.len()
    }

    /// `(active, in_flight, trailing, attempts)` —— 四个集合的观测口。
    #[cfg(test)]
    fn chart(&self, address: &Address) -> (bool, bool, bool, usize) {
        let state = lock(&self.inner.state);
        (
            state.active.contains(address),
            state.in_flight.contains(address),
            state.trailing.contains(address),
            state.attempts.get(address).copied().unwrap_or(0),
        )
    }
}

// `enqueue_address` / `enqueue_request` 供宿主与定时器用（`Manager` 是 `Clone` 的）；
// `impl PrRefreshPort for Manager` 落在 `crate::port`（anchor 指定的位置；见 `docs/32` §21.1）。

/// 系统时钟（unix 秒）。
#[must_use]
pub fn system_now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// 上游 `jitter`：`[0, 250ms)`。
///
/// 不用 rand crate（本 crate 没有这条依赖边）；用系统时间的亚秒位做廉价抖动 —— 目的只是
/// **打散突发**，不是密码学随机。
#[must_use]
pub fn default_jitter() -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    Duration::from_nanos(u64::from(nanos % 250_000_000))
}

/// `Mutex` 中毒 ⇒ 取回内层值（本仓惯例：记账状态的临界区不 panic —— 毒化只是上一次 panic
/// 的残留，不该让整条管道停摆）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 睡 `delay` 或直到停机；返回 `false` = 停机打断了这次睡眠。
async fn sleep_or_cancel(delay: Duration, cancel: &watch::Receiver<bool>) -> bool {
    let mut cancel = cancel.clone();
    tokio::select! {
        _ = cancel.changed() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// 无 `futures` 依赖边 ⇒ 手写一个极小的 `join_all`（只用于停机收口，顺序确定）。
async fn join_all(tasks: Vec<JoinHandle<()>>) {
    for task in tasks {
        // 任一 worker 卡住时，外层的 `timeout` 兜住整个收口。
        let _ = task.await;
    }
}

/// 编译期形状断言：`Manager` 必须能装进 `Arc<dyn PrRefreshPort>`（宿主注入槽的类型）。
const _: fn(Manager) -> std::sync::Arc<dyn crate::port::PrRefreshPort> =
    |manager| std::sync::Arc::new(manager);
