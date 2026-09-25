//! 运行触发去抖：`pendingBatcher`（上游 `channel/engine/batcher.go`，169 行）。
//!
//! - **写者**：M7-2（`docs/60` §3.3 的写集表）。
//! - **它解决什么**：一条入站消息只推迟**一次 agent run 的触发**（`chat_message` 行、去重、
//!   帧 ACK 都已在前面同步完成）。窗口按 `(chat_session_id, context_revision)` 分桶：
//!   静默窗口到期时，**最新一次** schedule 的触发**恰好跑一次** ⇒ 一代之内的连发塌成一次 run，
//!   而 `/clear` 的代际边界是**硬边界**（它换 key，于是绝不覆盖前一代的未决触发）。
//! - **状态在进程内**（`docs/60` §2.5 的 R-M7-1）：WS 租约保证一条安装只有一个活跃持有者，
//!   所以一个会话只被一个进程去抖。窗口内硬崩 = 丢掉这次触发（消息是持久的，只是不会有 run
//!   直到下一条消息）；优雅停机走 [`RunBatcher::drain`]，正常重启**不**撞这个边界。
//! - **与上游的两处形态差异**（不是语义差异）：
//!   1. 上游把 `flush func()` 交给调用方；本仓的 `RunTriggerer` 契约是
//!      `schedule_chat_run(ChatRunParams)` ⇒ 本文件**就是**那个端口（包装一个内层触发者）；
//!   2. 上游的定时器接缝是 `afterFunc func(d, fn) stoppableTimer`；这里的等价物是
//!      [`TimerScheduler`]（默认真实现 [`TokioTimers`]，用例注入手动实现 ⇒ **不** sleep 真实时间）。
//! - **围栏**：每次 (re)arm 铸一个单调 generation，回调带上它 ⇒ "定时器到点"与"Stop 取消"
//!   并发时，被取代的那次回调**不会**再触发（上游 `onFire` 的同一条围栏）。
//! - **`drain` 之后是终态**：之后到达的 schedule **就地**触发，不再攒窗口（停机竞态：消息在
//!   排空开始之后才到）。
//!
//! 行预算（门 ⑩）：≤800 行。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::id::Id;

use crate::engine::resolvers::{ChatRunParams, EngineResult, RunTriggerer};

/// 默认静默窗口：**3s**（上游 `DefaultChatRunBatchWindow`，MUL-2968）。
///
/// 长到足以把"转发一段对话再补一句话"的连发收成一次 run，短到机器人的第一句回复不显得迟。
pub const DEFAULT_CHAT_RUN_BATCH_WINDOW: Duration = Duration::from_secs(3);

// =====================================================================
// 定时器接缝
// =====================================================================

/// 一次性定时器的取消句柄。
pub trait TimerHandle: Send {
    /// 取消（已经点着或已经取消过 ⇒ no-op）。
    fn cancel(&mut self);
}

/// 定时器工厂（上游 `afterFunc` 的等价物）。
///
/// **契约**：`arm` 只**登记**，绝不在调用栈里同步调用 `fire`（否则会与
/// [`RunBatcher`] 的状态锁互锁）。真实现是 [`TokioTimers`]（spawn + sleep）。
pub trait TimerScheduler: Send + Sync {
    /// `delay` 之后调用 `fire`；返回的句柄可以取消它。
    fn arm(&self, delay: Duration, fire: Box<dyn FnOnce() + Send>) -> Box<dyn TimerHandle>;
}

/// `tokio` 实现：spawn 一个任务 sleep 再调用。
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioTimers;

struct TokioTimerHandle {
    task: tokio::task::JoinHandle<()>,
}

impl TimerHandle for TokioTimerHandle {
    fn cancel(&mut self) {
        self.task.abort();
    }
}

impl TimerScheduler for TokioTimers {
    fn arm(&self, delay: Duration, fire: Box<dyn FnOnce() + Send>) -> Box<dyn TimerHandle> {
        let task = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            fire();
        });
        Box::new(TokioTimerHandle { task })
    }
}

// =====================================================================
// 去抖器
// =====================================================================

struct Entry {
    handle: Box<dyn TimerHandle>,
    params: ChatRunParams,
    generation: u64,
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Entry>,
    seq: u64,
    stopped: bool,
    inflight: Vec<tokio::task::JoinHandle<()>>,
}

/// 一次 `schedule` 的判决。
enum ScheduleOutcome {
    /// 窗口已 arm（或替换了同桶的窗口）。
    Armed,
    /// 已有活跃窗口且调用方要求"只在缺失时 arm"⇒ 什么都没做。
    Skipped,
    /// 已经排空 ⇒ 返回给调用方**就地**触发。
    Stopped(ChatRunParams),
}

struct Inner {
    window: Duration,
    timers: Arc<dyn TimerScheduler>,
    trigger: Arc<dyn RunTriggerer>,
    state: Mutex<State>,
    /// 自引用（`schedule` 里要把 `Arc<Self>` 交给定时器回调）⇒ 由 [`RunBatcher`] 建时注入。
    self_ref: Mutex<Option<Arc<Inner>>>,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 分桶键：`chat_session_id` + 上下文代际（`/clear` 的硬边界就靠它）。
    fn key(params: &ChatRunParams) -> String {
        format!("{}#{}", params.session_id, params.context_revision)
    }

    /// 点着一次触发（跑在定时器的任务上；**不**持锁调用内层触发）。
    fn fire(self: &Arc<Self>, key: &str, generation: u64) {
        let params = {
            let mut state = self.lock();
            match state.pending.get(key) {
                // 已被取代 / 正在停机 ⇒ 不点着（停机路径由 `drain` 负责跑它们）。
                Some(entry) if entry.generation == generation && !state.stopped => {
                    state.pending.remove(key).map(|entry| entry.params)
                }
                _ => None,
            }
        };
        let Some(params) = params else {
            return;
        };
        let trigger = Arc::clone(&self.trigger);
        let inner = Arc::clone(self);
        let task = tokio::spawn(async move {
            if let Err(error) = trigger.schedule_chat_run(params).await {
                tracing::warn!(
                    code = error.code_hint(),
                    "channel engine: debounced run trigger failed"
                );
            }
            inner.prune_inflight();
        });
        self.lock().inflight.push(task);
    }

    fn prune_inflight(&self) {
        let mut state = self.lock();
        state.inflight.retain(|task| !task.is_finished());
    }

    fn schedule(&self, params: ChatRunParams, replace: bool) -> ScheduleOutcome {
        let key = Self::key(&params);
        let generation = {
            let mut state = self.lock();
            if state.stopped {
                return ScheduleOutcome::Stopped(params);
            }
            if state.pending.contains_key(&key) && !replace {
                return ScheduleOutcome::Skipped;
            }
            state.seq += 1;
            state.seq
        };
        // 先放开锁再 arm（`arm` 的实现会 spawn，不持有状态锁更安全）。
        if let Some(existing) = self.lock().pending.get_mut(&key) {
            existing.handle.cancel();
        }
        let inner = self.arc_self();
        let key_for_fire = key.clone();
        let handle = self.timers.arm(
            self.window,
            Box::new(move || inner.fire(&key_for_fire, generation)),
        );
        let mut state = self.lock();
        // 同一桶的并发 arm：后到的那一个赢，先把先前那个取消掉。
        if let Some(existing) = state.pending.get_mut(&key) {
            existing.handle.cancel();
        }
        state.pending.insert(
            key,
            Entry {
                handle,
                params,
                generation,
            },
        );
        ScheduleOutcome::Armed
    }

    /// 取回自己（`Arc<Self>` 在 `schedule` 里拿不到 ⇒ 由 [`RunBatcher`] 建时注入）。
    fn arc_self(&self) -> Arc<Self> {
        self.self_ref
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("self reference is set before any schedule")
    }
}

/// 运行触发去抖器（`RunTriggerer` 的实现；包装一个内层触发者）。
///
/// ⚠️ 与 [`crate::engine::router::Router`] 的接法：`Router::new(classifier, trigger, …)` 里传
/// **本类型**，本类型再包住真正入队的那个实现 ⇒ 去抖是 Router 之外的一层，路由流水线不动。
pub struct RunBatcher {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RunBatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunBatcher")
            .field("window", &self.inner.window)
            .field("pending", &self.pending_count())
            .finish_non_exhaustive()
    }
}

impl RunBatcher {
    /// 用默认窗口（3s）构造。
    pub fn new(trigger: Arc<dyn RunTriggerer>) -> Self {
        Self::with_window(trigger, DEFAULT_CHAT_RUN_BATCH_WINDOW)
    }

    /// 用指定窗口构造（非正窗口回落到 [`DEFAULT_CHAT_RUN_BATCH_WINDOW`]，上游同款）。
    pub fn with_window(trigger: Arc<dyn RunTriggerer>, window: Duration) -> Self {
        Self::with_parts(trigger, window, Arc::new(TokioTimers))
    }

    /// 用指定定时器构造（用例注入手动实现 ⇒ 时间序列可断言、不 sleep 真实时间）。
    pub fn with_parts(
        trigger: Arc<dyn RunTriggerer>,
        window: Duration,
        timers: Arc<dyn TimerScheduler>,
    ) -> Self {
        let window = if window.is_zero() {
            DEFAULT_CHAT_RUN_BATCH_WINDOW
        } else {
            window
        };
        let inner = Arc::new(Inner {
            window,
            timers,
            trigger,
            state: Mutex::new(State::default()),
            self_ref: Mutex::new(None),
        });
        *inner
            .self_ref
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&inner));
        Self { inner }
    }

    /// 当前有窗口在跑的分桶数。
    pub fn pending_count(&self) -> usize {
        self.inner.lock().pending.len()
    }

    /// 是否已经进入终态（`drain` 之后）。
    pub fn is_draining(&self) -> bool {
        self.inner.lock().stopped
    }

    /// 一个分桶的键（`chat_session_id` + 代际）；诊断用。
    pub fn bucket_key(session_id: Id, context_revision: i64) -> String {
        format!("{session_id}#{context_revision}")
    }

    /// **只在没有活跃窗口时**才 arm（崩溃恢复用）。
    ///
    /// 恢复老代际时用这个：缺定时器必须补上，但**新**代际的消息不得重置老代际已经跑着的静默
    /// 窗口、也不得替换它的发起人元数据（上游 `ScheduleIfAbsent` 逐字）。
    ///
    /// 返回 `true` = 这次真的 arm 了。
    pub async fn schedule_if_absent(&self, params: ChatRunParams) -> EngineResult<bool> {
        match self.inner.schedule(params, false) {
            ScheduleOutcome::Armed => Ok(true),
            ScheduleOutcome::Skipped => Ok(false),
            // 终态 ⇒ 就地触发（与 `schedule_chat_run` 同一条停机竞态处理）。
            ScheduleOutcome::Stopped(inline) => {
                self.inner.trigger.schedule_chat_run(inline).await?;
                Ok(false)
            }
        }
    }

    /// 等**已经点着**的触发收尾（不点着新的）。诊断 / 用例用。
    pub async fn settle(&self) {
        let tasks: Vec<_> = {
            let mut state = self.inner.lock();
            std::mem::take(&mut state.inflight)
        };
        for task in tasks {
            let _ = task.await;
        }
    }
}

#[async_trait]
impl RunTriggerer for RunBatcher {
    async fn schedule_chat_run(&self, params: ChatRunParams) -> EngineResult<()> {
        if let ScheduleOutcome::Stopped(inline) = self.inner.schedule(params, true) {
            // 终态之后就地触发（停机竞态：排空已开始又有消息到达）。
            self.inner.trigger.schedule_chat_run(inline).await?;
        }
        Ok(())
    }

    /// 排空：置终态、**恰好一次**地跑掉每个未决窗口，等它们在飞的触发收尾，再交给内层排空。
    ///
    /// 幂等；调用一次即终态（此后的 schedule 就地触发）。
    async fn drain(&self) -> EngineResult<()> {
        let pending: Vec<ChatRunParams> = {
            let mut state = self.inner.lock();
            state.stopped = true;
            let keys: Vec<String> = state.pending.keys().cloned().collect();
            let mut params = Vec::with_capacity(keys.len());
            for key in keys {
                if let Some(mut entry) = state.pending.remove(&key) {
                    entry.handle.cancel();
                    params.push(entry.params);
                }
            }
            params
        };
        for params in pending {
            self.inner.trigger.schedule_chat_run(params).await?;
        }
        self.settle().await;
        self.inner.trigger.drain().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mc_core::channel::ChannelKind;

    use super::*;
    use crate::engine::resolvers::ResolvedInstallation;

    /// 手动定时器（**正解**：`Arc` 共享，`fire_all` 手动点着所有未取消的登记）。
    ///
    /// ⚠️ 契约：`arm` 只**登记**，绝不在调用栈里同步调用 `fire`（否则会与去抖器的状态锁互锁）。
    type Fires = Option<Box<dyn FnOnce() + Send>>;

    #[derive(Default)]
    struct SharedTimers {
        entries: Arc<Mutex<Vec<(Duration, Fires)>>>,
    }

    /// 记录内层触发的假实现。
    #[derive(Default)]
    struct RecordingTrigger {
        calls: Mutex<Vec<(Id, i64)>>,
        drains: AtomicUsize,
        fail: Mutex<Option<&'static str>>,
    }

    #[async_trait]
    impl RunTriggerer for RecordingTrigger {
        async fn schedule_chat_run(&self, params: ChatRunParams) -> EngineResult<()> {
            if let Some(message) = *self.fail.lock().expect("fail") {
                return Err(crate::engine::resolvers::EngineError::infra(message));
            }
            self.calls
                .lock()
                .expect("calls")
                .push((params.session_id, params.context_revision));
            Ok(())
        }

        async fn drain(&self) -> EngineResult<()> {
            self.drains.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn params(session_id: Id, context_revision: i64, initiator: Id) -> ChatRunParams {
        ChatRunParams {
            installation: ResolvedInstallation::new(
                Id::new(),
                Id::new(),
                Id::new(),
                Id::new(),
                ChannelKind::Slack,
                true,
            ),
            session_id,
            initiator_user_id: initiator,
            channel_binding_id: None,
            route_revision: 1,
            force_fresh: false,
            context_revision,
        }
    }

    // ---- 手动定时器（`Arc` 共享，`fire_all` 手动点着）----

    struct SharedHandle {
        entries: Arc<Mutex<Vec<(Duration, Fires)>>>,
        index: usize,
    }

    impl TimerHandle for SharedHandle {
        fn cancel(&mut self) {
            let mut entries = self.entries.lock().expect("timers");
            if let Some(entry) = entries.get_mut(self.index) {
                entry.1 = None;
            }
        }
    }

    impl TimerScheduler for SharedTimers {
        fn arm(&self, delay: Duration, fire: Box<dyn FnOnce() + Send>) -> Box<dyn TimerHandle> {
            let mut entries = self.entries.lock().expect("timers");
            entries.push((delay, Some(fire)));
            let index = entries.len() - 1;
            Box::new(SharedHandle {
                entries: Arc::clone(&self.entries),
                index,
            })
        }
    }

    impl SharedTimers {
        /// 点着所有仍登记的定时器（按登记顺序），并清空。
        fn fire_all(&self) {
            let mut entries = std::mem::take(&mut *self.entries.lock().expect("timers"));
            for (_, fire) in entries.drain(..) {
                if let Some(fire) = fire {
                    fire();
                }
            }
        }

        fn armed(&self) -> usize {
            self.entries
                .lock()
                .expect("timers")
                .iter()
                .filter(|(_, fire)| fire.is_some())
                .count()
        }

        fn last_window(&self) -> Option<Duration> {
            self.entries
                .lock()
                .expect("timers")
                .last()
                .map(|(delay, _)| *delay)
        }
    }

    fn batcher(window: Duration) -> (Arc<SharedTimers>, Arc<RecordingTrigger>, RunBatcher) {
        let timers = Arc::new(SharedTimers::default());
        let trigger = Arc::new(RecordingTrigger::default());
        let trigger_ref: Arc<dyn RunTriggerer> = Arc::clone(&trigger) as Arc<dyn RunTriggerer>;
        let timers_ref: Arc<dyn TimerScheduler> = Arc::clone(&timers) as Arc<dyn TimerScheduler>;
        let batcher = RunBatcher::with_parts(trigger_ref, window, timers_ref);
        (timers, trigger, batcher)
    }

    /// 一代之内的连发塌成**一次** run，且用的是**最新**那条的参数。
    #[tokio::test]
    async fn a_burst_collapses_into_one_run_with_the_latest_params() {
        let (timers, trigger, batcher) = batcher(Duration::from_secs(3));
        let session_id = Id::new();
        let first = Id::new();
        let last = Id::new();
        for initiator in [first, Id::new(), last] {
            batcher
                .schedule_chat_run(params(session_id, 1, initiator))
                .await
                .expect("schedule");
        }
        assert_eq!(
            batcher.pending_count(),
            1,
            "同一 (session, 代际) 只有一个桶"
        );
        assert_eq!(timers.armed(), 1, "后到的 schedule 取消了前一个定时器");
        assert_eq!(timers.last_window(), Some(Duration::from_secs(3)));
        assert!(
            trigger.calls.lock().expect("calls").is_empty(),
            "窗口没到就不触发"
        );

        timers.fire_all();
        batcher.settle().await;
        let calls = trigger.calls.lock().expect("calls").clone();
        assert_eq!(calls.len(), 1, "静默窗口到期只触发一次");
        assert_eq!(calls[0], (session_id, 1));
        assert_eq!(batcher.pending_count(), 0);
    }

    /// 代际是**硬边界**：`/clear` 之后的新代按自己的 key 开窗口，绝不覆盖前一代的未决触发。
    #[tokio::test]
    async fn a_generation_boundary_is_its_own_bucket() {
        let (timers, trigger, batcher) = batcher(Duration::from_secs(3));
        let session_id = Id::new();
        batcher
            .schedule_chat_run(params(session_id, 1, Id::new()))
            .await
            .expect("schedule gen 1");
        batcher
            .schedule_chat_run(params(session_id, 2, Id::new()))
            .await
            .expect("schedule gen 2");
        assert_eq!(batcher.pending_count(), 2, "两代各一个桶");
        assert_eq!(timers.armed(), 2);

        timers.fire_all();
        batcher.settle().await;
        let mut calls = trigger.calls.lock().expect("calls").clone();
        calls.sort_by_key(|(_, revision)| *revision);
        assert_eq!(calls, vec![(session_id, 1), (session_id, 2)]);
        assert_eq!(
            RunBatcher::bucket_key(session_id, 2),
            format!("{session_id}#2"),
            "桶键含代际"
        );
    }

    /// 被取代的那一次回调**不**触发（定时器到点与取消并发的围栏）。
    #[tokio::test]
    async fn a_superseded_timer_never_fires() {
        let (timers, trigger, batcher) = batcher(Duration::from_secs(3));
        let session_id = Id::new();
        let first = Id::new();
        batcher
            .schedule_chat_run(params(session_id, 1, first))
            .await
            .expect("first");
        // 把**第一个**定时器的回调取出来（稍后手动点着它）：它会被下一次 schedule 取代。
        let stale = {
            let mut entries = timers.entries.lock().expect("timers");
            entries
                .get_mut(0)
                .and_then(|(_, fire)| fire.take())
                .expect("登记的定时器")
        };
        batcher
            .schedule_chat_run(params(session_id, 1, Id::new()))
            .await
            .expect("second");
        // 被取代的那次回调到点：generation 对不上 ⇒ 什么都不做。
        stale();
        assert!(
            trigger.calls.lock().expect("calls").is_empty(),
            "被取代的不触发"
        );

        timers.fire_all();
        batcher.settle().await;
        assert_eq!(
            trigger.calls.lock().expect("calls").len(),
            1,
            "只有最后那一次 arm 的回调有效"
        );
    }

    /// `schedule_if_absent`：没有活跃窗口才 arm（崩溃恢复不得重置老代的静默窗口）。
    #[tokio::test]
    async fn schedule_if_absent_does_not_reset_a_live_window() {
        let (timers, trigger, batcher) = batcher(Duration::from_secs(3));
        let session_id = Id::new();
        let recovery = Id::new();
        assert!(
            batcher
                .schedule_if_absent(params(session_id, 1, recovery))
                .await
                .expect("arm"),
            "缺定时器必须补上"
        );
        assert!(
            !batcher
                .schedule_if_absent(params(session_id, 1, Id::new()))
                .await
                .expect("no re-arm"),
            "已有窗口就不动它（发起人元数据也不能被替换）"
        );
        assert_eq!(timers.armed(), 1);

        timers.fire_all();
        batcher.settle().await;
        let calls = trigger.calls.lock().expect("calls").clone();
        assert_eq!(calls.len(), 1, "只有恢复的那一次");
    }

    /// `drain`：每个未决窗口**恰好跑一次**、等收尾、转交内层排空，且之后是终态。
    #[tokio::test]
    async fn drain_flushes_each_pending_window_exactly_once_and_is_terminal() {
        let (timers, trigger, batcher) = batcher(Duration::from_secs(3));
        let session_id = Id::new();
        batcher
            .schedule_chat_run(params(session_id, 1, Id::new()))
            .await
            .expect("gen 1");
        batcher
            .schedule_chat_run(params(session_id, 2, Id::new()))
            .await
            .expect("gen 2");
        assert_eq!(timers.armed(), 2);

        batcher.drain().await.expect("drain");
        assert!(batcher.is_draining());
        assert_eq!(batcher.pending_count(), 0);
        let mut calls = trigger.calls.lock().expect("calls").clone();
        calls.sort_by_key(|(_, revision)| *revision);
        assert_eq!(calls, vec![(session_id, 1), (session_id, 2)]);
        assert_eq!(
            trigger.drains.load(Ordering::SeqCst),
            1,
            "排空转交给内层触发者一次"
        );
        // 已经被 `drain` 取消的定时器（若之后被误点着）不得再触发。
        timers.fire_all();
        batcher.settle().await;
        assert_eq!(trigger.calls.lock().expect("calls").len(), 2);

        // 终态之后就地触发（停机竞态）。
        batcher
            .schedule_chat_run(params(session_id, 3, Id::new()))
            .await
            .expect("inline after drain");
        let calls = trigger.calls.lock().expect("calls").clone();
        assert_eq!(calls.len(), 3, "终态后的 schedule 就地跑，不再攒窗口");
        assert_eq!(calls[2], (session_id, 3));
        // 再排空一次是安全的（幂等）。
        batcher.drain().await.expect("drain again");
        assert_eq!(trigger.drains.load(Ordering::SeqCst), 2);
    }

    /// 默认窗口：非正窗口回落到 3s（上游同款）。
    #[tokio::test]
    async fn a_non_positive_window_falls_back_to_the_default() {
        let (_timers, _trigger, batcher) = batcher(Duration::ZERO);
        assert!(format!("{batcher:?}").contains("3000ms") || format!("{batcher:?}").contains("3s"));
        assert_eq!(batcher.pending_count(), 0);
        assert!(!batcher.is_draining());
    }

    /// 内层触发的错误只记日志，不改变去抖器的状态（窗口照样清空）。
    #[tokio::test]
    async fn a_failing_inner_trigger_does_not_wedge_the_window() {
        let (timers, trigger, batcher) = batcher(Duration::from_secs(3));
        *trigger.fail.lock().expect("fail") = Some("dispatcher down");
        batcher
            .schedule_chat_run(params(Id::new(), 1, Id::new()))
            .await
            .expect("schedule");
        timers.fire_all();
        batcher.settle().await;
        assert_eq!(batcher.pending_count(), 0);
        assert!(trigger.calls.lock().expect("calls").is_empty());
    }
}
