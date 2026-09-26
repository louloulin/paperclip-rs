//! **窗口与桶**：`WeCom` 公布的那两个窗口、按聊计数的滑动闸，以及按 installation 分片的那张表
//! （上游 `internal/integrations/wecom/rate_limit.go` 里 `quotaWindow` / `sendQuota` /
//! `newSendQuotaWith` 那一段）。
//!
//! 本文件是 `rate_limit.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（父文件正文 818 行 ⇒ 超限），
//! 加上"一格 = 一个面"的记法纪律（`docs/60-M7-PLAN.md` §3.3）。逐条清单见 `docs/32` §38 的 D9。
//!
//! 上半（窗口的语义、为什么不是令牌桶、`admit` / `nextSlot` / `waitFor` / `sweep` 的逐条理由）在
//! 上游是同一段注释，这里原样留着 —— 判据与理由放在**同一个**文件里，别让后来的人只读到一个。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mc_core::id::Id;

use super::super::ws_sender::{Deadline, ACK_TIMEOUT};
use super::{
    RateLimitRefusal, DEFAULT_QUOTA_SHARDS, RATE_LIMIT_PER_HOUR, RATE_LIMIT_PER_MINUTE,
    RATE_WAIT_BUDGET,
};

/// `WeCom` 公布的窗口之一：任何一段 `span` 长的时间里至多 `limit` 次发送（上游 `quotaWindow`）。
///
/// `limit` 至少是 1 —— 一个什么都过不去的窗口是"这个 bot 被关掉了"的部署，那属于**这个文件之上**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaWindow {
    pub span: Duration,
    pub limit: usize,
}

impl QuotaWindow {
    /// 建一个窗口。
    #[must_use]
    pub fn new(span: Duration, limit: usize) -> Self {
        Self {
            span,
            limit: limit.max(1),
        }
    }
}

// =====================================================================
// 配额
// =====================================================================

/// 一个会话上已经发出的那些时刻（升序），加上上一次清扫的时刻。
#[derive(Default)]
struct QuotaState {
    sent: HashMap<String, Vec<Instant>>,
    last_sweep: Option<Instant>,
}

/// 按公布速率放行 `aibot_send_msg` 帧，**按目标聊**计数（上游 `sendQuota`）。
///
/// # 为什么一个进程里一张表就是全部记账（上游逐字）
///
/// `WeCom` 按 `(application, recipient)` 计数，而 aibot **没有** REST 出站路径 ⇒ 一条安装的每一帧
/// 都由握着那条安装 socket 租约的副本写出。这个结构活在那一把 socket 上 ⇒ 它看见的帧正是 `WeCom`
/// 在数的帧。
///
/// # 滑动计数而不是令牌桶（上游逐字）
///
/// 公布的数字是"一个窗口里的**次数**"：一个以 30/分钟回填的桶会在**一个滚动分钟内**放进多达 60 条，
/// 而那正是我们想避开的那条线。
pub struct SendQuota {
    state: Mutex<QuotaState>,
    windows: Vec<QuotaWindow>,
    longest: Duration,
    max_wait: Duration,
    /// 一次写不许从调用方的截止时刻里被等掉的那一部分。上游是 `ackTimeout`：那是判决定来
    /// 的等待上限（`ws_sender.rs`），所以后面那一半至少要它。
    write_budget: Duration,
}

impl std::fmt::Debug for SendQuota {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SendQuota")
            .field("windows", &self.windows)
            .field("max_wait", &self.max_wait)
            .field("write_budget", &self.write_budget)
            .field("tracked_chats", &self.tracked_chats())
            .finish_non_exhaustive()
    }
}

impl SendQuota {
    /// 生产形态：上游那两个窗口 + [`RATE_WAIT_BUDGET`]（上游 `newSendQuota`）。
    #[must_use]
    pub fn new() -> Self {
        Self::with_windows(
            RATE_WAIT_BUDGET,
            &[
                QuotaWindow::new(Duration::from_mins(1), RATE_LIMIT_PER_MINUTE),
                QuotaWindow::new(Duration::from_hours(1), RATE_LIMIT_PER_HOUR),
            ],
        )
    }

    /// 自带窗口的闸（上游 `newSendQuotaWith`）：用例用它把等待与拒绝两条路在**毫秒**里走完，
    /// 而不是分钟。
    #[must_use]
    pub fn with_windows(max_wait: Duration, windows: &[QuotaWindow]) -> Self {
        let longest = windows
            .iter()
            .map(|window| window.span)
            .max()
            .unwrap_or_default();
        Self {
            state: Mutex::new(QuotaState::default()),
            windows: windows.to_vec(),
            longest,
            max_wait,
            write_budget: ACK_TIMEOUT,
        }
    }

    /// 换掉"从调用方预算里扣住不给写的那一部分"（用例）。
    #[must_use]
    pub fn with_write_budget(mut self, write_budget: Duration) -> Self {
        self.write_budget = write_budget;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QuotaState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 等这个聊可以拿一个空位，或者**不拿**地失败（上游 `reserve`）。
    ///
    /// 一个空位**近到值得等**的时候它等 —— 一道闸的意义就是一次突发**迟到**而不是不来 ——
    /// 而不值得等的时候它立刻放弃，而不是花掉调用方整份预算、最后抵达同一个答案却没时间记录它。
    ///
    /// # Errors
    ///
    /// [`RateLimitRefusal`]：什么都没往 wire 上去。
    pub async fn reserve(&self, deadline: Deadline, chat_id: &str) -> Result<(), RateLimitRefusal> {
        let now = Instant::now();
        let mut give_up_at = now + self.max_wait;
        if let Some(deadline) = deadline {
            // `deadline − write_budget`，**不是** deadline。刚好塞进调用方截止时刻的一次等待
            // 不够好：它后面还有一次写加上等判决，而 `ackTimeout` 是那一件单独被允许花的时间。
            let latest = deadline.checked_sub(self.write_budget).unwrap_or(deadline);
            if latest < give_up_at {
                give_up_at = latest;
            }
        }
        loop {
            let now = Instant::now();
            let wait = self.admit(chat_id, now);
            if wait.is_zero() {
                // 空位空着 ⇒ 没花掉任何预算。**故意**在 `give_up_at` 之前判：
                // 一个只剩最后一刻的调用方，在这道闸不在路上时仍然拿到它的那一回合。
                return Ok(());
            }
            if now + wait > give_up_at {
                return Err(RateLimitRefusal::NoSlot { next_slot: wait });
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(RateLimitRefusal::CallerGaveUp);
            }
            tokio::time::sleep_until((now + wait).into()).await;
        }
    }

    /// 记下这个聊的一次发送并报 0，或者**不动**计数、报最早那个空位多久之后空出来
    /// （上游 `admit`）。
    pub fn admit(&self, chat_id: &str, now: Instant) -> Duration {
        let mut state = self.lock();
        Self::sweep(&mut state, now, self.longest);
        let sent = Self::trim_before(
            &state.sent.get(chat_id).cloned().unwrap_or_default(),
            now.checked_sub(self.longest).unwrap_or(now),
        )
        .to_vec();
        let wait = self.wait_for(&sent, now);
        if wait > Duration::ZERO {
            state.sent.insert(chat_id.to_string(), sent);
            return wait;
        }
        let mut sent = sent;
        sent.push(now);
        state.sent.insert(chat_id.to_string(), sent);
        Duration::ZERO
    }

    /// `at` 那一刻这个聊的下一个空位多久之后空出来，**不**取走它（上游 `nextSlot`）。
    ///
    /// [`SendQuota::admit`] 的只读孪生：用来判断"某个动作值不值得开始" —— 问 `admit` 会以花掉那个
    /// 正在被问的东西的方式回答问题。
    #[must_use]
    pub fn next_slot(&self, chat_id: &str, at: Instant) -> Duration {
        let state = self.lock();
        let entries = state.sent.get(chat_id).cloned().unwrap_or_default();
        let sent = Self::trim_before(&entries, at.checked_sub(self.longest).unwrap_or(at));
        self.wait_for(sent, at)
    }

    /// `sent` 还要多久才在每个窗口里都有位置；现在就有位置时报 0（上游 `waitFor`）。
    ///
    /// 读窗口的**唯一**地方，所以 [`SendQuota::admit`] 与 [`SendQuota::next_slot`] 不可能漂成
    /// 对同一个问题给出两个答案。
    #[must_use]
    pub fn wait_for(&self, sent: &[Instant], now: Instant) -> Duration {
        let mut wait = Duration::ZERO;
        for window in &self.windows {
            let first = index_at_or_after(sent, now.checked_sub(window.span).unwrap_or(now));
            let in_window = sent.len().saturating_sub(first);
            if in_window < window.limit {
                continue;
            }
            // 这个窗口在它最老的若干项老化出去之前没有位置 —— 对一个恰好坐在上限上的窗口是一项。
            let free = sent[first + in_window - window.limit]
                .checked_add(window.span)
                .and_then(|at| at.checked_duration_since(now))
                .unwrap_or_default();
            if free > wait {
                wait = free;
            }
        }
        wait
    }

    /// 丢掉在最长的那个窗口里什么都不剩的聊（上游 `sweep`）。调用方持有锁。
    ///
    /// 没有它，一个跟很多聊说过话的进程会为每一个聊永远留着一个时刻切片。
    fn sweep(state: &mut QuotaState, now: Instant, longest: Duration) {
        if state
            .last_sweep
            .is_some_and(|last| now.saturating_duration_since(last) < longest)
        {
            return;
        }
        state.last_sweep = Some(now);
        let cutoff = now.checked_sub(longest).unwrap_or(now);
        state
            .sent
            .retain(|_, sent| sent.last().is_some_and(|last| *last > cutoff));
    }

    /// 从一个升序切片里丢掉比 `cutoff` 老的项（上游 `trimBefore`）。
    fn trim_before(sent: &[Instant], cutoff: Instant) -> &[Instant] {
        &sent[index_at_or_after(sent, cutoff)..]
    }

    /// 这个聊在册的时刻数（诊断与用例用）。
    #[must_use]
    pub fn sent_count(&self, chat_id: &str) -> usize {
        self.lock().sent.get(chat_id).map_or(0, std::vec::Vec::len)
    }

    /// 在册的聊数（诊断与用例用；清扫的可见那一半）。
    #[must_use]
    pub fn tracked_chats(&self) -> usize {
        self.lock().sent.len()
    }

    /// 在册的窗口。
    #[must_use]
    pub fn windows(&self) -> &[QuotaWindow] {
        &self.windows
    }

    /// 放弃等待的那条线（`min(max_wait, deadline − write_budget)` 的前一半）。
    #[must_use]
    pub fn max_wait(&self) -> Duration {
        self.max_wait
    }
}

impl Default for SendQuota {
    fn default() -> Self {
        Self::new()
    }
}

/// `cutoff` 落在一个升序的发送时刻切片里的位置（上游 `indexAtOrAfter`）。
pub fn index_at_or_after(sent: &[Instant], cutoff: Instant) -> usize {
    sent.partition_point(|at| *at <= cutoff)
}

// =====================================================================
// 按 installation 分片
// =====================================================================

/// 一张按 installation 分片的配额表（本片专属验收：「限流桶按安装分片」）。
///
/// 上游一个 `sendQuota` 活在一个 `wsSender` 上 = 一把 socket = 一条安装 ⇒ 表在这里，桶在那里。
/// 一个 installation 的桶**跟着 installation 走**，不跟着 socket 走：一次重连不再把计数清零
/// （上游自己指出那条缝，见模块文档差异 1）。
///
/// 表本身有界（[`DEFAULT_QUOTA_SHARDS`]）：装满时淘汰一条**最久没用过**的（一张只增不减的表是
/// 一条泄漏 —— `task:failed` 那一类事件对部署里每一个 run 都会发）。
pub struct QuotaShards {
    shards: Mutex<HashMap<Id, Arc<SendQuota>>>,
    capacity: usize,
    /// 每一条**新**桶的窗口（[`QuotaShards::new`] 给的是上游公布的那两个）。
    windows: Vec<QuotaWindow>,
    max_wait: Duration,
}

impl std::fmt::Debug for QuotaShards {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QuotaShards")
            .field("capacity", &self.capacity)
            .field("shards", &self.len())
            .finish_non_exhaustive()
    }
}

impl Default for QuotaShards {
    fn default() -> Self {
        Self::new(DEFAULT_QUOTA_SHARDS)
    }
}

impl QuotaShards {
    /// 建一张有界的表：每一个桶都用**生产**窗口（上游公布的那两个）。
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self::with_config(
            capacity,
            RATE_WAIT_BUDGET,
            &[
                QuotaWindow::new(Duration::from_mins(1), RATE_LIMIT_PER_MINUTE),
                QuotaWindow::new(Duration::from_hours(1), RATE_LIMIT_PER_HOUR),
            ],
        )
    }

    /// 建一张有界的表：每一个桶都用**给定**窗口。
    ///
    /// 用例用它把窗口缩到毫秒（上游 `newSendQuotaWith` 的同一手法），而它也是"WeCom 哪天改了公布
    /// 口径"时唯一要改的那个旋钮 —— 表与桶的配置只有**一个**来源。
    #[must_use]
    pub fn with_config(capacity: usize, max_wait: Duration, windows: &[QuotaWindow]) -> Self {
        Self {
            shards: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            windows: windows.to_vec(),
            max_wait,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, Arc<SendQuota>>> {
        match self.shards.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 这条安装的那一个桶（第一次问它时铸一个新的）。
    ///
    /// 铸的是**同一份配置**（[`SendQuota::new`]）⇒ 每一条安装的窗口与放弃线逐字相同。
    #[must_use]
    pub fn bucket(&self, installation_id: Id) -> Arc<SendQuota> {
        let mut shards = self.lock();
        if let Some(existing) = shards.get(&installation_id) {
            return Arc::clone(existing);
        }
        if shards.len() >= self.capacity {
            if let Some(victim) = shards.keys().next().copied() {
                shards.remove(&victim);
            }
        }
        let quota = Arc::new(SendQuota::with_windows(self.max_wait, &self.windows));
        shards.insert(installation_id, Arc::clone(&quota));
        quota
    }

    /// 这条安装**当前**在不在册（诊断与用例用；它**不**铸桶）。
    #[must_use]
    pub fn get(&self, installation_id: Id) -> Option<Arc<SendQuota>> {
        self.lock().get(&installation_id).map(Arc::clone)
    }

    /// 在册的桶数。
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
