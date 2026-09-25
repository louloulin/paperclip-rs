//! **优先级队列**：每片一条队列、一个 worker，准入非阻塞，满了就削减。
//!
//! 本文件是 `relay.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分
//! （上游 1,578 行 ⇒ 按「重投递链 / 优先级队列」拆），逐条清单见 `docs/32` §34 的 D9。
//!
//! # 分片买到的是什么（上游逐字）
//!
//! 分片是**并发**的界，不是隔离的保证：安装被哈希到 `RelayConfig::shards` 条队列上，
//! 所以两个撞在同一片上的 bot **确实**会互相等。片数买到的是"一个慢 bot 无法占满每个 worker"。

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::chain::{wait_until, Lines};
use super::{RelayFrame, RelayOutbound};

/// 进程内"已经处理过的 relay 事件 id"的默认容量（上游 `newSeenEvents(4096)`）。
pub const DEFAULT_SEEN_EVENTS: usize = 4096;

/// 一个在等 worker 的帧，带着它的 claim 用的 id、以及一个 worker 已经捡起它几次（上游 `queued`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub frame: RelayFrame,
    pub event_id: String,
    pub attempts: usize,
}

impl Queued {
    /// 第一次被 Worker 捡起。
    #[must_use]
    pub fn new(frame: RelayFrame, event_id: &str) -> Self {
        Self {
            frame,
            event_id: event_id.to_string(),
            attempts: 0,
        }
    }
}

/// 一个有界、**每进程**的"已经处理过的 relay 事件 id"集合（上游 `seenEvents`）。
///
/// 它是 Redis claim **前面**那道便宜的闸 —— 发布方读到它自己那条帧的副本是最常见的情况，
/// 而那从不需要一次往返。它**不是**幂等机制；[`super::DedupeStore`] 才是。
#[derive(Debug)]
pub struct SeenEvents {
    inner: Mutex<SeenInner>,
    limit: usize,
}

#[derive(Debug, Default)]
struct SeenInner {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenEvents {
    /// 建一个有界的集合。
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Mutex::new(SeenInner::default()),
            limit,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SeenInner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 上游 `claim`：这个 id 是**新的**吗（`true` = 第一次见，可以往下走）。
    ///
    /// 空 id 一律报 `true`：没有 id 就没有幂等可谈，而调用方（`deliver_outbound` 的帧没有事件 id）
    /// 需要的是"别把无 id 当成重复"。
    pub fn claim(&self, id: &str) -> bool {
        if id.is_empty() {
            return true;
        }
        let mut inner = self.lock();
        if !inner.ids.insert(id.to_string()) {
            return false;
        }
        inner.order.push_back(id.to_string());
        while inner.order.len() > self.limit {
            if let Some(oldest) = inner.order.pop_front() {
                inner.ids.remove(&oldest);
            }
        }
        true
    }

    /// 上游 `forget`：把 id 从集合里去掉（一次失败的投递要能被再试一次）。
    pub fn forget(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        let mut inner = self.lock();
        inner.ids.remove(id);
        inner.order.retain(|seen| seen != id);
    }

    /// 当前记住几个 id（诊断与用例读它）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().ids.len()
    }

    /// 空吗。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 一条分片的准入队列（上游 `r.queues[i] chan queued`，只是有界且**不阻塞**）。
///
/// `push` 在有界时返回 `false` 而不是等：调用方是共享的分片读循环，等它会把那条 shard 上别的
/// 一切（浏览器实时流量、daemon 唤醒、别的 bot）一起卡住。
#[derive(Debug)]
pub struct ShardQueue {
    items: Mutex<VecDeque<Queued>>,
    depth: usize,
    notify: tokio::sync::Notify,
    /// 被削减过几帧（诊断用）。
    shed: AtomicUsize,
}

impl ShardQueue {
    /// 建一条深度为 `depth` 的队列。
    #[must_use]
    pub fn new(depth: usize) -> Self {
        Self {
            items: Mutex::new(VecDeque::new()),
            depth,
            notify: tokio::sync::Notify::new(),
            shed: AtomicUsize::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Queued>> {
        match self.items.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 入队；满了报 `false`（调用方去 `shed`）。
    pub fn push(&self, item: Queued) -> bool {
        let mut items = self.lock();
        if items.len() >= self.depth {
            drop(items);
            self.shed.fetch_add(1, Ordering::SeqCst);
            return false;
        }
        items.push_back(item);
        drop(items);
        // `notify_one`（不是 `notify_waiters`）：没有等待者时它**存下一个许可**，
        // 于是 worker 下一次 `notified()` 会立刻返回 —— 这正是"帧到达与 worker 开始等之间"
        // 那段窗口不能丢帧的原因。
        self.notify.notify_one();
        true
    }

    /// 非阻塞取一个。
    pub fn pop(&self) -> Option<Queued> {
        self.lock().pop_front()
    }

    /// 当前排了几个。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 空吗。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 深度（诊断用）。
    #[must_use]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// 被本队列削减过几帧。
    #[must_use]
    pub fn shed_count(&self) -> usize {
        self.shed.load(Ordering::SeqCst)
    }

    /// 等下一次入队（worker 的 select 用）。
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

impl RelayOutbound {
    /// 上游 `work`：一片的**整个**一生。
    ///
    /// 它碰的每一样都属于**这一个**任务：每条安装的线与那一个唤醒它们的计时器。两处都是刻意的：
    ///
    /// - 线是为了让一个被重投递的帧停在自己那条安装的**队首**而不是队列尾部，于是一条更晚的回复
    ///   不可能超到它前面；
    /// - 计时器是 worker 自己的，是为了让一次待定的重投递成为 worker 的一部分 —— 在它外面调度的
    ///   计时器可能在 worker 已经排空并退出之后触发，把一条帧放进一条没人读的队列。
    pub(crate) async fn work(
        &self,
        shard: usize,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        // 上游 `handlerReady`：没有 handler 就没有东西可以拿来投递（`start` 之前的帧留在队列里）。
        loop {
            if self.handler_ready() {
                break;
            }
            tokio::select! {
                () = self.attached_notify.notified() => {}
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        // 被取消时 handler 是否已经附着是任意的（select 在两个就绪分支之间
                        // 是任意的）⇒ 只信 `handler_ready` 这个事实；没有它，排空没有可用的东西。
                        if !self.handler_ready() {
                            return;
                        }
                        break;
                    }
                }
            }
        }
        let queue = match self.queues().get(shard) {
            Some(queue) => queue.clone(),
            None => return,
        };
        let mut lines = Lines::new();
        loop {
            // 队列里已经有的东西先走（非阻塞）。
            while let Some(item) = queue.pop() {
                if *shutdown.borrow() {
                    // select 是公平的，所以一个项可以赢过已经触发的取消。在已取消的上下文上执行它
                    // 会把它误归档成一次去重故障；它属于排空。
                    self.drain_remaining(&mut lines, shard, Some(item)).await;
                    return;
                }
                self.offer(&mut lines, item).await;
            }
            let wait = wait_until(lines.earliest_due(), Instant::now());
            tokio::select! {
                () = queue.notified() => {}
                () = tokio::time::sleep(wait) => {
                    if *shutdown.borrow() {
                        self.drain_remaining(&mut lines, shard, None).await;
                        return;
                    }
                    self.fire_due(&mut lines).await;
                }
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        self.drain_remaining(&mut lines, shard, None).await;
                        return;
                    }
                }
            }
        }
    }

    /// 等所有有界的睡眠都停下（用例在断言"没有别的投递会发生"时用它）。
    ///
    /// 它只做一个**宽限**：把 worker 手上还没到期的线等一轮（上游 `Wait` 等的是 `sync.WaitGroup`，
    /// 而本仓的 worker 是任务 ⇒ 停机开关 + [`super::RelayHandle::wait`] 才是那条路径）。
    pub async fn quiesce(&self, grace: Duration) {
        tokio::time::sleep(grace).await;
    }
}
