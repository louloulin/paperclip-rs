//! **一条回复怎么到达那个能发出它的副本**（上游 `internal/integrations/wecom/relay_outbound.go`，
//! **1,578 行**）。
//!
//! - **写者**：M7-17（`LUM-1782` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：`WeCom` 是**唯一**一个没有出站 REST 的渠道：每一次写都走
//!   aibot WebSocket，而 WS 租约意味着**恰好一个**副本握着它。与此同时 `chat:done` 由服务了
//!   daemon 的 `POST /tasks/{id}/complete` 的那个副本发布到进程内事件总线 —— 那是一次负载均衡
//!   决定。离了租约，`outbound.rs` 过去除了丢掉这条回复无事可做（GH #7215、#6890）。
//!
//! # 本仓的替身：进程内队列（`docs/60` §2.5 的 R-M7-1）
//!
//! 上游把这件工作交给仓库本来就为这个形状的问题跑着的 Redis Stream 中继。本仓**没有 Redis
//! 依赖**（`docs/60` §2.5 的判据：那四处 Redis 用途全部有"无 Redis 时"的降级路径 ⇒ 这一处是
//! **换部署形态**、不是伪造行为）⇒ 本片把中继落成**进程内**的：
//!
//! | 上游 | 本仓 | 形态差异 |
//! | --- | --- | --- |
//! | `realtime.ShardedStreamRelay`（跨副本 Redis Stream） | [`RelayPublisher`] 端口（本片**不**给生产实现） | 见 D1：单副本部署里中继**本来就不需要** |
//! | `NewRedisDedupe`（跨副本 claim） | [`InProcessDedupe`]（`Mutex<HashMap>` + TTL） | 见 D2 |
//! | `dedupe == nil ⇒ 没有中继` | 同一判据（[`RelayOutbound::new`] 收 `Option<Arc<dyn DedupeStore>>`） | —— |
//! | `context.Context` 的取消/截止 | [`RelayBudget`] 与 `Instant` 记账 | 同 M7-16 的 D5 |
//!
//! 🔴 **单副本部署契约**（写进 `docs/32` §34，是 R-M7-1 的正面那一半）：进程内替身意味着中继
//! **只在这一进程内**成立 ⇒ 生产部署契约 = **单副本**，或者"渠道连接只在一个副本上开"。
//!
//! # 那份机制对它的消费者提出的**三条**要求（上游逐字，本仓逐条照做）
//!
//! 1. **它会重放。** 上游的 shard reader 从 `(now - ReplayGrace)` 开始读，而不是 `$` ⇒ 一个 pod
//!    下线期间发布的事件会被重新读到。**进程内替身不重放**（没有可重放的日志），但 claim 的
//!    **形状**照旧：声明按"这一轮"为键（[`relay_event_id`]）、寿命远长于重放窗口
//!    （[`dedupe_ttl_for`]）⇒ "同一份完成被发布两次"（发布重试、第二个订阅者、宿主自己的重放）
//!    在这条路径上**仍然是同一条 claim**，不会变成聊天里的第二条消息。
//! 2. **它是同步调的。** 上游 shard read loop 与我们这条路径共用，而我们的工作是**一次网络
//!    往返**（等平台的判决，最多 `ACK_TIMEOUT`）。内联做会让一个不健康的 bot 卡住那条 shard 上的
//!    一切 ⇒ [`RelayOutbound::deliver_outbound`] **只把帧交给一条有界队列就返回**。
//! 3. **它的读者起得早。** 因此**注册**必须能在发送者注册表与订阅者存在**之前**完成：本对象先建
//!    先注册，[`RelayOutbound::attach`] 之后再供给 handler（`start` 之前的帧在队列里等，
//!    而不是对着一个空槽被丢掉）。
//!
//! # 顺序（**这一节是上游全部设计的理由**）
//!
//! 一个安装的每一条回复由**一条**队列与**一个** worker 承载，按到达顺序 —— **包括跨一次重投递**。
//! 一个需要再试一次的帧**不**回队列尾部；它停在自己那条安装线的**队首**，它后面的一切都等，
//! 因为"两条回答以错的顺序到达同一个人面前"正是这份顺序存在要防的缺陷。
//!
//! 分片是**并发**的界，不是隔离的保证：两个撞到同一片的 bot **确实**会互相等。片数买到的是
//! "一个慢 bot 无法占满每个 worker"。
//!
//! # 文件分工（`docs/60` §6.3 的强制拆分：1,578 行 ⇒ 按「重投递链 / 优先级队列」拆）
//!
//! | 本目录 | 上游 | 内容 |
//! | --- | --- | --- |
//! | `relay.rs`（本文件） | `relay_outbound.go` 的调度核心 | 配置 / 帧 / claim 端口 / handler 端口 / 端到端结局 / 启动 |
//! | `relay/chain.rs` | `relay_outbound.go` 的 re-offer 那一半 | 重投递链、安装线、收尾排空 |
//! | `relay/queue.rs` | `relay_outbound.go` 的分片那一半 | `SeenEvents`、每片的队列与 worker、准入与削减 |
//! | `relay/relayed.rs` | `relay_outbound.go` 的 `deliverRelayed` 那一半 | 在握 socket 的副本上执行 |
//! | `relay/tests.rs` | `relay_*_test.go` | **幂等**与**顺序**的用例 |
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 帧里驮的是**标识**（安装 / 聊 / task / 会话），不是渲染好的载荷 —— 凡是租约持有者自己读得到
//! 的东西都按 id 传（附件由**要发它的那个副本**去取，而不是走中继）。本文件没有任何凭据字段。

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::engine::DEFAULT_POLL_INTERVAL;
use crate::wecom::metrics::Metrics;
use crate::wecom::ws_sender::ACK_TIMEOUT;

mod chain;
mod claims;
mod dispatch;
mod frame;
mod queue;
mod relayed;
#[cfg(test)]
mod tests;

pub use chain::{earliest_due, Hold, Lines};
pub use claims::{
    ClaimState, Clock, DedupeStore, InProcessDedupe, CLAIM_LOST_VALUE, CLAIM_SETTLED_VALUE,
};
pub use dispatch::{
    settle_budget_spent, NoticeRouter, PendingOutcome, RelayBudget, RelayHandle, RelayHandler,
    RelayOutcome, RelayPublisher, RelayRecord, RelayResult, CLAIM_SETTLE_ATTEMPTS, RELAY_SCOPE,
};
pub use frame::{dedupe_key, relay_event_id, relay_inbox_event_id, RelayFrame, RelayKind};
pub use queue::{Queued, SeenEvents, ShardQueue, DEFAULT_SEEN_EVENTS};
pub use relayed::{wordless_seal_copy, SEAL_REASON_CANCELLED, SEAL_REASON_NO_REPLY};

// =====================================================================
// 调度器
// =====================================================================

/// 把一条投递发布给**别的**副本，并执行别的副本发布的那些（上游 `RelayOutbound`）。
/// 每进程一个。
pub struct RelayOutbound {
    publisher: Option<Arc<dyn RelayPublisher>>,
    dedupe: Option<Arc<dyn DedupeStore>>,
    dedupe_ttl: Duration,
    cfg: RelayConfig,
    retry_plan: Vec<Duration>,
    /// 本进程在每一条 claim token 里的那一半（[`RelayOutbound::token_for`]）。每个
    /// `RelayOutbound` 铸一次，这样一条 claim 能把自己的重投递与**另一个**副本的区分开，
    /// 而且只区分这个。
    owner: String,
    seen: SeenEvents,
    /// 被路由的回复的 id，带给结局观察者 —— "到底有没有人投递了它"的**唯一**所有者。
    /// 有界、且是**削减**而不是阻塞：发布方跑在一个总线订阅者的任务上。
    pending: Mutex<Vec<PendingOutcome>>,
    metrics: RwLock<Option<&'static dyn Metrics>>,
    attached: AtomicBool,
    attached_notify: tokio::sync::Notify,
    handler: RwLock<Option<Arc<dyn RelayHandler>>>,
    queues: Vec<Arc<ShardQueue>>,
    pending_capacity: usize,
    pending_ready: tokio::sync::Notify,
    start_seq: AtomicU64,
}

impl fmt::Debug for RelayOutbound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayOutbound")
            .field("shards", &self.queues.len())
            .field("has_publisher", &self.publisher.is_some())
            .field("has_dedupe", &self.dedupe.is_some())
            .field("dedupe_ttl", &self.dedupe_ttl)
            .finish_non_exhaustive()
    }
}

// =====================================================================
// 配置
// =====================================================================

/// 分片数的默认值（上游 `defaultRelayShards`）。
pub const DEFAULT_RELAY_SHARDS: usize = 8;

/// 每片队列深度（上游 `defaultRelayQueueDepth`）。
pub const DEFAULT_RELAY_QUEUE_DEPTH: usize = 256;

/// 整次停机排空的预算（上游 `defaultRelayDrainBudget`）。
pub const DEFAULT_RELAY_DRAIN_BUDGET: Duration = Duration::from_secs(10);

/// 重投递链第一次等待的默认值（上游 `defaultRelayRetryBackoff`）。
pub const DEFAULT_RELAY_RETRY_BACKOFF: Duration = Duration::from_millis(200);

/// 一次 claim 往返的默认预算（上游 `defaultClaimBudget`，在 `dedupe_redis.go`）。
///
/// 进程内替身快到几乎为零，但**这个数必须存在**：`outcome_grace` 每次 offer 都付它一次，
/// 而一份由调度器私藏的拷贝会自由地与真正生效的超时漂开（上游逐字）。
pub const DEFAULT_CLAIM_BUDGET: Duration = Duration::from_millis(50);

/// 一条 claim 寿命的下限（上游 `minDedupeTTL`）。
pub const MIN_DEDUPE_TTL: Duration = Duration::from_secs(3600);

/// 重投递链最多几节（上游 `relayRetryChainCap`）：一个病态配置不许产出无界的链。
pub const RELAY_RETRY_CHAIN_CAP: usize = 24;

/// 上游 `dedupeTTLFor`：`max(minDedupeTTL, 2×replayGrace)`。
///
/// 两次宽的宽，是为了让一条声明舒服地活过它守的那个窗口（那条窗口是**运维旋钮**，
/// 不是常量 ⇒ TTL 也不能是）；下限是为了一个极小的宽不会产出比默认一直有的那一小时更短的声明。
#[must_use]
pub fn dedupe_ttl_for(replay_grace: Duration) -> Duration {
    let twice = replay_grace.saturating_mul(2);
    if twice > MIN_DEDUPE_TTL {
        twice
    } else {
        MIN_DEDUPE_TTL
    }
}

/// 调度器的尺寸（上游 `RelayConfig`）。
///
/// **没有一格是口味**：每一格背后都有一个运维界，写在它的文档里；而两个界在**包外**的
/// （中继的重放窗口、监管器的租约轮询间隔）是**传进来的**，不是猜出来的。
///
/// `None` 取它的文档默认值（[`RelayConfig::with_defaults`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RelayConfig {
    /// 一次投递尝试在**拿到 claim 之后**最多能花多久；它界的是**整条逻辑投递**：等目标聊的轮次，
    /// 加上一条长回答被切成的每一段各自的 ack 等待。`None` = [`ACK_TIMEOUT`]。
    ///
    /// 整条一个预算，因为那正是发布方的结局宽（[`RelayOutbound::outcome_grace`]）为一次 offer
    /// 留下的量 —— 一个为"一个 ack"而设的宽，配上一个要等好几段的投递，就是一次把持有者还在写的
    /// 回复**围栏**掉的 `Resolve`。
    pub delivery_budget: Option<Duration>,
    /// 几条独立的队列承载帧。它是**并发**的界而不是顺序的界（顺序来自哈希把一条安装放到一条
    /// 队列上）。要对齐的数字是"同时能慢几个 bot，才会让一个健康的 bot 排在它们后面"。
    pub shards: Option<usize>,
    /// **每片**的深度。它必须吸收的突发是一次重启的重放：分片读者从 `(now - ReplayGrace)`
    /// 打开、把那个窗口里发布的一切一次交过来。
    pub queue_depth: Option<usize>,
    /// 界的是**整次**停机后的排空（不是一次投递）。它的界在包外：排空跑在进程自己的停机里，
    /// 所以它得装得下渠道监管器的 `ShutdownTimeout`（`engine::Config`，默认 15s）。
    pub drain_budget: Option<Duration>,
    /// 中继配置的启动回看窗口。它给 claim 的 TTL 定尺（[`dedupe_ttl_for`]），
    /// 而且它是一个**运维旋钮**（`REALTIME_RELAY_REPLAY_GRACE`）—— 这正是它是个形参的原因。
    pub replay_grace: Duration,
    /// 一次 WebSocket 租约搬到另一个副本要多久。它给重投递链定尺，界是渠道监管器的
    /// `PollInterval`（默认 30s）：一个副本在**下一次扫**时才知道它该持有某个安装，
    /// 所以一次在搬家中间到达的帧至少要被重投递这么久。
    pub lease_settle: Option<Duration>,
    /// 两次重投递之间的**第一次**等待；链从这里翻倍，每节封顶在 `lease_settle / 4`，
    /// 这样一个提前落地的租约不必被坐等。故意很小：一次重投递成功的常见理由就是搬家已经做完了。
    pub retry_backoff: Option<Duration>,
}

impl RelayConfig {
    /// 取默认值（上游 `withDefaults`）。
    #[must_use]
    pub fn with_defaults(self) -> Self {
        Self {
            delivery_budget: self.delivery_budget,
            shards: self
                .shards
                .filter(|value| *value > 0)
                .or(Some(DEFAULT_RELAY_SHARDS)),
            queue_depth: self
                .queue_depth
                .filter(|value| *value > 0)
                .or(Some(DEFAULT_RELAY_QUEUE_DEPTH)),
            drain_budget: self
                .drain_budget
                .filter(|value| *value > Duration::ZERO)
                .or(Some(DEFAULT_RELAY_DRAIN_BUDGET)),
            replay_grace: self.replay_grace,
            lease_settle: self
                .lease_settle
                .filter(|value| *value > Duration::ZERO)
                .or(Some(DEFAULT_POLL_INTERVAL)),
            retry_backoff: self
                .retry_backoff
                .filter(|value| *value > Duration::ZERO)
                .or(Some(DEFAULT_RELAY_RETRY_BACKOFF)),
        }
    }

    /// 一次投递尝试的预算（取默认后）。
    #[must_use]
    pub fn delivery_budget(&self) -> Duration {
        self.delivery_budget.unwrap_or(ACK_TIMEOUT)
    }

    /// 片数（取默认后）。
    #[must_use]
    pub fn shard_count(&self) -> usize {
        self.shards.unwrap_or(DEFAULT_RELAY_SHARDS)
    }

    /// 每片深度（取默认后）。
    #[must_use]
    pub fn depth(&self) -> usize {
        self.queue_depth.unwrap_or(DEFAULT_RELAY_QUEUE_DEPTH)
    }

    /// 排空预算（取默认后）。
    #[must_use]
    pub fn drain_budget(&self) -> Duration {
        self.drain_budget.unwrap_or(DEFAULT_RELAY_DRAIN_BUDGET)
    }

    /// 租约落定窗口（取默认后）。
    #[must_use]
    pub fn lease_settle(&self) -> Duration {
        self.lease_settle.unwrap_or(DEFAULT_POLL_INTERVAL)
    }

    /// 第一次重投递等待（取默认后）。
    #[must_use]
    pub fn retry_backoff(&self) -> Duration {
        self.retry_backoff.unwrap_or(DEFAULT_RELAY_RETRY_BACKOFF)
    }

    /// 上游 `retryPlan`：重投递链，从落定窗口**算出来**而不是写下来。
    ///
    /// 延迟从 [`Self::retry_backoff`] 翻倍，每节封顶在 `lease_settle / 4`，而链长到它的**总时长**
    /// 覆盖一个半落定窗口 —— 搬家本身，加上紧随它的重连与订阅的那半份。
    ///
    /// 两重有界（[`RELAY_RETRY_CHAIN_CAP`]），免得一个病态配置产出无界的链。
    #[must_use]
    pub fn retry_plan(&self) -> Vec<Duration> {
        let settle = self.lease_settle();
        let target = settle + settle / 2;
        let backoff = self.retry_backoff();
        let mut cap = settle / 4;
        if cap < backoff {
            cap = backoff;
        }
        let mut plan = Vec::new();
        let mut total = Duration::ZERO;
        let mut delay = backoff;
        while total < target && plan.len() < RELAY_RETRY_CHAIN_CAP {
            if delay > cap {
                delay = cap;
            }
            plan.push(delay);
            total += delay;
            delay = delay.saturating_mul(2);
        }
        plan
    }
}
