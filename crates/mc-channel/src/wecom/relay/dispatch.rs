//! `relay` 的**调度器**：三个接缝（发布 / 通知 / 在握 socket 的副本上执行）、一次投递的结论、
//! `RelayOutbound` 的全部方法、停机开关，以及"一条被路由的回复的最终结局由谁结算"。
//!
//! 本文件是 `relay.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。
//! `RelayOutbound` 的结构体本体留在 `relay.rs`（它的私有字段要被 `chain` / `queue` / `relayed`
//! 读到，而那些模块是 `relay` 的后代、不是 `relay::dispatch` 的后代）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::wecom::metrics::{or_nop_metrics, Metrics};

use crate::wecom::outbound::Outbound;

use super::queue::{Queued, SeenEvents, DEFAULT_SEEN_EVENTS};
use super::{
    dedupe_key, dedupe_ttl_for, ClaimState, DedupeStore, RelayConfig, RelayFrame, RelayKind,
    RelayOutbound, ShardQueue, DEFAULT_CLAIM_BUDGET,
};

// =====================================================================
// 发布与执行的两个接缝
// =====================================================================

/// 本包用到的那一片中继（上游 `relayPublisher`：`PublishWithID(scope, scopeID, exclude, frame, id)`）。
pub trait RelayPublisher: Send + Sync {
    /// 带 id 发布一帧。
    ///
    /// # Errors
    ///
    /// 传输层失败。
    fn publish_with_id(
        &self,
        scope_type: &str,
        scope_id: &str,
        exclude: &str,
        frame: &[u8],
        id: &str,
    ) -> Result<(), String>;
}

/// 上游发布用的 scope 名（`realtime.ScopeWecomOutbound`）。
pub const RELAY_SCOPE: &str = "wecom_outbound";

/// 中继的**唯一**一个发布面（上游 `noticeRouter`）。
///
/// 上游逐字：它是 `typing_indicator.go` 已经在用、也是 `outbound.rs` 唯一用到的那个方法；
/// 而且**具体类型**会让"这次结束到底有没有被路由"变成不可测 —— 那正是三个缺口藏身的问题。
pub trait NoticeRouter: Send + Sync {
    /// 发布一帧；返回"发出去了吗"。
    fn publish(&self, frame: &RelayFrame, event_id: &str) -> bool;
}

/// 在**握着 socket 的那个副本**上执行一次投递（上游 `relayHandler`）。
///
/// `Outbound` 实现它；这层间接正是本对象能在订阅者存在**之前**注册到中继上的原因。
#[async_trait]
pub trait RelayHandler: Send + Sync {
    /// 执行一次投递。
    async fn deliver_relayed(&self, frame: &RelayFrame) -> RelayResult;

    /// "本进程**此刻**握着那个安装的活连接吗"。它给全局 claim 设闸 —— 见
    /// [`RelayOutbound::perform`]。
    fn owns_socket(&self, installation_id: &str) -> bool;

    /// 施加一个完事帧欠下的记录（上游 `relayResult.record` 那个闭包）。
    ///
    /// 调用点是 [`RelayOutbound::perform`]，**在** claim 被结算之后。
    fn record(&self, record: RelayRecord);
}

// =====================================================================
// 结论
// =====================================================================

/// 一次投递尝试告诉调度器：这一帧**完事了吗**；以及对一个完事的帧，它的持有者欠哪个记录。
///
/// 记录与投递分开，是为了让它能在 claim 被**结算之后**才做：一个 `Settle` 被拒的持有者
/// （发布方已经把这条回复记成丢了）必须**什么都不记**，否则这条回复会以两个记录结束。
///
/// 上游把一个闭包装进 `relayResult.record`（Go 的闭包携带接收者）。本仓把它落成一个**值**：
/// `deliver_relayed` 拿的是 `&self`，而一个借用 `self` 的闭包活不到调度器结算 claim 的那一刻
/// ⇒ 记录必须能被**搬运**，再由 handler 上的 [`RelayHandler::record`] 施加。语义逐条不变
/// （见 `docs/32` §34 的 D10）。
#[derive(Debug, Clone, PartialEq)]
pub struct RelayResult {
    /// 这一帧的结局。
    pub outcome: RelayOutcome,
    /// 一个完事的帧欠下的那一次记账（`None` = 不欠）。
    pub record: Option<RelayRecord>,
}

impl RelayResult {
    /// 一个不需要记账的结论。
    #[must_use]
    pub fn new(outcome: RelayOutcome) -> Self {
        Self {
            outcome,
            record: None,
        }
    }

    /// 一个带记录的结论。
    #[must_use]
    pub fn recorded(outcome: RelayOutcome, record: RelayRecord) -> Self {
        Self {
            outcome,
            record: Some(record),
        }
    }
}

/// 一个完事的帧欠下的那一次记账（上游 `relayResult.record` 那个闭包所做的事）。
///
/// 由 [`RelayHandler::record`] 在 claim 被结算**之后**施加：一个 `Settle` 被拒的持有者必须
/// 什么都不记。
#[derive(Debug, Clone, PartialEq)]
pub enum RelayRecord {
    /// 一条回复到了用户那里（`outbound_delivered`）。
    Delivered,
    /// 一次发送的结局 —— **同一个** `record_send` 映射，所以中继路径与本地路径对"部分发送"
    /// 的看法一致（上游 #8344：它们过去不一致，而一个部分投递会不会惊动任何人就取决于哪个副本
    /// 恰好握着租约）。
    Send {
        session_id: String,
        event_type: String,
        error: Option<crate::wecom::ws_sender::SenderError>,
    },
    /// 一次收尾的结局**未知**。
    Unconfirmed {
        session_id: String,
        event_type: String,
        reason: String,
        error: Option<crate::wecom::ws_sender::SenderError>,
    },
    /// 中继过来的收件箱推送失败了。**只记日志**：收件箱推送不是一条回复，而回复计数器
    /// 的单位是"agent 回复"（上游逐字：把一条收件箱推送算进去会让"送达/丢弃"比随
    /// "哪个副本恰好握着 socket"而变）。
    InboxFailed { installation_id: String },
}

/// 告诉调度器 claim 可不可以被还回去（上游 `deliveryOutcome`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayOutcome {
    /// 本副本这个安装**没有 socket**。另一个副本会接过这一帧；claim 必须还回去。
    NotOurs,
    /// 送到了，或者以一种**不该重试**的方式失败了。
    Done,
    /// 一个字节都没上过线 ⇒ claim 还回去，一次重放或另一个副本可以再试。
    ///
    /// **故意不用于** ack 超时：`AckTimeout` 意味着帧很可能已经到了，而这个 adapter 的既定规则是
    /// "这样的发送，调用方自担重试的风险"。
    ProvablyNotSent,
}

/// 一条已经被路由、"端到端命运未知"的回复（上游 `pendingOutcome`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOutcome {
    pub key: String,
    pub session_id: String,
    pub installation_id: String,
    pub task_id: String,
    pub due_at: Instant,
}

/// 上游 `claimSettleAttempts`：持有者在放弃之前请存储结算它的 claim 几次。
pub const CLAIM_SETTLE_ATTEMPTS: usize = 3;

/// 上游 `settleBudgetSpent`：ctx 上有一个**界**（截止）已经过去了。
///
/// 裸的取消**不算**。停机打断工作，但帧已经在用户的聊里、它的 claim 仍然必须被结算
/// （所以存储调用会丢掉取消）。截止是相反的情况：[`RelayOutbound::drain_remaining`] 用**一个**
/// 截止界整次排空，而一条不断开新尝试的重试链会花掉停机已经承诺出去的时间。
/// 已经做过的尝试算数，新的一个都不开。
#[must_use]
pub fn settle_budget_spent(deadline: Option<Instant>, now: Instant) -> bool {
    matches!(deadline, Some(deadline) if now >= deadline)
}

/// 一条投递在飞时的那份预算（上游 `ctx` 的截止时刻那一半）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayBudget {
    deadline: Instant,
}

impl RelayBudget {
    /// 从某个时刻起 `budget` 之内。
    #[must_use]
    pub fn lasting(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
        }
    }

    /// 一个指定截止时刻的预算（用例把"已经花掉大半"写出来，不睡真觉）。
    #[must_use]
    pub fn at(deadline: Instant) -> Self {
        Self { deadline }
    }

    /// 截止时刻。
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// 到点了吗。
    #[must_use]
    pub fn exceeded(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

impl RelayOutbound {
    /// 建跨副本路由器。`cfg.replay_grace` 是中继配置的启动重放窗口，它给 claim 的 TTL 定尺
    /// （[`dedupe_ttl_for`]）。用 [`RelayOutbound::attach`] 给订阅者、用
    /// [`RelayOutbound::start`] 起 worker；在这两件事之前把它注册到中继上是安全的。
    #[must_use]
    pub fn new(
        publisher: Option<Arc<dyn RelayPublisher>>,
        dedupe: Option<Arc<dyn DedupeStore>>,
        cfg: RelayConfig,
    ) -> Self {
        let cfg = cfg.with_defaults();
        let retry_plan = cfg.retry_plan();
        let queues = (0..cfg.shard_count())
            .map(|_| Arc::new(ShardQueue::new(cfg.depth())))
            .collect();
        Self {
            publisher,
            dedupe,
            dedupe_ttl: dedupe_ttl_for(cfg.replay_grace),
            retry_plan,
            owner: new_owner_token(),
            seen: SeenEvents::new(DEFAULT_SEEN_EVENTS),
            pending: Mutex::new(Vec::new()),
            metrics: RwLock::new(None),
            attached: AtomicBool::new(false),
            attached_notify: tokio::sync::Notify::new(),
            handler: RwLock::new(None),
            cfg,
            queues,
            pending_capacity: cfg.depth(),
            pending_ready: tokio::sync::Notify::new(),
            start_seq: AtomicU64::new(0),
        }
    }

    /// 装健康信号汇。启动期调一次，在任何投递可能跑之前（worker 要等 `attach`），
    /// 所以唯一的并发读者是分片读者上的削减计数器。
    pub fn set_metrics(&self, metrics: &'static dyn Metrics) {
        if let Ok(mut slot) = self.metrics.write() {
            *slot = Some(metrics);
        }
    }

    /// 当前汇（没配就是 no-op）。
    #[must_use]
    pub fn mx(&self) -> &'static dyn Metrics {
        match self.metrics.read() {
            Ok(slot) => or_nop_metrics(*slot),
            Err(_) => or_nop_metrics(None),
        }
    }

    /// 配置（用例读它）。
    #[must_use]
    pub fn config(&self) -> &RelayConfig {
        &self.cfg
    }

    /// 重投递链（用例读它）。
    #[must_use]
    pub fn retry_plan(&self) -> &[Duration] {
        &self.retry_plan
    }

    /// claim 的 TTL。
    #[must_use]
    pub fn dedupe_ttl(&self) -> Duration {
        self.dedupe_ttl
    }

    /// 有没有 claim 存储（没有就没有中继：见 [`DedupeStore`]）。
    #[must_use]
    pub fn has_dedupe(&self) -> bool {
        self.dedupe.is_some()
    }

    /// 供给执行投递的 handler。调一次，在订阅者存在之后。
    ///
    /// `start` 之前的帧在队列里等（要求 3），而不是对着空槽被丢掉。
    pub fn attach(&self, handler: Arc<dyn RelayHandler>) {
        if let Ok(mut slot) = self.handler.write() {
            *slot = Some(handler);
        }
        self.attached.store(true, Ordering::SeqCst);
        // **两个都发**：`notify_waiters` 叫醒已经在等的 worker，`notify_one` 给一个**还没**
        // 开始等的 worker 存一个许可（否则"注册先于 attach、attach 先于 worker 起来"那条
        // 窗口会让它一直等下去 —— 而这正是上游要求 3 说的那个顺序）。
        self.attached_notify.notify_waiters();
        self.attached_notify.notify_one();
    }

    /// handler 到位了吗。
    #[must_use]
    pub fn handler_ready(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
    }

    /// 当前 handler（没到位就是 `None`）。
    #[must_use]
    pub fn handler(&self) -> Option<Arc<dyn RelayHandler>> {
        self.handler.read().ok().and_then(|slot| slot.clone())
    }

    /// 本进程的 claim token 前缀 + 一次投递的 id（上游 `tokenFor`）。
    #[must_use]
    pub fn token_for(&self, event_id: &str) -> String {
        format!("{}/{event_id}", self.owner)
    }

    /// 每片队列（诊断与用例读它）。
    #[must_use]
    pub fn queues(&self) -> &[Arc<ShardQueue>] {
        &self.queues
    }

    /// 上游 `shardFor`：一趟 FNV-1a 把一条安装映到一条队列上。
    ///
    /// [`ShardQueue`] 的文档说清了它买到的是什么（并发，而不是隔离）。
    #[must_use]
    pub fn shard_for(&self, installation_id: &str) -> usize {
        fnv1a(installation_id) as usize % self.queues.len().max(1)
    }

    /// 上游 `DeliverWecomOutbound`：**不许阻塞**。
    ///
    /// 调用方是共享的分片读循环，那条 shard 上别的一切都排在它后面。
    pub fn deliver_outbound(&self, scope_id: &str, frame: &[u8], event_id: &str) {
        match RelayFrame::decode(frame) {
            Ok(frame) => {
                let shard = self.shard_for(&frame.installation_id);
                let item = Queued::new(frame, event_id);
                if !self.queues[shard].push(item.clone()) {
                    // 削减而不是卡住 shard。**要计数**，因为一条没人收到的回复正是这整条路径
                    // 存在的意义：不再沉默。
                    self.shed(&item, "dispatch queue full");
                }
            }
            Err(message) => tracing::warn!(
                error = message.as_str(),
                installation_id = scope_id,
                "wecom relay: undecodable frame"
            ),
        }
    }

    /// 上游 `publish`：把一条投递交给别的副本。返回"发出去了吗"。
    pub fn publish(&self, frame: &RelayFrame, event_id: &str) -> bool {
        let Some(publisher) = self.publisher.as_ref() else {
            return false;
        };
        let Ok(body) = frame.encode() else {
            tracing::warn!("wecom relay: marshal outbound frame failed");
            return false;
        };
        if let Err(message) =
            publisher.publish_with_id(RELAY_SCOPE, &frame.installation_id, "", &body, event_id)
        {
            tracing::warn!(
                error = message.as_str(),
                installation_id = frame.installation_id.as_str(),
                task_id = frame.task_id.as_str(),
                "wecom relay: publish failed"
            );
            return false;
        }
        // 一条被发布的回复现在**有人欠它一个结局**。`watch_outcomes` 就是那个人 ——
        // 见 [`RelayOutbound::settle`] 的注释：为什么不能是别人。
        if frame.kind == RelayKind::Reply {
            self.await_outcome(frame, event_id);
        }
        true
    }

    /// 上游 `outcomeGrace`：一条被路由的回复在被判定"没人有 socket"之前能等多久。
    ///
    /// 它必须比**整条**重投递链长（一条还在重试的回复是在飞，不是丢了），再加上那些 offer
    /// 每次付的 claim 往返。
    #[must_use]
    pub fn outcome_grace(&self) -> Duration {
        // 一次 claim 往返**每 offer**，不是整条链一次。每一次重投递都自己 `Claim`，
        // 而每次都可能烧掉存储的整份预算 ⇒ 一个由"退避之和 + 一次往返"搭出来的宽，会在一慢存储
        // 上链还在跑的时候就到期，而这个观察者接着会记一次下一次尝试用投递反驳掉的丢失。
        // 按最坏情况定尺的代价是健康存储上一次更晚的 settle —— 那是无害的方向。
        let budget = self
            .dedupe
            .as_ref()
            .map_or(DEFAULT_CLAIM_BUDGET, |dedupe| dedupe.claim_budget());
        // **两次**往返每 offer，不是一次。每一次 offer 都自己 `Claim`，而每一次 offer 都在**同一份
        // 预算**上以第二次调用结束：该再 offer 一次时是 `Release`，完事时是 `Settle`。
        // 只数 `Claim` 会把存储那一半时间漏出算式，而 absent 状态**刻意不被 `Resolve` 围栏**
        // （在那里过早过期会被记成一次丢失，而一次更晚的 offer 仍然可能认领、投递并结算 ——
        // 那正是这整条路径存在要消灭的矛盾）。
        let offers = self.retry_plan.len() + 1;
        let offers_hint = u32::try_from(offers).unwrap_or(u32::MAX);
        let mut total = budget.saturating_mul(2 * offers_hint);
        for delay in &self.retry_plan {
            total += *delay;
        }
        // 完事那次 offer 上的结算重试：它多出来的尝试与它们之间的等待。
        let settle_pauses =
            u64::try_from((budget + self.settle_retry_backoff()).as_millis()).unwrap_or(u64::MAX);
        total += Duration::from_millis(
            settle_pauses.saturating_mul(u64::try_from(CLAIM_SETTLE_ATTEMPTS - 1).unwrap_or(0)),
        );
        // 再加上**每 offer 一次投递**，不是整条链一次。`perform` 给每一次拿到 claim 的投递一份
        // 自己的预算，而烧掉一整份预算的那个失败也正是把 claim 还回去的那个失败：一次 offer
        // 因为聊忙等到自己的预算用完、带着 `chat_busy`（= 可证明没发出）回来 ⇒ claim 被释放、
        // 帧带着一份**全新**预算被再 offer 一次。把一个投递记在整条链上，就是在给一条**不可能
        // 发生**的链定尺（每次 offer 的退避加上一次 offer 的投递），而在默认值上那是"5s 的宽"
        // 对"这条链能花的 60s"。
        //
        // 落在一次活着的 offer 里的 `Resolve` 就是全部代价：它在同一个操作里把键围栏成 lost，
        // 于是稍后回来的持有者什么都不记，而计数器已经说这条回复被丢了 —— 一条回复，
        // 同时被数成丢了与送到了。
        //
        // 另一个方向——一次投递活得比 `perform` 给它的预算更久——不可能发生：**它就是**那份预算，
        // 施加在投递跑的那个预算上，所以一条被切成好几帧的回答是把预算花在它们**全部**上，
        // 而不是每片各拿一次 ack 等待。
        total += self.cfg.delivery_budget().saturating_mul(offers_hint);
        total
    }

    /// 上游 `settleRetryBackoff`：结算重试之间的节奏。用 claim 层自己的旋钮，而不是新造一个。
    #[must_use]
    pub fn settle_retry_backoff(&self) -> Duration {
        self.cfg.retry_backoff()
    }

    /// 上游 `awaitOutcome`：把一条被路由的回复登记给 [`RelayOutbound::watch_outcomes`]。
    /// **从不阻塞**：调用方是一个总线订阅者的任务。
    pub fn await_outcome(&self, frame: &RelayFrame, event_id: &str) {
        if self.dedupe.is_none() {
            return;
        }
        let pending = PendingOutcome {
            key: dedupe_key(event_id),
            session_id: frame.session_id.clone(),
            installation_id: frame.installation_id.clone(),
            task_id: frame.task_id.clone(),
            due_at: Instant::now() + self.outcome_grace(),
        };
        let mut queue = match self.pending.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len() >= self.pending_capacity {
            tracing::warn!(
                installation_id = frame.installation_id.as_str(),
                task_id = frame.task_id.as_str(),
                "wecom relay: outcome watch full, a routed reply's fate will go unrecorded"
            );
            return;
        }
        // 宽是常量 ⇒ 这个向量天然有序（上游逐字）。
        queue.push(pending);
        drop(queue);
        self.pending_ready.notify_waiters();
    }

    /// 当前登记了几条待观察的回复（诊断与用例读它）。
    #[must_use]
    pub fn pending_count(&self) -> usize {
        match self.pending.lock() {
            Ok(queue) => queue.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// 上游 `settle`：问"到底有没有人认领过这条被路由的回复"，没人的话记下这次丢失。
    ///
    /// [`RelayOutbound::watch_outcomes`] 是它的唯一调用者。
    pub async fn settle(&self, pending: &PendingOutcome) {
        let Some(dedupe) = self.dedupe.as_ref() else {
            return;
        };
        let state = match dedupe.resolve(&pending.key).await {
            Ok(state) => state,
            Err(message) => {
                // 读不出来的 claim 存储两边都证明不了。在这里说"丢了"会把一次 Redis 抖动
                // 变成一支幻影丢弃大军。
                tracing::warn!(
                    error = message.as_str(),
                    installation_id = pending.installation_id.as_str(),
                    task_id = pending.task_id.as_str(),
                    "wecom relay: could not settle a routed reply's outcome"
                );
                return;
            }
        };
        match state {
            // 它的持有者记了结局，或者**已经被处理过** —— 一次重新发布的完成会撞上同一个键。
            // 两种都让这一趟保持安静。
            ClaimState::Settled | ClaimState::Lost => {}
            ClaimState::Held => {
                // 一个副本拿了它、从没结算：它的释放或结算在线上丢了，或者那个进程跟着一起走了。
                // `Resolve` 刚刚把键围栏成 lost，于是稍后回来的持有者发现它的 `Settle` 被拒、
                // 什么都不记；**这一个**记录就是这条回复的记录。
                self.mx()
                    .record_outbound_dropped(crate::wecom::outcome::DropReason::Transport.as_str());
                tracing::warn!(
                    reason = crate::wecom::outcome::DropReason::Transport.as_str(),
                    chat_session_id = pending.session_id.as_str(),
                    installation_id = pending.installation_id.as_str(),
                    task_id = pending.task_id.as_str(),
                    detail = "a replica claimed the delivery and never recorded an outcome",
                    "wecom outbound: reply not delivered"
                );
            }
            ClaimState::Absent => {
                self.mx().record_outbound_dropped(
                    crate::wecom::outcome::DropReason::NoLiveConnection.as_str(),
                );
                tracing::warn!(
                    reason = crate::wecom::outcome::DropReason::NoLiveConnection.as_str(),
                    chat_session_id = pending.session_id.as_str(),
                    installation_id = pending.installation_id.as_str(),
                    task_id = pending.task_id.as_str(),
                    detail = "routed to the replicas and none took the delivery",
                    "wecom outbound: reply not delivered"
                );
            }
        }
    }

    /// 上游 `watchOutcomes`：一条被路由的回复**最终结局**的唯一所有者。
    ///
    /// 别人都不可能是。发布方把帧交给中继就返回；每个读到它、又没握着 socket 的副本静默且正确地
    /// 返回、什么都不计；真正投递的那个副本计那次投递。所以当**没有一个**副本握着 socket 时
    /// （全都在重连中），这条回复**在每一方都表现正确**的情况下丢了，而没有任何计数器移动。
    /// 那就是 `SELF_HOSTING.md` 描述、而此前没有任何东西能给它定尺的窗口。
    ///
    /// claim 键正是让它事后可观测的东西：谁投递谁认领（那个副本的 token 下），只有**证明了**
    /// 自己没发出任何东西的副本才释放（比较并删除），而持有者一旦记了结局就结算。
    /// 所以一旦重投递链不可能还在跑，`Resolve` 读到四件事之一：absent（这条回复谁都没到）、
    /// settled（它的持有者数过了）、lost（同一键的早前一趟已经处理）、或者仍被某个 token 握着
    /// —— 一个从没能记下任何东西的持有者，而 `Resolve` 在同一个操作里把它围栏成 lost，
    /// 于是持有者即使回来也什么都不记。**每条被路由的回复一次 Lua 调用**，而被路由的回复
    /// 是离租约的少数派。
    pub async fn watch_outcomes(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        if self.dedupe.is_none() {
            // 没有 claim 存储就也没有中继（见 [`DedupeStore`]）⇒ 没有任何被路由的回复的结局
            // 需要被问。
            let _ = shutdown.changed().await;
            return;
        }
        loop {
            let wait = {
                let queue = match self.pending.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                match queue.first() {
                    Some(first) => first.due_at.saturating_duration_since(Instant::now()),
                    // 没有待办就睡一个很长的觉；一次新登记会 `notify_waiters` 把它叫醒。
                    None => Duration::from_secs(3600),
                }
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {
                    let due = {
                        let mut queue = match self.pending.lock() {
                            Ok(queue) => queue,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        let now = Instant::now();
                        let split = queue.iter().position(|pending| pending.due_at > now).unwrap_or(queue.len());
                        queue.drain(..split).collect::<Vec<_>>()
                    };
                    for pending in due {
                        self.settle(&pending).await;
                    }
                }
                () = self.pending_ready.notified() => {}
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        // 停机**不是**证据。一条还在宽里的回复仍可能被持有它的那个副本投递出去，
                        // 而本进程正在离开；在这里数它，会报一次**没发生**的丢失。
                        let remaining = self.pending_count();
                        if remaining > 0 {
                            tracing::info!(
                                count = remaining,
                                "wecom relay: shutting down with routed replies still unresolved"
                            );
                        }
                        return;
                    }
                }
            }
        }
    }

    /// 起 worker：每条分片一个，另加一个结局观察者。
    ///
    /// 返回的 [`RelayHandle`] 拥有停机开关，并在进程要退出时等它们收尾（上游 `Start` + `Wait`）。
    #[must_use]
    pub fn start(self: &Arc<Self>) -> RelayHandle {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mut joins = Vec::new();
        for shard in 0..self.queues.len() {
            let relay = Arc::clone(self);
            let shutdown = shutdown_rx.clone();
            joins.push(tokio::spawn(async move {
                relay.work(shard, shutdown).await;
            }));
        }
        let observer = Arc::clone(self);
        let shutdown = shutdown_rx;
        joins.push(tokio::spawn(async move {
            observer.watch_outcomes(shutdown).await;
        }));
        self.start_seq.fetch_add(1, Ordering::SeqCst);
        RelayHandle {
            shutdown: shutdown_tx,
            joins,
        }
    }

    /// 上游 `handlerReady`：等 `attach` 跑过，并报"到底有没有 handler 可以投递"。
    pub async fn handler_ready_wait(
        &self,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> bool {
        if self.handler_ready() {
            return true;
        }
        tokio::select! {
            () = self.attached_notify.notified() => true,
            _ = shutdown.changed() => {
                // 在 handler 到达之前（或到达之时）被取消。如果它**已经**附着
                // （select 在两个就绪通道之间是任意的），队列里的东西仍然能被排空；
                // 没有它就没有任何东西可以拿来投递。
                self.handler_ready()
            }
        }
    }
}

/// 上游 `Start` 交回来的停机开关 + 收尾等待。
#[derive(Debug)]
pub struct RelayHandle {
    shutdown: tokio::sync::watch::Sender<bool>,
    joins: Vec<tokio::task::JoinHandle<()>>,
}

impl RelayHandle {
    /// 请求停机（worker 会先把队列里已经有的东西排空）。
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// 等每个 worker 停下（上游 `Wait`）。测试与停机路径用。
    pub async fn wait(self) {
        for join in self.joins {
            let _ = join.await;
        }
    }
}

impl Outbound {
    /// 上游 `WithRelay`：把跨副本路由器接到出站订阅者上。
    ///
    /// 没有它，订阅者保持它原来的行为：离租约产出的回复**就地**被丢掉。
    #[must_use]
    pub fn with_relay(mut self, relay: Arc<dyn NoticeRouter>) -> Self {
        self.set_relay(relay);
        self
    }
}

/// 一个进程级唯一的所有者片段（上游 `newReqID` 的**形态**；本片不复用帧层的 `new_req_id`，
/// 因为那个属于 aibot 帧的 `req_id` 约定）。
fn new_owner_token() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{pid:x}-{nanos:x}")
}

/// FNV-1a（上游 `fnv.New32a`；本仓不许新增依赖 ⇒ 三十行的手写版本）。
fn fnv1a(value: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in value.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
