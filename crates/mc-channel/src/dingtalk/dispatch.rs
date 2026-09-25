//! `DingTalk` 入站**调度器**：把入站处理与 Stream 读循环解耦（上游 `dispatch.go` 202 行）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 为什么需要它（上游注释逐字）：帧要**立刻** ACK，作业跑在 **per-conversation 串行队列**上
//!   ⇒ 一次慢的媒体下载既不能饿死 ping / 系统帧，也不能重排某个会话的对话顺序。
//!
//! # 六条语义（每条一个用例）
//!
//! 1. **同会话严格按序**：一个会话同一时刻只有一个 drain 任务（`active` 集合），新的 enqueue
//!    往队尾追加，绝不新开并行度；
//! 2. **跨会话并行、全局有界**：并发上限是 `max_workers`（上游 8）的**信号量**，按**安装**
//!    计（一个 channel 一个 dispatcher）；
//! 3. **调用方永不阻塞**：`enqueue` 是同步的，队列满了就**丢弃最新**并记 warn（上游逐字：
//!    "the caller (the socket read loop) must never block"）；
//! 4. **两层上限都是内存兜底**：单会话 `max_queue_depth`（256）+ 整安装 `max_pending`（2048）。
//!    它们**远高于**任何真实的人类爆发 ⇒ 溢出在实践中不可达；
//! 5. **作业脱离 socket 的 ctx**：红色重连**不得**取消在飞的追加（上游逐字）。本仓的等价物是
//!    "drain 任务由 `tokio::spawn` 起 ⇒ 与 `connect` 的 future 无关"；
//! 6. **收口是可等的**：`start_close` 停止收新的，`wait_closed(budget)` 等已 ACK 的跑完。
//!
//! # 与上游的两处形态差异（登记 `docs/32` §19 的 D 项）
//!
//! 1. **取消穿透**：上游把 `d.ctx` 派生的 `ctx` 交给 handler，硬取消时在飞作业**立刻**看到
//!    取消。本仓的作业签名没有 ctx（engine 的 `InboundHandler` 也没有）⇒ 硬取消只丢弃
//!    **排队中**的作业，在飞作业跑完自己那一次（最长 `job_timeout`）。收口预算因此是
//!    "等排队清空 + 在飞自然结束"，不是"立刻掐断"。
//! 2. **worker 计数**：上游用 `sync.WaitGroup`；本仓用一条 `watch` 通道广播计数
//!    （`watch` 不会丢最新值 ⇒ 收口监听没有丢唤醒的窗口）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{watch, Semaphore};

use super::inbound::BotCallbackData;

/// 一个作业的超时（上游 `dispatchJobTimeout`）：只约束**同步**的入库路径 —— 远端媒体的
/// 解析已被共享 Router 拆到别的路径上。
pub const JOB_TIMEOUT: Duration = Duration::from_secs(120);
/// 一个安装的并发作业上限（上游 `maxDispatchWorkers`）。
pub const MAX_WORKERS: usize = 8;
/// 单个会话的排队深度（上游 `maxDispatchQueueDepth`）：纯粹是内存兜底。
pub const MAX_QUEUE_DEPTH: usize = 256;
/// 一个安装里"已接受但未跑完"的总数上限（上游 `maxDispatchPending`）：
/// 只限单会话的话，每个不同会话仍可各挂一个等待信号量的 drain 任务。
pub const MAX_PENDING: usize = 2048;

/// 一个入站作业（上游 `inboundJob`）：**保留原始回调**而不是归一化结果 ——
/// 这样 bot 身份解析可以在归一化之前发生（上游 `dingtalk_channel.go` 的注释逐字），
/// 而平台字段不会漏进跨平台的 `InboundMessage`。
#[derive(Debug, Clone, PartialEq)]
pub struct InboundJob {
    /// 接收这条回调的连接所属安装的 AppKey（安装路由键）。
    pub app_id: String,
    pub callback: BotCallbackData,
}

impl InboundJob {
    /// 就位构造。
    #[must_use]
    pub fn new(app_id: impl Into<String>, callback: BotCallbackData) -> Self {
        Self {
            app_id: app_id.into(),
            callback,
        }
    }
}

/// 作业消费者（实现在 `mod.rs`：归一化 + 交给 engine 的 `SharedInboundHandler`）。
///
/// 没有返回值（上游 `dispatcher.handle` 也没有）：作业里的失败只记日志 —— 帧**已经** ACK，
/// engine 的去重与审计接管后续。
#[async_trait]
pub trait JobHandler: Send + Sync {
    /// 跑一个作业（调用方已经套好超时与并发位）。
    async fn handle(&self, job: InboundJob);
}

/// 调度器的四个旋钮（默认 = 上游的四个常量；用例调小它们）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchLimits {
    pub max_workers: usize,
    pub max_queue_depth: usize,
    pub max_pending: usize,
    pub job_timeout: Duration,
}

impl Default for DispatchLimits {
    fn default() -> Self {
        Self {
            max_workers: MAX_WORKERS,
            max_queue_depth: MAX_QUEUE_DEPTH,
            max_pending: MAX_PENDING,
            job_timeout: JOB_TIMEOUT,
        }
    }
}

/// `enqueue` 的判决（上游只记日志；本仓把它变成**可断言**的返回值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// 已排队（或在既有队列上追加）。
    Queued,
    /// 调度器已经收口 ⇒ 丢弃。
    DroppedClosed,
    /// 整安装的 pending 满了 ⇒ 丢弃。
    DroppedInstallationFull,
    /// 该会话的队列满了 ⇒ 丢弃。
    DroppedConversationFull,
}

impl EnqueueOutcome {
    /// 是否真的入队了。
    #[must_use]
    pub fn queued(self) -> bool {
        matches!(self, Self::Queued)
    }
}

/// 四个 `watch` 通道的常驻接收者（见 [`Dispatcher::channel_keepers`]）。
struct ChannelKeepers {
    _closed: watch::Receiver<bool>,
    _cancelled: watch::Receiver<bool>,
    _done: watch::Receiver<bool>,
    _workers: watch::Receiver<usize>,
}

impl std::fmt::Debug for ChannelKeepers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ChannelKeepers")
    }
}

#[derive(Debug, Default)]
struct DispatchState {
    queues: HashMap<String, VecDeque<InboundJob>>,
    active: HashSet<String>,
    pending: usize,
    closed: bool,
}

/// per-conversation 串行、跨会话并行（有界）的入站调度器。
pub struct Dispatcher {
    handler: Arc<dyn JobHandler>,
    limits: DispatchLimits,
    permits: Arc<Semaphore>,
    state: Mutex<DispatchState>,
    /// 已收口（不再接受新的 enqueue）。
    closed: watch::Sender<bool>,
    /// 硬取消（只丢弃**排队中**的作业，见模块文档差异 1）。
    cancelled: watch::Sender<bool>,
    /// 已跑完收口（所有 drain 任务退出）。
    done: watch::Sender<bool>,
    /// 活跃 drain 任务数（`watch` 不会丢最新值 ⇒ 收口监听无丢唤醒窗口）。
    workers: watch::Sender<usize>,
    /// ⚠️ 四个 `watch` 通道各自的**常驻接收者**：`watch::Sender::send` 在没有接收者时会返回
    /// `Err` 且**不更新值** ⇒ 没有它们，"先置位、后来者才订阅"会读到旧值（收口判决因此失效）。
    /// 这四个字段只为了活着，不参与任何判决。
    channel_keepers: ChannelKeepers,
    /// 累计入队 / 丢弃计数（诊断；上游用日志）。
    queued_total: AtomicUsize,
    dropped_total: AtomicUsize,
}

/// `missing_fields_in_debug` 是**故意**豁免的：这里要挡住的是"把用户正文打出来"，而不是
/// "把每个字段都打出来" —— 队列内容与内部 `state` 都不该出现在日志里。
#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for Dispatcher {
    /// 手写：`handler` 是 trait 对象，队列内容**不进**日志（可能含用户正文）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        formatter
            .debug_struct("Dispatcher")
            .field("handler", &"<dyn JobHandler>")
            .field("limits", &self.limits)
            .field("conversations", &state.queues.len())
            .field("pending", &state.pending)
            .field("closed", &state.closed)
            .field("workers", &self.workers.borrow())
            .field("free_worker_slots", &self.permits.available_permits())
            // 四个 `watch` 通道的常驻接收者只为了"发送端始终有接收者"而存在（见字段注释）。
            .field("channel_keepers", &self.channel_keepers)
            .finish()
    }
}

impl Dispatcher {
    /// 装配（上限用上游默认值）。
    #[must_use]
    pub fn new(handler: Arc<dyn JobHandler>) -> Self {
        Self::with_limits(handler, DispatchLimits::default())
    }

    /// 带旋钮的装配（用例用）。
    #[must_use]
    pub fn with_limits(handler: Arc<dyn JobHandler>, limits: DispatchLimits) -> Self {
        let (closed, closed_keeper) = watch::channel(false);
        let (cancelled, cancelled_keeper) = watch::channel(false);
        let (done, done_keeper) = watch::channel(false);
        let (workers, workers_keeper) = watch::channel(0_usize);
        Self {
            channel_keepers: ChannelKeepers {
                _closed: closed_keeper,
                _cancelled: cancelled_keeper,
                _done: done_keeper,
                _workers: workers_keeper,
            },
            handler,
            limits,
            permits: Arc::new(Semaphore::new(limits.max_workers)),
            state: Mutex::new(DispatchState::default()),
            closed,
            cancelled,
            done,
            workers,
            queued_total: AtomicUsize::new(0),
            dropped_total: AtomicUsize::new(0),
        }
    }

    /// 累计入队数（诊断）。
    #[must_use]
    pub fn queued_total(&self) -> usize {
        self.queued_total.load(Ordering::SeqCst)
    }

    /// 累计丢弃数（诊断）。
    #[must_use]
    pub fn dropped_total(&self) -> usize {
        self.dropped_total.load(Ordering::SeqCst)
    }

    /// 是否已收口。
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// 已收口的信号（给帧循环用：收口 ⇒ 会话可以优雅退出）。
    #[must_use]
    pub fn closed_receiver(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }

    fn lock(&self) -> MutexGuard<'_, DispatchState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 追加一个作业到它的会话队列；**永不阻塞调用方**（上游逐字）。
    ///
    /// 同一个会话的作业严格按序跑（新会话 ⇒ 开一个 drain 任务；已在跑 ⇒ 只追加）。
    pub fn enqueue(self: &Arc<Self>, conversation_id: &str, job: InboundJob) -> EnqueueOutcome {
        let start = {
            let mut state = self.lock();
            if state.closed {
                self.dropped_total.fetch_add(1, Ordering::SeqCst);
                return EnqueueOutcome::DroppedClosed;
            }
            if state.pending >= self.limits.max_pending {
                drop(state);
                tracing::warn!(
                    conversation_id,
                    msg_id = job.callback.msg_id,
                    "dingtalk dispatch: installation queue full, dropping message"
                );
                self.dropped_total.fetch_add(1, Ordering::SeqCst);
                return EnqueueOutcome::DroppedInstallationFull;
            }
            if state
                .queues
                .get(conversation_id)
                .is_some_and(|queue| queue.len() >= self.limits.max_queue_depth)
            {
                drop(state);
                tracing::warn!(
                    conversation_id,
                    msg_id = job.callback.msg_id,
                    "dingtalk dispatch: conversation queue full, dropping message"
                );
                self.dropped_total.fetch_add(1, Ordering::SeqCst);
                return EnqueueOutcome::DroppedConversationFull;
            }
            state
                .queues
                .entry(conversation_id.to_string())
                .or_default()
                .push_back(job);
            state.pending += 1;
            state.active.insert(conversation_id.to_string())
        };
        self.queued_total.fetch_add(1, Ordering::SeqCst);
        if start {
            self.spawn_drain(conversation_id.to_string());
        }
        EnqueueOutcome::Queued
    }

    /// 起一个会话的 drain 任务（脱离 socket 的 future：重连不取消在飞的作业）。
    ///
    /// ⚠️ 计数在这里**先加**再加任务：收口监听读的是"活跃 drain 任务数"，若等任务自己加，
    /// 一个刚落地的 enqueue 会被读成"没有在跑的" ⇒ 收口会提前宣布完成。
    fn spawn_drain(self: &Arc<Self>, conversation_id: String) {
        if tokio::runtime::Handle::try_current().is_err() {
            // 没有运行时 ⇒ 作业永远跑不了；明确告警而不是静默积压。
            tracing::error!(
                conversation_id,
                "dingtalk dispatch: no async runtime; queued job will not run"
            );
            return;
        }
        self.worker_started();
        let dispatcher = Arc::clone(self);
        tokio::spawn(async move {
            Arc::clone(&dispatcher)
                .drain_conversation(conversation_id)
                .await;
            dispatcher.worker_finished();
        });
    }

    /// 一个会话的串行 drain：严格按序取作业、等一个全局工作位、跑完、继续。
    ///
    /// 队列空 ⇒ 清掉 `active` 并退出（后续 enqueue 会开一个新的 drain —— 上游逐字）。
    async fn drain_conversation(self: Arc<Self>, conversation_id: String) {
        while let Some(job) = self.next_job(&conversation_id) {
            let permit = tokio::select! {
                acquired = Arc::clone(&self.permits).acquire_owned() => match acquired {
                    Ok(permit) => permit,
                    Err(_) => break,
                },
                () = self.hard_cancelled() => {
                    self.drop_queued(&conversation_id, &job);
                    break;
                }
            };
            let _ = tokio::time::timeout(self.limits.job_timeout, self.handler.handle(job)).await;
            drop(permit);
            self.complete_one();
        }
    }

    /// 取该会话的下一个作业；队列空 / 硬取消 ⇒ 清掉 `active` 并返回 `None`。
    fn next_job(&self, conversation_id: &str) -> Option<InboundJob> {
        let mut state = self.lock();
        if self.hard_cancelled_now() {
            let dropped = state.queues.remove(conversation_id).map_or(0, |q| q.len());
            state.pending = state.pending.saturating_sub(dropped);
            state.active.remove(conversation_id);
            return None;
        }
        let job = state
            .queues
            .get_mut(conversation_id)
            .and_then(VecDeque::pop_front);
        if job.is_some() {
            return job;
        }
        state.queues.remove(conversation_id);
        state.active.remove(conversation_id);
        None
    }

    /// 硬取消时放弃这个作业与它后面排队的（上游：ctx 取消 ⇒ 整个会话队列作废）。
    fn drop_queued(&self, conversation_id: &str, job: &InboundJob) {
        let mut state = self.lock();
        let dropped = state.queues.remove(conversation_id).map_or(0, |q| q.len());
        state.pending = state.pending.saturating_sub(dropped + 1);
        state.active.remove(conversation_id);
        drop(state);
        tracing::debug!(
            conversation_id,
            msg_id = job.callback.msg_id,
            "dingtalk dispatch: cancelled while waiting for a worker slot"
        );
    }

    /// 一个作业跑完（pending 减一）。
    fn complete_one(&self) {
        let mut state = self.lock();
        state.pending = state.pending.saturating_sub(1);
    }

    /// 一个 drain 任务开始（在起任务**之前**调用，见 [`Dispatcher::spawn_drain`]）。
    fn worker_started(&self) {
        let current = self.workers.borrow().saturating_add(1);
        let _ = self.workers.send(current);
    }

    /// 一个 drain 任务结束。
    fn worker_finished(&self) {
        let current = self.workers.borrow().saturating_sub(1);
        let _ = self.workers.send(current);
    }

    fn hard_cancelled_now(&self) -> bool {
        *self.cancelled.borrow()
    }

    async fn hard_cancelled(&self) {
        let mut receiver = self.cancelled.subscribe();
        if *receiver.borrow_and_update() {
            return;
        }
        let _ = receiver.changed().await;
    }

    /// 停止接受新的作业，并**开始**异步等所有 drain 任务退出（上游 `startClose`）。
    ///
    /// 与 `wait_closed` 分开，是为了让拥有者能在"重连要不要复用这条队列"的判决之前
    /// 先发布"已收口"这个状态。
    pub fn start_close(self: &Arc<Self>) {
        let first = {
            let mut state = self.lock();
            let first = !state.closed;
            state.closed = true;
            first
        };
        let _ = self.closed.send(true);
        if !first {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            let _ = self.done.send(true);
            return;
        }
        let dispatcher = Arc::clone(self);
        tokio::spawn(async move {
            let mut workers = dispatcher.workers.subscribe();
            while *workers.borrow_and_update() != 0 {
                if workers.changed().await.is_err() {
                    break;
                }
            }
            let _ = dispatcher.done.send(true);
        });
    }

    /// 等收口完成；预算用尽 ⇒ **硬取消**并返回 `false`（上游 `waitClosed` 逐条对应）。
    pub async fn wait_closed(&self, budget: Duration) -> bool {
        let mut done = self.done.subscribe();
        if *done.borrow_and_update() {
            return true;
        }
        if tokio::time::timeout(budget, done.changed()).await.is_ok() {
            true
        } else {
            let _ = self.cancelled.send(true);
            *self.done.borrow()
        }
    }

    /// 收口 + 等（上游 `drainAndClose`）。
    pub async fn drain_and_close(self: &Arc<Self>, budget: Duration) -> bool {
        self.start_close();
        self.wait_closed(budget).await
    }

    /// 队列快照：`(会话 id, 排队深度)`（诊断 / 用例用；按会话 id 排序 ⇒ 确定性）。
    #[must_use]
    pub fn queue_depths(&self) -> Vec<(String, usize)> {
        let state = self.lock();
        let mut rows: Vec<(String, usize)> = state
            .queues
            .iter()
            .map(|(id, queue)| (id.clone(), queue.len()))
            .collect();
        rows.sort();
        rows
    }

    /// 在跑的会话数。
    #[must_use]
    pub fn active_conversations(&self) -> usize {
        self.lock().active.len()
    }

    /// 已接受但未跑完的作业数。
    #[must_use]
    pub fn pending(&self) -> usize {
        self.lock().pending
    }
}

// =====================================================================
// 每安装一条队列槽（上游 `dispatchSlotRegistry` / `dispatchSlot`）
// =====================================================================

/// 每个 `AppKey` 一条队列：**重连复用同一条队列**（上游这套槽的全部存在理由）。
///
/// 上游逐字：`Supervisor.Build runs once per reconnect. Reusing the queue by the installation's
/// unique AppKey prevents an old in-flight turn and the next turn received after reconnect from
/// running concurrently.`（重连之前还在飞的轮次，与重连之后收到的轮次，**不得**并发。）
///
/// 两条纪律：
///
/// - **已收口的槽不再复用**：`acquire` 见到已收口的槽就建一条新队列（旧的那条已经不再接受
///   作业了）；
/// - **`release` 按 `Arc` 比较**：重连期重叠的旧代不得删掉已经被换代接管的那一格
///   （上游 `releaseClosedDispatchSlot` 的 `CompareAndSwap` 同款）。
#[derive(Default)]
pub struct DispatchSlotRegistry {
    slots: Mutex<HashMap<String, Arc<Dispatcher>>>,
    limits: DispatchLimits,
    created: AtomicUsize,
}

impl std::fmt::Debug for DispatchSlotRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DispatchSlotRegistry")
            .field("slots", &self.lock().len())
            .field("limits", &self.limits)
            .field("created", &self.created.load(Ordering::SeqCst))
            .finish()
    }
}

impl DispatchSlotRegistry {
    /// 空注册表（队列上限用上游默认值）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            limits: DispatchLimits::default(),
            created: AtomicUsize::new(0),
        }
    }

    /// 带旋钮的注册表（用例用）。
    #[must_use]
    pub fn with_limits(limits: DispatchLimits) -> Self {
        Self {
            limits,
            ..Self::new()
        }
    }

    /// 队列旋钮（诊断）。
    #[must_use]
    pub fn limits(&self) -> DispatchLimits {
        self.limits
    }

    /// 取（或建）该 `AppKey` 的队列。
    pub fn acquire(&self, app_key: &str, handler: Arc<dyn JobHandler>) -> Arc<Dispatcher> {
        let mut slots = self.lock();
        if let Some(existing) = slots.get(app_key) {
            if !existing.is_closed() {
                return Arc::clone(existing);
            }
        }
        let dispatcher = Arc::new(Dispatcher::with_limits(handler, self.limits));
        self.created.fetch_add(1, Ordering::SeqCst);
        slots.insert(app_key.to_string(), Arc::clone(&dispatcher));
        dispatcher
    }

    /// 把槽从注册表里摘掉 —— **只**当它还是当前那一格（按 `Arc` 比较）。
    ///
    /// 返回 `true` = 真的摘掉了。
    pub fn release(&self, app_key: &str, dispatcher: &Arc<Dispatcher>) -> bool {
        let mut slots = self.lock();
        let current = slots
            .get(app_key)
            .is_some_and(|slot| Arc::ptr_eq(slot, dispatcher));
        if current {
            slots.remove(app_key);
        }
        current
    }

    /// 已建的队列总数（诊断；含已经不复用但仍在被引用的那些）。
    #[must_use]
    pub fn created(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Dispatcher>>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
