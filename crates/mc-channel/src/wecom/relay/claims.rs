//! `relay` 的**claim 存储**：跨重启、跨副本的至多一次认领（上游 `DedupeStore`）
//! 与它的 Redis 进程内替身（`docs/60` §2.5 的 R-M7-1）。
//!
//! 本文件是 `relay.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{DEFAULT_CLAIM_BUDGET, MIN_DEDUPE_TTL};

// =====================================================================
// claim 存储（上游 `DedupeStore`，Redis 的进程内替身）
// =====================================================================

/// 上游 `claimState`：一次 `Resolve` 从一个 claim 键上读到的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// 没人握着它：从没被认领、被释放、或者过期了。
    Absent,
    /// 一个副本拿了它、还没记任何结局。`Resolve` 在报告它的**同时**把它翻成 [`Self::Lost`]。
    Held,
    /// 它的持有者记了结局。
    Settled,
    /// 发布方把它记成丢了。
    Lost,
}

/// 一个键不再是 token 之后所持有的值（上游 `claimSettledValue`）。
pub const CLAIM_SETTLED_VALUE: &str = "settled";

/// 一个键被发布方记成丢了之后所持有的值（上游 `claimLostValue`）。
///
/// 一个 token 永不与它们相撞：它是十六进制加一个斜杠（[`RelayOutbound::token_for`]）。
pub const CLAIM_LOST_VALUE: &str = "lost";

/// 一次**跨重启、跨副本**的至多一次认领（上游 `DedupeStore`）。
///
/// 生产里由 Redis 支撑（`NewRedisDedupe`）；`None` 只留下进程内那道闸，而那对一个单副本部署是
/// **正确**的，因为那里中继根本不会被用到。
#[async_trait]
pub trait DedupeStore: Send + Sync {
    /// 取 `key` 给 `token`；`key` 已经握着 `token` 时**再取一次**（同一个持有者在一个结果未知的
    /// `Release` 之后回来）。跨进程原子。token 是这里每一个操作都能安全重试的原因：
    /// 一条重发的命令永远不可能作用在一个更晚的持有者已经取走的 claim 上。
    ///
    /// # Errors
    ///
    /// 存储层故障（结局未知）。
    async fn claim(&self, key: &str, token: &str, ttl: Duration) -> Result<bool, String>;

    /// 把 `token` 握着的 claim 还回去，给一次**可证明没发生**的投递：一次比较并删除。
    /// `false` + 无错误 = 键不再握着这个 token（删除已经落地、claim 过期、或者现在别人握着它）
    /// —— **不是**失败。非 `Err` 意味着**结局未知**：那次删除可能落地了、也可能没有。
    ///
    /// # Errors
    ///
    /// 存储层故障（结局未知）。
    async fn release(&self, key: &str, token: &str) -> Result<bool, String>;

    /// 把 `token` 握着的 claim 标成已结算：它的持有者**即将**记这次投递的结局。
    /// `false` = 键不再握着 `token`（发布方已经把它解成丢了，或者它过期了），那持有者就
    /// **什么都不得记**，于是这条回复只以一个记录结束。结算一个已经结算的 claim 报 `true`
    /// （重试是安全的）。
    ///
    /// # Errors
    ///
    /// 存储层故障（结局未知）。
    async fn settle(&self, key: &str, token: &str) -> Result<bool, String>;

    /// 发布方在宽结束时的读，也是它的**围栏**：那一刻仍被某个 token 握着的 claim 会在**同一次
    /// 操作里**被翻成 [`ClaimState::Lost`]，于是一个更晚回来的持有者会发现它的 `Settle` 被拒、
    /// 从而什么都不记。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn resolve(&self, key: &str) -> Result<ClaimState, String>;

    /// 上面**一次**往返最长能花多久。存储自己说出这个数，因为存储自己执行它：
    /// [`RelayOutbound::outcome_grace`] 每次 offer 都付这份预算一次，而一份由调度器私藏的拷贝
    /// 会自由地与真正生效的超时漂开。
    fn claim_budget(&self) -> Duration;
}

/// 时钟（用例注入，别睡真觉）。
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Redis 的**进程内替身**（R-M7-1；`docs/32` §34 的 D2）。
///
/// 语义逐条照上游：`claim` 是"取或重取同一个 token"，`release` 是比较并删除，`settle` 是
/// "只有这个 token 才结算"，`resolve` 是**读且围栏**（一个仍被握着的键在报告的同时被翻成
/// `lost`）。差别只在**范围**：它活在本进程里 ⇒ 多副本部署下同一 installation 可能被两个副本
/// 同时连接，而生产部署契约因此是单副本（或"渠道连接只在一个副本上开"）。
pub struct InProcessDedupe {
    entries: Mutex<HashMap<String, Entry>>,
    capacity: usize,
    budget: Duration,
    now: Clock,
    /// 已结算/已丢弃的值活多久（上游那两条常量在 Redis 里也是带 TTL 的）。
    tombstone_ttl: Duration,
}

#[derive(Debug, Clone)]
struct Entry {
    value: String,
    expires_at: Instant,
}

impl fmt::Debug for InProcessDedupe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InProcessDedupe")
            .field("capacity", &self.capacity)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl Default for InProcessDedupe {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl InProcessDedupe {
    /// 建一个容量有界的替身（`capacity` 是键数上限；满了先淘汰一个过期项，再淘汰任意一项）。
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            capacity,
            budget: DEFAULT_CLAIM_BUDGET,
            now: Arc::new(Instant::now),
            tombstone_ttl: MIN_DEDUPE_TTL,
        }
    }

    /// 换掉时钟（用例把 TTL 走完而不睡真觉）。
    #[must_use]
    pub fn with_clock(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }

    /// 换掉"一次往返"的预算。
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 读一个键，顺手丢掉过期的。
    fn read(&self, key: &str) -> Option<Entry> {
        let now = (self.now)();
        let mut entries = self.lock();
        match entries.get(key) {
            Some(entry) if entry.expires_at <= now => {
                entries.remove(key);
                None
            }
            Some(entry) => Some(entry.clone()),
            None => None,
        }
    }

    fn write(&self, key: &str, value: String, ttl: Duration) {
        let now = (self.now)();
        let mut entries = self.lock();
        if entries.len() >= self.capacity && !entries.contains_key(key) {
            let victim = entries
                .iter()
                .find(|(_, entry)| entry.expires_at <= now)
                .map(|(key, _)| key.clone())
                .or_else(|| entries.keys().next().cloned());
            if let Some(victim) = victim {
                entries.remove(&victim);
            }
        }
        entries.insert(
            key.to_string(),
            Entry {
                value,
                expires_at: now + ttl,
            },
        );
    }

    /// 当前键数（诊断与用例读它；容量上限是这里**唯一**能被看见的"跨副本不成立"那一半）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 空吗。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl DedupeStore for InProcessDedupe {
    async fn claim(&self, key: &str, token: &str, ttl: Duration) -> Result<bool, String> {
        match self.read(key) {
            None => {
                self.write(key, token.to_string(), ttl);
                Ok(true)
            }
            Some(entry) if entry.value == token => {
                self.write(key, token.to_string(), ttl);
                Ok(true)
            }
            Some(_) => Ok(false),
        }
    }

    async fn release(&self, key: &str, token: &str) -> Result<bool, String> {
        match self.read(key) {
            Some(entry) if entry.value == token => {
                self.lock().remove(key);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn settle(&self, key: &str, token: &str) -> Result<bool, String> {
        match self.read(key) {
            Some(entry) if entry.value == token => {
                let ttl = self.tombstone_ttl;
                self.write(key, CLAIM_SETTLED_VALUE.to_string(), ttl);
                Ok(true)
            }
            Some(entry) if entry.value == CLAIM_SETTLED_VALUE => Ok(true),
            _ => Ok(false),
        }
    }

    async fn resolve(&self, key: &str) -> Result<ClaimState, String> {
        let ttl = self.tombstone_ttl;
        match self.read(key) {
            None => Ok(ClaimState::Absent),
            Some(entry) if entry.value == CLAIM_SETTLED_VALUE => Ok(ClaimState::Settled),
            Some(entry) if entry.value == CLAIM_LOST_VALUE => Ok(ClaimState::Lost),
            Some(_) => {
                // 读**且**围栏：同一个操作里翻成 lost。
                self.write(key, CLAIM_LOST_VALUE.to_string(), ttl);
                Ok(ClaimState::Held)
            }
        }
    }

    fn claim_budget(&self) -> Duration {
        self.budget
    }
}
