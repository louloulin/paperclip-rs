//! M5-D8（`LUM-1745`）：webhook **投递 worker** 的轮询循环宿主 —— `1s` ticker + `Notify`
//! + 4 并发 + 优雅停机。
//!
//! # 这一片补的是哪条掉棒
//!
//! `/api/webhooks/**` 的**入站 → 落库**（M5-5）与**认领一条 → 推到终态**
//! （[`WebhookIngress::process_next_delivery`]）都已合入，但把这两步连起来的**轮询循环**从未被
//! 任何切片接收（`docs/44` §? 的裁定落在文档里、没落进 `LUM-1659` 的描述）⇒ 生产路径上
//! `process_next_delivery*` **一个调用点都没有**，入站落下的 `queued` 行除测试外永远不被消费。
//! 本文件就是那个调用点。
//!
//! # 上游映射（`internal/handler/webhook_delivery_worker.go`，301 行，@ `90e0bdf`）
//!
//! | 上游 | 本地 |
//! | --- | --- |
//! | `webhookWorkerPollInterval = time.Second` | [`POLL_INTERVAL`] |
//! | `webhookWorkerConcurrency = 4` | [`CONCURRENCY`] |
//! | `Run(ctx)`：4 个 goroutine 各跑 `runLoop` | [`start_with`] 起 [`CONCURRENCY`] 个 [`run_loop`] |
//! | `runLoop`：先 `ProcessNext`，`worked == false` 才 `select { ctx.Done / notify / ticker }` | [`run_loop`] 的「先 [`process_once`] 再 `select!`」三段 |
//! | **每个 loop 自带 ticker** | 每个 loop 一个 `tokio::time::interval` |
//! | `notify chan struct{}`（cap = 并发）+ `Notify()` 非阻塞送 | [`NotifyPort`]（每 loop 一条 cap-1 槽，`try_send`）+ [`WebhookNotify`] |
//! | `WaitWithTimeout(5s)` | [`WebhookWorkerHandles::shutdown`]（[`SHUTDOWN_TIMEOUT`]） |
//! | `cmd/server/main.go:744` `go ….Run(sweepCtx)` / `:890` `WaitWithTimeout` | `main.rs` 起停接线（停机链第 3 段） |
//!
//! # 语义要点（**照抄，别改写**）
//!
//! 队列与租约都在 Postgres，进程重启 / 副本切换只是重新认领过期行；**内存通知只是延迟提示**
//! ⇒ 「不 `Notify` 也能靠 `1s` ticker 推动」是**设计**，不是降级。本文件因此让 ticker 全权
//! 负责「会不会被消费」，`Notify` 只负责「多快被消费」。
//!
//! # 一处本地偏差（登记 `docs/32` §27）
//!
//! 上游 `ProcessNext(ctx)` 的 `ctx` 取消会让正在飞的 pgx 查询立刻报错；本地
//! `process_next_delivery()` **没有** `ctx` 参数（它的签名归 M5-5，本片不改）⇒ 停机时**正在收口
//! 的那一条会跑完**，由 [`shutdown`][WebhookWorkerHandles::shutdown] 的 5s 上限兜住（超时则
//! `abort_all`，留下的最多是「已认领、未收口」的一条 —— 租约 2 分钟后过期，由下一个进程
//! reclaim，正是上游的既有语义）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mc_autopilot::webhook::{SharedWebhookNotify, WebhookIngress, WebhookNotify};
use mc_db::Db;
use mc_realtime::RealtimeHandle;
use sqlx::postgres::PgPool;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;

/// 轮询间隔（上游 `webhookWorkerPollInterval`）。
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// worker 池大小（上游 `webhookWorkerConcurrency`）。**也是提示槽的总容量。**
pub const CONCURRENCY: usize = 4;

/// 停机等待上限（上游 `WaitWithTimeout(5 * time.Second)`，`cmd/server/main.go:890`）。
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// 每个 loop 的提示槽容量（见 [`NotifyPort`]：`CONCURRENCY × 本条` = 上游那条 chan 的 `cap`）。
const NOTIFY_SLOT_CAPACITY: usize = 1;

/// 循环的可调参数。**确定性接缝**：用例要一个「只有 `Notify` 才推得动」的长间隔，生产用
/// [`Options::default`]（[`POLL_INTERVAL`] / [`CONCURRENCY`]）。
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// 每个 loop 自带的 ticker 间隔。
    pub poll_interval: Duration,
    /// 池里 loop 的条数（`0` 会被抬到 `1`，见 [`start_with`]）。
    pub concurrency: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            poll_interval: POLL_INTERVAL,
            concurrency: CONCURRENCY,
        }
    }
}

/// 池的运行读数。全是单调计数器（`peak_in_flight` 除外，它是**最大**计数器）——
/// 它们只喂日志与用例，不参与任何判定。
#[derive(Debug, Default)]
struct Stats {
    /// 此刻正在 `process_next_delivery` 里的 loop 条数。
    in_flight: AtomicUsize,
    /// [`Self::in_flight`] 的历史峰值（`DoD` 3：并发 = [`CONCURRENCY`]）。
    peak_in_flight: AtomicUsize,
    /// 被推到终态 / 推迟的投递条数（上游 `worked == true` 的次数）。
    processed: AtomicUsize,
    /// 「队列空」的次数。
    empty_polls: AtomicUsize,
    /// `process_next_delivery` 报错的次数。
    failed_polls: AtomicUsize,
}

impl Stats {
    /// 进入一次认领：`in_flight + 1` 并抬高峰值。
    fn enter(&self) -> usize {
        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(in_flight, Ordering::SeqCst);
        in_flight
    }

    /// 离开一次认领。
    fn leave(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 提示口的实现：**每个 loop 一条专属的 cap-1 通道**。
///
/// 上游是**一条** `cap = 并发` 的 buffered chan，`Notify()` 非阻塞送一枚 token、满则丢
/// （`webhook_delivery_worker.go:42`）。本地换成「每 loop 一条 cap-1 通道 + 逐条 `try_send`」：
///
/// - **总容量仍是并发数**（`4 × 1`），满了就 `Err`（丢弃，既不阻塞也不 panic）—— 上游那个
///   `select { case w.notify <- struct{}{}: default: }` 的两条性质**逐条对得上**；
/// - **一次提示扇到全部 loop**（上游那条共享 chan 的一次 `Notify()` 只唤醒其中一个）——
///   只是延迟更短，不改变任何正确性（提示本来就是「可以丢」的）。
///
/// ⚠️ **不能**把上游那条 chan 直接搬过来：`mpsc::Receiver` 要独占，四个 loop 共享它必须加锁，
/// 而**加了锁就没法等自己那拍 ticker 了**（`select!` 里 `rx.lock()` 会把另外三个 loop 一起
/// 挡在锁上、它们的 ticker 永远轮不到）—— 那会直接破坏 `DoD` 2（「不调 `Notify()` 也能在
/// `1s` 量级内被消费」）。
struct NotifyPort {
    slots: Vec<mpsc::Sender<()>>,
}

impl WebhookNotify for NotifyPort {
    fn notify(&self) {
        for slot in &self.slots {
            // 满 = 那个 loop 已经醒着、或还没消费完上一枚 ⇒ **丢弃这一枚**（上游 `default:` 同款）。
            let _ = slot.try_send(());
        }
    }
}

/// 建出提示口与每个 loop 各一条接收端。
fn notify_slots(concurrency: usize) -> (SharedWebhookNotify, Vec<mpsc::Receiver<()>>) {
    let mut slots = Vec::with_capacity(concurrency);
    let mut receivers = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let (sender, receiver) = mpsc::channel(NOTIFY_SLOT_CAPACITY);
        slots.push(sender);
        receivers.push(receiver);
    }
    (Arc::new(NotifyPort { slots }), receivers)
}

/// 一个 loop 的全部句柄。
struct LoopContext {
    /// 认领 + 收口（`process_next_delivery` 在它里面）。
    ingress: Arc<WebhookIngress>,
    /// 共享读数。
    stats: Arc<Stats>,
    /// 停止置位（`watch` = 「一次置位、四个 loop 各收一份」）。
    shutdown: watch::Receiver<bool>,
    /// 本 loop 的提示槽。
    slot: mpsc::Receiver<()>,
    /// ticker 间隔。
    poll_interval: Duration,
}

/// worker 池的句柄：起停 + 诊断读数。
pub struct WebhookWorkerHandles {
    /// 停止置位端；`take` 过 = 已停。
    shutdown: Option<watch::Sender<bool>>,
    /// **常驻接收者**：`watch::Sender::send` 在**无接收者**时返回 `Err` 且**值不更新**（本仓
    /// 已踩过的坑）⇒ 句柄自己留一份，保证 [`Self::shutdown`] 的置位一定落到通道上。
    _keepalive: watch::Receiver<bool>,
    /// 池里的 loop。用 `JoinSet` 而不是 `Vec<JoinHandle>`：超时后要能 `abort_all`。
    loops: JoinSet<()>,
    /// 提示口（同一个 `Arc` 既喂池、也注进 `mc-http` 的进程级槽）。
    port: SharedWebhookNotify,
    /// 共享读数。
    stats: Arc<Stats>,
    /// 起手参数。
    options: Options,
}

impl WebhookWorkerHandles {
    /// 提示口（**注入给 `mc-http` 的就是它**；用例也用它，免得碰进程级槽）。
    pub fn notify_port(&self) -> SharedWebhookNotify {
        Arc::clone(&self.port)
    }

    /// 池里 loop 条数。
    pub fn concurrency(&self) -> usize {
        self.options.concurrency
    }

    /// 每个 loop 的 ticker 间隔。
    pub fn poll_interval(&self) -> Duration {
        self.options.poll_interval
    }

    /// 此刻在飞的认领条数。
    pub fn in_flight(&self) -> usize {
        self.stats.in_flight.load(Ordering::SeqCst)
    }

    /// 在飞认领的历史峰值（**应恒 `<= concurrency()`**）。
    pub fn peak_in_flight(&self) -> usize {
        self.stats.peak_in_flight.load(Ordering::SeqCst)
    }

    /// 已被推到终态 / 推迟的投递条数。
    pub fn processed(&self) -> usize {
        self.stats.processed.load(Ordering::SeqCst)
    }

    /// 「队列空」的轮数。
    pub fn empty_polls(&self) -> usize {
        self.stats.empty_polls.load(Ordering::SeqCst)
    }

    /// 报错的轮数。
    pub fn failed_polls(&self) -> usize {
        self.stats.failed_polls.load(Ordering::SeqCst)
    }

    /// 优雅停机：置位 → 等所有 loop 自己在 [`SHUTDOWN_TIMEOUT`] 内退出；超时则 `abort_all`。
    ///
    /// 上游是 `WaitWithTimeout(5 * time.Second)`（同一条上限）。**不留半截租约**这件事不由这里
    /// 保证 —— 它由「租约 2 分钟过期 + 下一个进程 reclaim」那条既有语义保证（见
    /// [`mc_repos::autopilot::ingress::claim_queued`] 的文档）。
    pub async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            // 常驻接收者保证这次置位一定落地（见 `_keepalive` 的字段文档）。
            let _ = shutdown.send(true);
        }
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, drain(&mut self.loops))
            .await
            .is_err()
        {
            // 超时：剩下的 loop 直接中止。⚠️ 被中止的 loop 可能正卡在一次认领里 ⇒ 那一条投递
            // 的租约到期后由下一个进程 reclaim（上游 `ctx` 取消同款语义）。
            self.loops.abort_all();
            while self.loops.join_next().await.is_some() {}
            tracing::warn!(
                timeout_ms = u64::try_from(SHUTDOWN_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
                "webhook worker: shutdown timed out; aborted the remaining loops"
            );
        }
        tracing::info!(
            processed = self.processed(),
            empty_polls = self.empty_polls(),
            failed_polls = self.failed_polls(),
            in_flight = self.in_flight(),
            peak_in_flight = self.peak_in_flight(),
            "webhook delivery worker stopped"
        );
    }
}

/// 等池里所有 loop 自己退出（`shutdown` 已置位 ⇒ 每个 loop 最多再收口**一条**投递）。
async fn drain(loops: &mut JoinSet<()>) {
    while let Some(joined) = loops.join_next().await {
        if let Err(error) = joined {
            tracing::error!(error = %error, "webhook worker: loop ended abnormally");
        }
    }
}

/// 装配并启动投递 worker 池（`main.rs` 在**调度器之后**调用；上游 `cmd/server/main.go:744`）。
///
/// 与渠道宿主 / 集成宿主不同，这里**没有装配判据**：上游无条件起它（队列是常量表、worker 是
/// 它唯一的消费者），所以本地也无条件起。
pub fn start(db: &Db, realtime: RealtimeHandle) -> WebhookWorkerHandles {
    start_with(db.pool().clone(), realtime, Options::default())
}

/// 与 [`start`] 同，但参数由调用方给（**确定性接缝**：真库用例要一个「只有 `Notify` 才推得动」
/// 的长 ticker 间隔）。
///
/// `concurrency` 至少为 1（`0` 的池没有任何消费者，那是个静默失效，不给这个口子）。
///
/// 起池之后**立刻**把池自己的提示口写进 `mc-http` 的进程级槽
/// （`routes/webhooks/autopilots.rs::set_webhook_notify_port`）：入站面（`routes/webhooks`）与
/// replay 路由（`routes/autopilots/delivery.rs`）都读它。⚠️ 槽是**按请求读**的 ⇒ `main.rs`
/// 先建 router、后起本宿主也不影响生效（与 M8-2 的快照刷新槽同款）。
pub(crate) fn start_with(
    pool: PgPool,
    realtime: RealtimeHandle,
    options: Options,
) -> WebhookWorkerHandles {
    let concurrency = options.concurrency.max(1);
    let options = Options {
        concurrency,
        ..options
    };
    let (port, receivers) = notify_slots(concurrency);
    let ingress = Arc::new(WebhookIngress::new(pool).with_events(realtime));
    let stats = Arc::new(Stats::default());
    let (shutdown, watch_rx) = watch::channel(false);

    let mut loops = JoinSet::new();
    for slot in receivers {
        loops.spawn(run_loop(LoopContext {
            ingress: Arc::clone(&ingress),
            stats: Arc::clone(&stats),
            shutdown: watch_rx.clone(),
            slot,
            poll_interval: options.poll_interval,
        }));
    }

    let handles = WebhookWorkerHandles {
        shutdown: Some(shutdown),
        _keepalive: watch_rx,
        loops,
        port,
        stats,
        options,
    };
    mc_http::routes::webhooks::autopilots::set_webhook_notify_port(handles.notify_port());
    tracing::info!(
        concurrency = handles.concurrency(),
        poll_interval_ms = u64::try_from(handles.poll_interval().as_millis()).unwrap_or(u64::MAX),
        "webhook delivery worker started (ticker + notify + concurrent loops)"
    );
    handles
}

/// 一个 loop：先干活，干到队列空才等「停机 / 本 loop 的提示 / 本 loop 的 ticker」。
///
/// 三段与上游 `runLoop` 逐条对应：
/// ① `ProcessNext`（[`process_once`]）；② `worked == true` ⇒ 立刻再来一条（**不睡**）；
/// ③ 否则 `select { ctx.Done / notify / ticker }`。
async fn run_loop(mut ctx: LoopContext) {
    let mut ticker = tokio::time::interval(ctx.poll_interval);
    // 缺拍时**不补跑**：补跑会把「积压的 tick」变成一串立即返回的 `ProcessNext`，
    // 而上游每个 loop 的 ticker 语义是「等下一拍」。
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        // 停机检查在**每一轮开头**：`process_next_delivery` 内部有多次 DB 往返、不可中断
        // （它没有 `ctx` 参数）⇒ 正在收口的那一条会跑完，由 5s 上限兜住（见模块头「本地偏差」）。
        if *ctx.shutdown.borrow() {
            return;
        }
        if process_once(&ctx).await {
            // 上游：`worked == true` ⇒ `continue`，立刻再认领一条。
            continue;
        }
        tokio::select! {
            // 停机优先：置位后不再等一拍 ticker。
            biased;
            _ = ctx.shutdown.changed() => return,
            // 本 loop 的提示槽（容量 1；满则由投递方丢弃 —— 上游 `default:` 分支同款）。
            _ = ctx.slot.recv() => {}
            _ = ticker.tick() => {}
        }
    }
}

/// 认领并收口**一条**到期投递。返回 `true` = 处理过一条（上游 `worked`）。
async fn process_once(ctx: &LoopContext) -> bool {
    ctx.stats.enter();
    let claimed = ctx.ingress.process_next_delivery().await;
    ctx.stats.leave();

    match claimed {
        Ok(Some(_)) => {
            ctx.stats.processed.fetch_add(1, Ordering::SeqCst);
            true
        }
        Ok(None) => {
            ctx.stats.empty_polls.fetch_add(1, Ordering::SeqCst);
            false
        }
        Err(error) => {
            // 上游对「认领失败」记一条 `slog.Error` 并按 `worked == false` 处理（认领失败那种
            // 失败面确实是 `false`），只在「已认领、收口中出错」时才 `true` 继续排空。
            // 本地 `process_next_delivery` 的 `Err` 把两类合成一个变体（都是
            // `WebhookError::Worker`）⇒ **统一按 `false`**：宁可慢一拍，也绝不在库故障时紧循环
            // 自旋（那正是上游那半边行为里最危险的一处）。
            ctx.stats.failed_polls.fetch_add(1, Ordering::SeqCst);
            tracing::error!(error = %error, "webhook worker: process delivery failed");
            false
        }
    }
}

#[cfg(test)]
mod tests;
