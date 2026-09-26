//! **一次被路由投递背后的至多一次认领** —— 上游 `internal/integrations/wecom/dedupe_redis.go`
//! （200 行）的进程内替身，也是 `R-M7-1`（`docs/60` §2.5）的**真实**落点。
//!
//! - **写者**：M7-20（`LUM-1785` / `docs/60-M7-PLAN.md` §3.3）。
//!
//! # 上游为什么用 Redis，以及本仓为什么换成进程内
//!
//! 上游逐字：这个 claim 活在中继自己用的那台 Redis 上，而"没有 Redis 就没有中继、也就没有跨副本
//! 路由、于是没有什么要去重"正是它**不是**一条新依赖的全部理由。**一个从来不需要 Redis 的部署
//! 不会因为这里被要求装一台。**
//!
//! 本仓没有 Redis（根 `Cargo.toml` 与 `crates/**/Cargo.toml` 零命中，`mc-config` 也没有 redis
//! 配置）⇒ 换**进程内**替身 + **单副本部署契约**（`docs/60` §2.5 的判据：上游那一处有"无 Redis
//! 时"的等价语义，所以这不是伪造行为，而是换部署形态）。登记为 `docs/32` §38 的 D2 与 R1。
//!
//! # 它与 `relay::InProcessDedupe`（M7-17）的关系：**两份实现，一条判据**
//!
//! M7-17 在自己的写集里落过一份 `relay/claims.rs::InProcessDedupe`（它那一面需要 `DedupeStore`
//! 这个接口才能把中继装起来，见 §34 的 D2）。本仓的写集纪律是"一格 = 一个文件 = 一个写者"
//! （`docs/60` §3.3）⇒ 本片**不得**回头改那个文件。所以本文件落的是上游那份文件**逐条**语义的
//! **生产**实现，而它与 M7-17 那份有两处**可指出的**差别：
//!
//! 1. 🔴 **上游 `resolve` 的 legacy 那一格**（`redisResolveSource` 的 `string.find(v, '/', 1, true)`）
//!    在这一份里**落了**：一个**不是 token 形状**的值是"这套方案之前留下的认领"（那时整条 claim
//!    就是一个 `SET NX` 的裸 `1`），它的持有者**已经就地记过自己的结局** ⇒ `resolve` 报
//!    [`ClaimState::Settled`] 并**把这个键原样留着**。围栏它会给一条已经计过数的回复再记一次结局。
//!    这一格在**纯进程内**部署里够不着（见 [`WeComClaimStore::raw_value`] 的文档），
//!    但上游把它写进了脚本、**判据就只有一个**，所以本片照抄并逐字钉住。
//! 2. **值域与容量**：本文件的容量上限、TTL 与 tombstone TTL 都是显式的构造参数（上游那两条
//!    "settled / lost" 常量在 Redis 里同样带 TTL）。
//!
//! # 值域（上游逐字，别自己发明）
//!
//! | 值 | 谁写的 | `resolve` 报 |
//! | --- | --- | --- |
//! | 一个 **token**（十六进制 + `/` + 持有者） | `claim` | [`ClaimState::Held`]，**并在同一次操作里围栏成 lost** |
//! | [`CLAIM_SETTLED_VALUE`] | `settle` | [`ClaimState::Settled`] |
//! | [`CLAIM_LOST_VALUE`] | `resolve` 的围栏 | [`ClaimState::Lost`] |
//! | 其它（legacy：裸 `1`） | 这套方案之前的 `SET NX` | [`ClaimState::Settled`]，**原样保留** |
//! | 什么都没有 / 过期了 | —— | [`ClaimState::Absent`] |
//!
//! **每一个 token 都带 `/`**（[`crate::wecom::relay::RelayOutbound`] 的 `token_for` 造的）⇒
//! 两种形态不需要版本位、也不需要一次键迁移就能分辨。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::relay::{
    ClaimState, Clock, DedupeStore, CLAIM_LOST_VALUE, CLAIM_SETTLED_VALUE, DEFAULT_CLAIM_BUDGET,
    MIN_DEDUPE_TTL,
};

// =====================================================================
// 值域常量
// =====================================================================

/// token 的分隔符（上游 `tokenFor` 里的 `"/"`，也是 `redisResolveSource` 用来分辨 legacy 值的
/// 那**一个**字符）。
pub const TOKEN_SEPARATOR: char = '/';

/// 这套方案**之前**那条认领留下的值：一个裸的 `1`（上游注释逐字：*the plain "1" of the SET NX
/// that used to be the whole claim*）。
///
/// 本文件**从不**写它 —— 它只用来给用例种一个 legacy 键，从而把上游 `resolve` 的那一格钉住。
pub const LEGACY_CLAIM_VALUE: &str = "1";

/// 一个认领值是不是 **token 形状**（上游 `string.find(v, '/', 1, true)`：普通查找，不是正则）。
///
/// 空串**不算**：上游那一支只在 `v` 非空之后才走得到，而空值在这里没有定义。
#[must_use]
pub fn is_token_shaped(value: &str) -> bool {
    value.contains(TOKEN_SEPARATOR)
}

// =====================================================================
// 键
// =====================================================================

/// 一个**轮次**的认领键：与流条目带的是**同一个事件 id**，于是"重启之后被重放的帧"与"租约搬迁
/// 期间被两个副本读到的帧"落在**同一个**键上（上游逐字）。
///
/// 直接复用中继那一份命名（[`crate::wecom::relay::dedupe_key`]），**不另立一套**：同一个事件 id
/// 在两个地方必须是同一个字符串，否则这道闸与中继的幂等会各认各的。
#[must_use]
pub fn claim_key(event_id: &str) -> String {
    super::relay::dedupe_key(event_id)
}

// =====================================================================
// 认领存储
// =====================================================================

/// 一条认领（上游 Redis 里的 `key → value`，外加它自己的过期时刻）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    value: String,
    expires_at: Instant,
}

/// **进程内**的至多一次认领存储（上游 `redisDedupe`）。
///
/// 语义逐条在模块文档的表里；`claim` / `release` / `settle` / `resolve` 四个操作与上游三张 Lua
/// 脚本**逐格**对应（"取或重取同一个 token" / "比较并删除" / "只有这个 token 才结算" /
/// "读且围栏"）。
///
/// # 单副本契约（R-M7-1）
///
/// 它活在**本进程**里 ⇒ 多副本部署下同一个 installation 可能被两个副本同时连接，而这道闸在第二个
/// 副本上什么都没拦住。生产部署契约因此是**单副本**，或者"渠道连接只在一个副本上开"。
/// 上游那台 Redis 是这条契约的**唯一**东西，本仓用一次部署约定换掉了它。
pub struct WeComClaimStore {
    entries: Mutex<HashMap<String, Entry>>,
    capacity: usize,
    budget: Duration,
    tombstone_ttl: Duration,
    now: Clock,
}

impl fmt::Debug for WeComClaimStore {
    /// 手写：只报**形状**，不报内容 —— 键是事件 id、值是持有者 token，两者都是路由身份。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WeComClaimStore")
            .field("capacity", &self.capacity)
            .field("budget", &self.budget)
            .field("tombstone_ttl", &self.tombstone_ttl)
            .finish_non_exhaustive()
    }
}

impl Default for WeComClaimStore {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl WeComClaimStore {
    /// 建一个容量有界的替身（容量是**键数**上限；满了先淘汰一个已过期项，再淘汰任意一项 ——
    /// 上游把这件工作交给 Redis 的 `maxmemory` 策略，本仓只能自己定一个界）。
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            capacity,
            budget: DEFAULT_CLAIM_BUDGET,
            tombstone_ttl: MIN_DEDUPE_TTL,
            now: Arc::new(Instant::now),
        }
    }

    /// 换掉时钟（用例把 TTL 走完而**不睡真觉**）。
    #[must_use]
    pub fn with_clock(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }

    /// 换掉"一次往返"报出的预算（上游 `claimBudget`）。
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// 换掉 settled / lost 两个值活多久。
    #[must_use]
    pub fn with_tombstone_ttl(mut self, ttl: Duration) -> Self {
        self.tombstone_ttl = ttl;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 读一个键，顺手丢掉已经过期的（Redis 的 `PX` 到期）。
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
        if !entries.contains_key(key) && entries.len() >= self.capacity {
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

    /// 一个键此刻的**原样值**（诊断与用例用）。
    ///
    /// 它存在是为了让上游 `resolve` 的 legacy 那一格可判：那条判据的**全部**主张是"这个键
    /// **原样留着**"，而"原样"只有看得见原始值才谈得上被钉住。
    ///
    /// ⚠️ 在**纯进程内**部署里这一格**够不着**：legacy 值只可能来自这台 Redis 上更早的写入，
    /// 而进程内存储从一个空表开始、且本文件从不写 [`LEGACY_CLAIM_VALUE`]。照抄的理由与
    /// M7-16 的 D9 同款：**上游写下的判据只有一个**，而给一条够不着的分支留一条备用通道，
    /// 正是下一个实现者把它删掉之后又"顺手改回去"的来处。
    #[must_use]
    pub fn raw_value(&self, key: &str) -> Option<String> {
        self.read(key).map(|entry| entry.value)
    }
}

/// 认领的四个操作（上游三张 Lua 脚本 + `bookkeepingBudget` 的那两处调用）。
///
/// # 与上游 `context` 的差异（登记 `docs/32` §38 的 D3）
///
/// 上游每一个操作都把 `ctx` 变成一次**有界的往返**（`context.WithTimeout(ctx, d.budget)`），
/// `Release` / `Settle` 还要区分"丢取消"与"丢截止时刻"（`bookkeepingBudget`）。本仓的替身**不
/// 上线**：一次 `HashMap` 操作没有可界的东西，所以 `budget` 只作为**报出的数字**存在 ——
/// `RelayOutbound::outcome_grace` 每次 offer 都付它一次，而"一份由调度器私藏的拷贝会自由地与
/// 真正生效的超时漂开"这条上游理由在进程内同样成立。**不假装**它界住了什么。
#[async_trait]
impl DedupeStore for WeComClaimStore {
    async fn claim(&self, key: &str, token: &str, ttl: Duration) -> Result<bool, String> {
        match self.read(key) {
            // 没人握着它 ⇒ `SET key token PX ttl`。
            None => {
                self.write(key, token.to_string(), ttl);
                Ok(true)
            }
            // `v == ARGV[1]` ⇒ 同一个持有者在一次结局未知的 Release 之后回来 ⇒ 再取一次是对的。
            Some(entry) if entry.value == token => {
                self.write(key, token.to_string(), ttl);
                Ok(true)
            }
            // 别人握着它（或它已经 settled / lost）⇒ 不抢、不改。
            Some(_) => Ok(false),
        }
    }

    async fn release(&self, key: &str, token: &str) -> Result<bool, String> {
        match self.read(key) {
            Some(entry) if entry.value == token => {
                self.lock().remove(key);
                Ok(true)
            }
            // `false` + 无错误 = 键不再握着这个 token（删除已落地 / 过期 / 别人握走了）——
            // **不是**失败。
            _ => Ok(false),
        }
    }

    async fn settle(&self, key: &str, token: &str) -> Result<bool, String> {
        match self.read(key) {
            Some(entry) if entry.value == token => {
                // `SET key "settled" KEEPTTL`。
                let ttl = self.tombstone_ttl;
                self.write(key, CLAIM_SETTLED_VALUE.to_string(), ttl);
                Ok(true)
            }
            // `v == ARGV[2]` ⇒ 结算一个已经结算的 claim 报 true（重试是安全的）。
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
            // 🔴 legacy：**不是 token 形状** ⇒ 它是这套方案之前那条认领，它的持有者已经就地记过
            // 自己的结局 ⇒ 报 settled 并**原样留着**。围栏它会给一条已经计过数的回复再记一次。
            Some(entry) if !is_token_shaped(&entry.value) => Ok(ClaimState::Settled),
            // 读**且**围栏：同一个操作里翻成 lost，于是一个更晚回来的持有者会发现它的 Settle 被拒。
            Some(_) => {
                self.write(key, CLAIM_LOST_VALUE.to_string(), ttl);
                Ok(ClaimState::Held)
            }
        }
    }

    fn claim_budget(&self) -> Duration {
        self.budget
    }
}

#[cfg(test)]
mod tests;
