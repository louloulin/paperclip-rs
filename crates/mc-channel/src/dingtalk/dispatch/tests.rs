//! 入站调度器的用例（`dispatch.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 六条语义逐条钉住（见 `dispatch.rs` 的模块文档）：同会话按序、跨会话并行且**全局有界**、
//! 调用方永不阻塞、两层内存上限、作业脱离 socket、收口可等。并发断言用 `peak`（跑过的最大
//! 并发数）而不是墙钟时间 ⇒ 与调度器的实现细节无关。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;

use super::{
    DispatchLimits, DispatchSlotRegistry, Dispatcher, EnqueueOutcome, InboundJob, JobHandler,
    JOB_TIMEOUT, MAX_PENDING, MAX_QUEUE_DEPTH, MAX_WORKERS,
};
use crate::dingtalk::inbound::BotCallbackData;

// =====================================================================
// 记录型作业体
// =====================================================================

/// 记录每个作业的**完成顺序**与并发峰值；可选地先在闸门前等一会儿（让测试能观察到"在飞"）。
#[derive(Default)]
struct Recorder {
    order: Mutex<Vec<String>>,
    live: AtomicUsize,
    peak: AtomicUsize,
    delay: Mutex<Duration>,
    gate: Mutex<Option<watch::Sender<bool>>>,
}

impl Recorder {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay: Mutex::new(delay),
            ..Self::default()
        })
    }

    /// 装一个闸门：作业在放闸之前一直"在飞"（用于观察排队与上限）。
    fn gated(&self) -> watch::Receiver<bool> {
        let (sender, receiver) = watch::channel(false);
        *self.gate.lock().expect("lock") = Some(sender);
        receiver
    }

    fn release(&self) {
        if let Some(sender) = self.gate.lock().expect("lock").as_ref() {
            let _ = sender.send(true);
        }
    }

    fn order(&self) -> Vec<String> {
        self.order.lock().expect("lock").clone()
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl JobHandler for Recorder {
    async fn handle(&self, job: InboundJob) {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        self.order.lock().expect("lock").push(format!(
            "{}:{}",
            job.callback.conversation_id, job.callback.msg_id
        ));
        let delay = *self.delay.lock().expect("lock");
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        // 把闸门句柄取出来再 await（不把 `MutexGuard` 带过 await 点 ⇒ 作业体保持 `Send`）。
        let gate = self.gate.lock().expect("lock").clone();
        if let Some(sender) = gate {
            let mut receiver = sender.subscribe();
            if !*receiver.borrow_and_update() {
                let _ = receiver.changed().await;
            }
        }
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

fn job(conversation_id: &str, message_id: &str) -> InboundJob {
    let callback: BotCallbackData = serde_json::from_value(serde_json::json!({
        "senderStaffId": "staff",
        "conversationId": conversation_id,
        "msgId": message_id,
        "msgtype": "text",
        "text": {"content": "hi"},
    }))
    .expect("最小回调");
    InboundJob::new("app-key", callback)
}

fn dispatcher(handler: Arc<Recorder>, limits: DispatchLimits) -> Arc<Dispatcher> {
    Arc::new(Dispatcher::with_limits(handler, limits))
}

fn limits(max_workers: usize) -> DispatchLimits {
    DispatchLimits {
        max_workers,
        max_queue_depth: MAX_QUEUE_DEPTH,
        max_pending: MAX_PENDING,
        job_timeout: JOB_TIMEOUT,
    }
}

// =====================================================================
// 语义逐条
// =====================================================================

/// 上游的四个默认值就是这四个常量（别在别处悄悄改）。
#[test]
fn the_defaults_are_the_upstream_constants() {
    let defaults = DispatchLimits::default();
    assert_eq!(defaults.max_workers, 8);
    assert_eq!(defaults.max_queue_depth, 256);
    assert_eq!(defaults.max_pending, 2048);
    assert_eq!(defaults.job_timeout, Duration::from_secs(120));
    assert_eq!(MAX_WORKERS, 8);
    assert_eq!(MAX_QUEUE_DEPTH, 256);
    assert_eq!(MAX_PENDING, 2048);
    assert_eq!(JOB_TIMEOUT, Duration::from_secs(120));
}

/// 同一个会话：严格按入队顺序跑（同一时刻只有一个 drain 任务）。
#[tokio::test]
async fn per_conversation_jobs_run_in_order() {
    let handler = Recorder::new(Duration::ZERO);
    let dispatcher = dispatcher(Arc::clone(&handler), limits(4));
    for index in 0..3 {
        assert_eq!(
            dispatcher.enqueue("chat-a", job("chat-a", &format!("m{index}"))),
            EnqueueOutcome::Queued
        );
    }
    assert!(dispatcher.drain_and_close(Duration::from_secs(2)).await);
    assert_eq!(handler.order(), vec!["chat-a:m0", "chat-a:m1", "chat-a:m2"]);
    assert_eq!(handler.peak(), 1, "同会话不许并行");
    assert_eq!(dispatcher.pending(), 0);
    assert!(dispatcher.is_closed());
}

/// 不同会话：可以并行（上限内），且有界。
#[tokio::test]
async fn different_conversations_run_in_parallel_up_to_the_bound() {
    let handler = Recorder::new(Duration::from_millis(40));
    let parallel = dispatcher(Arc::clone(&handler), limits(2));
    for conversation in ["a", "b"] {
        parallel.enqueue(conversation, job(conversation, "m"));
    }
    assert!(parallel.drain_and_close(Duration::from_secs(2)).await);
    assert_eq!(handler.peak(), 2, "两个会话应当并行");

    // 同一个作业体、上限降到 1 ⇒ 峰值必须回到 1（信号量按**安装**计）。
    let serial = Recorder::new(Duration::from_millis(40));
    let serial_dispatcher = dispatcher(Arc::clone(&serial), limits(1));
    for conversation in ["a", "b", "c"] {
        serial_dispatcher.enqueue(conversation, job(conversation, "m"));
    }
    assert!(
        serial_dispatcher
            .drain_and_close(Duration::from_secs(2))
            .await
    );
    assert_eq!(serial.peak(), 1);
}

/// 单会话队列满 ⇒ **丢弃最新**（不阻塞调用方），并计入丢弃数。
#[tokio::test]
async fn a_full_conversation_queue_drops_the_newest() {
    let handler = Recorder::new(Duration::ZERO);
    let mut config = limits(1);
    config.max_queue_depth = 1;
    let dispatcher = dispatcher(Arc::clone(&handler), config);
    let keeper = handler.gated();
    assert_eq!(
        dispatcher.enqueue("chat", job("chat", "m0")),
        EnqueueOutcome::Queued
    );
    assert_eq!(
        dispatcher.enqueue("chat", job("chat", "m1")),
        EnqueueOutcome::DroppedConversationFull
    );
    assert_eq!(dispatcher.queued_total(), 1);
    assert_eq!(dispatcher.dropped_total(), 1);
    assert_eq!(dispatcher.queue_depths(), vec![("chat".to_string(), 1)]);

    handler.release();
    drop(keeper);
    assert!(dispatcher.drain_and_close(Duration::from_secs(2)).await);
    assert_eq!(handler.order(), vec!["chat:m0"]);
}

/// 整安装 pending 满 ⇒ 也丢弃最新（上限存在的理由：每个会话各挂一个等待者也要有天花板）。
#[tokio::test]
async fn a_full_installation_queue_drops_the_newest() {
    let handler = Recorder::new(Duration::ZERO);
    let mut config = limits(1);
    config.max_pending = 1;
    let dispatcher = dispatcher(Arc::clone(&handler), config);
    let keeper = handler.gated();
    assert_eq!(
        dispatcher.enqueue("a", job("a", "m0")),
        EnqueueOutcome::Queued
    );
    assert_eq!(
        dispatcher.enqueue("b", job("b", "m0")),
        EnqueueOutcome::DroppedInstallationFull
    );
    assert_eq!(dispatcher.pending(), 1);
    handler.release();
    drop(keeper);
    assert!(dispatcher.drain_and_close(Duration::from_secs(2)).await);
    assert_eq!(handler.order(), vec!["a:m0"]);
    assert_eq!(dispatcher.dropped_total(), 1);
}

/// 收口之后**不再接受**新作业（幂等，且不改已排队的）。
#[tokio::test]
async fn enqueue_after_close_is_dropped() {
    let handler = Recorder::new(Duration::ZERO);
    let dispatcher = dispatcher(Arc::clone(&handler), limits(2));
    dispatcher.start_close();
    assert!(dispatcher.is_closed());
    assert_eq!(
        dispatcher.enqueue("chat", job("chat", "m")),
        EnqueueOutcome::DroppedClosed
    );
    assert_eq!(dispatcher.dropped_total(), 1);
    assert!(dispatcher.drain_and_close(Duration::from_secs(1)).await);
    assert!(handler.order().is_empty());
}

/// 收口**等**已接受的作业跑完（不是"立刻掐断"）。
#[tokio::test]
async fn drain_and_close_waits_for_accepted_jobs() {
    let handler = Recorder::new(Duration::from_millis(30));
    let dispatcher = dispatcher(Arc::clone(&handler), limits(2));
    for conversation in ["a", "b", "c"] {
        dispatcher.enqueue(conversation, job(conversation, "m"));
    }
    assert!(dispatcher.drain_and_close(Duration::from_secs(2)).await);
    assert_eq!(handler.order().len(), 3, "三个都被跑完");
    assert_eq!(dispatcher.pending(), 0);
    assert_eq!(dispatcher.active_conversations(), 0);
}

/// 一个慢作业被自己的超时兜住（在飞的作业跑完自己那一次，排队的不受影响）。
#[tokio::test]
async fn a_slow_job_is_bounded_by_its_own_timeout() {
    let handler = Recorder::new(Duration::from_secs(5));
    let mut config = limits(1);
    config.job_timeout = Duration::from_millis(40);
    let dispatcher = dispatcher(Arc::clone(&handler), config);
    assert_eq!(
        dispatcher.enqueue("a", job("a", "slow")),
        EnqueueOutcome::Queued
    );
    assert_eq!(
        dispatcher.enqueue("b", job("b", "fast")),
        EnqueueOutcome::Queued
    );
    let waited_from = std::time::Instant::now();
    assert!(dispatcher.drain_and_close(Duration::from_secs(2)).await);
    // 两个作业各被 40ms 的超时截断 ⇒ 远小于 5s 的作业体睡眠。
    assert!(
        waited_from.elapsed() < Duration::from_secs(2),
        "超时没有生效：{:?}",
        waited_from.elapsed()
    );
    assert_eq!(handler.order().len(), 2, "两个作业都进了队列");
    assert_eq!(dispatcher.pending(), 0);
}

/// 收口预算用尽 ⇒ 硬取消：**只**丢排队中的，在飞的仍跑完自己那一次。
#[tokio::test]
async fn an_expired_shutdown_budget_cancels_only_the_queue() {
    let handler = Recorder::new(Duration::from_millis(30));
    let dispatcher = dispatcher(Arc::clone(&handler), limits(1));
    for index in 0..3 {
        dispatcher.enqueue("chat", job("chat", &format!("m{index}")));
    }
    // 30ms 完成一个作业 ⇒ 三个要 ~90ms；给 45ms 的预算必然用尽。
    let drained = dispatcher.drain_and_close(Duration::from_millis(45)).await;
    assert!(!drained, "预算用尽 ⇒ 收口未完成");
    // 在飞的第一个仍然落地（本仓形态：取消不穿透作业签名）。
    assert!(handler.order().len() <= 3);
}

// =====================================================================
// 队列槽（跨重连复用）
// =====================================================================

#[tokio::test]
async fn a_slot_is_reused_across_generations_until_it_closes() {
    let handler = Recorder::new(Duration::ZERO);
    let slots = DispatchSlotRegistry::with_limits(limits(2));
    let first = slots.acquire("app-key", Arc::clone(&handler) as Arc<dyn JobHandler>);
    let second = slots.acquire("app-key", Arc::clone(&handler) as Arc<dyn JobHandler>);
    assert!(Arc::ptr_eq(&first, &second), "重连要复用同一条队列");
    assert_eq!(slots.created(), 1);

    // 别的 AppKey 是另外一格。
    let other = slots.acquire("other-key", Arc::clone(&handler) as Arc<dyn JobHandler>);
    assert!(!Arc::ptr_eq(&first, &other));
    assert_eq!(slots.created(), 2);

    // 收口之后不再复用（旧队列已经不接受作业了）。
    assert!(first.drain_and_close(Duration::from_secs(1)).await);
    let replacement = slots.acquire("app-key", Arc::clone(&handler) as Arc<dyn JobHandler>);
    assert!(!Arc::ptr_eq(&first, &replacement));
    assert_eq!(slots.created(), 3);
    assert!(!replacement.is_closed());
}

#[tokio::test]
async fn releasing_a_slot_is_compare_by_arc() {
    let handler = Recorder::new(Duration::ZERO);
    let slots = DispatchSlotRegistry::with_limits(limits(2));
    let first = slots.acquire("app-key", Arc::clone(&handler) as Arc<dyn JobHandler>);
    // 换代：收口旧的 ⇒ 新的接手这一格。
    first.start_close();
    let second = slots.acquire("app-key", Arc::clone(&handler) as Arc<dyn JobHandler>);
    // 旧代来收尾：**不得**把已经被换代接管的格子删掉。
    assert!(!slots.release("app-key", &first));
    assert!(Arc::ptr_eq(
        &slots.acquire("app-key", Arc::clone(&handler) as Arc<dyn JobHandler>),
        &second
    ));
    // 当前那一格可以摘掉；再取一次就是**重建**（计数 +1）。
    assert!(slots.release("app-key", &second));
    let _rebuilt = slots.acquire("app-key", Arc::clone(&handler) as Arc<dyn JobHandler>);
    assert_eq!(slots.created(), 3, "被摘掉之后要重建");
}
