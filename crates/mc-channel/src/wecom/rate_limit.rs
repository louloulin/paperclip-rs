//! **一切让 `aibot_send_msg` 贴着 `WeCom` 公布配额的东西**：写之前的一道**每聊**闸，加上对两个
//! "现在不行"（而不是"永远不行"）的 `errcode` 的**一次**抖动退避
//! （上游 `internal/integrations/wecom/rate_limit.go`，**482 行**）。
//!
//! - **写者**：M7-20（`LUM-1785` / `docs/60-M7-PLAN.md` §3.3）。
//!
//! # 这一片交付什么、不交付什么（上游逐字）
//!
//! 一分钟内排在 30 条以下的会话**不受影响**。稍微超一点的那个被**推迟**进下一个空位而不是被拒
//! —— [`SendQuota::reserve`] 会在一个预算里等。**远远**爆掉的那个在写之前就在这里被拒：一次突发
//! 的第 31 帧不是被推迟，而是被一个**确定的错误**挡掉、wire 上什么都没有。让那种突发可投递**不是**
//! 这个文件能做到的事 —— 那需要一个地方把帧留到窗口移动，而本仓自出站队列退役之后**没有**那样的
//! 地方。
//!
//! 在我们自己这一侧拒绝**仍然是改进**，因为它替换掉的那样东西：没有这道闸，同一次突发会走到
//! `WeCom` 并以 `errcode 45009` 回来；`ws_sender` 把它翻成一个被拒绝的 API 错误，而出站路径上每一个
//! 调用方都把一条**说出来的**拒绝当成终局（`provably_not_sent` 说不再投递、`classify_drop` 记
//! `platform_refused`，回复就没了）。**一条被限流的帧就是一个人永远看不到的一个答案**，而他屏幕上
//! 没有任何东西说明这件事。我们自己拒绝在平台那一侧不花任何代价、不花那个聊的额度，而且**可证明
//! 没发出去**，于是中继可以把它投到别处。
//!
//! # 配额的两个数来自公布口径，**不是**在它们下面留的余量
//!
//! `WeCom` 对**一个会话**的公布配额：**30 条/分钟、1000 条/小时**。上游逐字引了那句中文：
//! *"无论是回复还是主动推送消息，总共给某个会话发消息的限制为 30 条/分钟，1000 条/小时"*
//! （`developer.work.weixin.qq.com/document/path/101463`）。超配额以 `errcode 45009`
//! （`.../path/90313`）回来。
//!
//! 两件事从那句话推出来，而这道闸**同时**建在它们之上：单位是**会话**而不是成员（所以下面的窗口
//! 按 chat id 分片 —— 群里整个房间共享一份额度），而且回复与推送**共用**同一份额度。
//!
//! # 本仓的形态差异（登记 `docs/32` §38 的 D1 / D5 / D6）
//!
//! 1. **桶按 installation 分片**（本片专属验收：「限流桶按安装分片」）。上游一个 `sendQuota`
//!    活在一个 `wsSender` 上 = 一把 socket = 一条安装，所以这道闸看见的帧**正是** `WeCom` 在数的
//!    那些。[`QuotaShards`] 把同一件事落成一张按 installation 分片的表。
//!    🔴 它同时**关掉**了上游自己指出的那条缝（*a reconnect mints a new wsSender with an empty
//!    window*）：本仓的桶跟着 installation 而不是跟着 socket 走 ⇒ 一次重连不再把计数清零。
//!    退避重试仍然留着 —— 它管的是 45033（并发限制）与"我们的计数与 `WeCom` 的不同"这一类。
//! 2. **拒绝的错误类型**：本仓的 [`SenderError`]（M7-16 的封闭枚举）**不得**新增变体 ⇒
//!    [`RateLimitRefusal`] 是本文件的类型，而在 `LiveSender` 那一层它落成
//!    `SenderError::NotAttempted`（"一个字节都没出去"的**唯一**记号）+ 一条带真实
//!    `next_slot` 的 WARN。
//! 3. **等空位不需要取消令牌**：上游 `reserve` 的 `ctx.Done()` 那一个分支在本仓**够不着** ——
//!    放弃时刻已经把等待界在 `deadline − write_budget` 之内（上游逐字"waiting past this point
//!    buys a turn on the wire the caller can no longer sit through"）。workspace 也没有
//!    `tokio-util`（`docs/60` §3.1 的依赖边一次冻死）。
//! 4. **预留按整条消息一次算清**：切分在 M7-16 的 `WsSender` **内部**（本片不得改它）⇒ 本片按
//!    `split_for_wire` 的**段数**一次预留。上游逐段预留 ⇒ 一条中途失败的答复在本仓会多花掉后面那几段
//!    的额度（**方向偏保守**：只会少发、不会多发）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::ws_frame::split_for_wire;
use super::ws_sender::{Deadline, SenderError, WsSender, ACK_TIMEOUT};

// =====================================================================
// 常量（上游逐字）
// =====================================================================

/// `WeCom` 对一个**会话**公布的每分钟上限（上游 `rateLimitPerMinute`）。
pub const RATE_LIMIT_PER_MINUTE: usize = 30;

/// `WeCom` 对一个**会话**公布的每小时上限（上游 `rateLimitPerHour`）。
pub const RATE_LIMIT_PER_HOUR: usize = 1000;

/// 这道闸让一个调用方等空位的最长时间（上游 `rateWaitBudget`）。
///
/// 上游逐字：这个界不是全部答案 —— [`SendQuota::reserve`] 也不许碰那次写自己的预算，所以真正发生的
/// 等待是 `min(rateWaitBudget, deadline − ackTimeout)`。
///
/// **它必须这样**，因为调用方**不共享**一个截止时刻：一条回复给整次投递十秒、一次收件箱推送五秒、
/// 一个附件五分钟。十秒里拿三秒走，写还剩七秒；五秒里拿三秒走，写只剩两秒，而光是一个 ack 就允许
/// 五秒。那次写会在半路被切断，`request` 交回一个上下文错误 —— "我们可能写了、但说不清"，那正是
/// `unconfirmedReason` 必须记成**未知**、而运维只能靠重发（人可能已经收到过）去解决的一格。
/// ⇒ 所以在那条五秒的路径上，这道闸**根本不等**：它直接拒，而拒绝是诚实的失败，因为什么都没写、
/// 调用方还握着它整份预算去记录这件事。
pub const RATE_WAIT_BUDGET: Duration = Duration::from_secs(3);

/// 一个被限流的帧那**一次**重试之前的基础延迟（上游 `sendRetryBackoff`）。
///
/// 对一次**可能是短暂的**拒绝做一次便宜的尝试。它**不是**在等一个空位：这道闸是一个滑动计数，下一个
/// 空位在那个窗口里最老的一项老化掉时才空出来，而一次突发里那几乎是整整一个窗口 —— `admit` 会乐意
/// 报 `59.999s`。等空位是 [`SendQuota::reserve`] 的事；从"一分钟 30 条就是每两秒一条"推一个固定
/// 延迟是在一个**故意不是**速率的限制上读平均值。
///
/// 两秒是照 **45033**（并发拒绝）定的，它的公布补救就是这一点也不多：*"企业微信出于系统保护的
/// 考虑，会对同一个企业调用同一个接口做并发数的限制，出现这种限制错误后，请企业调低并发数"*
/// （`.../path/90313`）。那一个不需要任何窗口翻过去 —— 同时调用的人少一点就是全部修法。
///
/// 对 **45009** 它是个长球，而代码把它当长球而不是假装相反。同一个页面说频率拦截的时长与挣到它的
/// 时段一样长：*"频率拦截时长一般与调用的限制时长相同，比如说是分钟级别的限制，则在中频率后的1分钟后
/// 自动解除"*。两秒清不掉一分钟的拦截。让那一次尝试**值得**的是这道闸看不见的那条缝 —— 一次重连
/// 会铸一个**新**的窗口（上游逐字；本仓由 [`QuotaShards`] 关掉了它，见模块文档差异 1）——
/// 所以一个 45009 可能是在对一个**本进程没在数**的计数发出的。[`retry_unaffordable`] 正是拦住它
/// 变成猛敲的东西：当我们**自己的**窗口说下一个空位还在很远，我们的计数与 `WeCom` 的一致、
/// 拦截是真的，那一帧不会被第二次发出去。
pub const SEND_RETRY_BACKOFF: Duration = Duration::from_secs(2);

/// `api freq out of limit`（上游 `errCodeAPIFreqLimit`）。
pub const ERR_CODE_API_FREQ_LIMIT: i32 = 45009;

/// `api concurrency out of limit`（上游 `errCodeAPIConcurrencyLimit`）。
pub const ERR_CODE_API_CONCURRENCY_LIMIT: i32 = 45033;

/// 分片表能装多少条 installation 的桶（本仓新增：进程内的界）。
pub const DEFAULT_QUOTA_SHARDS: usize = 1024;

// =====================================================================
// 拒绝
// =====================================================================

/// 这道闸拒绝了写：这个聊的配额花完了，而一个空位不会在**调用方的**预算里空出来。**什么都没到
/// wire**（上游 `errRateLimited`）。
///
/// 它**故意**不是本仓的 `SenderError` 的一个变体：那个枚举是 M7-16 定的**封闭**集（`docs/32`
/// §33 的 D5 与 §38 的 D5），而一个片顺手往里加一格会让别的片的分类表漂。
/// `LiveSender` 那一层把它落成 `SenderError::NotAttempted`（"一个字节都没出去"的记号）。
///
/// ⚠️ 上游逐字：它**故意**不是一个被包起来的上下文错误，**哪怕**结束这次等待的就是调用方的截止时刻。
/// 这条路径上的一个上下文错误意味着"我们可能写了、说不清"（`unconfirmedReason`），而那与这里发生的
/// 事**正好相反**：这道闸在 socket 的**上游**，所以这是**可证明什么都没发**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RateLimitRefusal {
    /// 下一个空位还在 `next_slot` 之后 —— 等它会把调用方的预算花光。
    #[error("wecom: this chat's outbound quota is spent; the next slot is {next_slot:?} away")]
    NoSlot { next_slot: Duration },
    /// 调用方在等一个空位时自己放弃了。
    #[error("wecom: the caller gave up while waiting for a slot")]
    CallerGaveUp,
}

impl RateLimitRefusal {
    /// 距离下一个空位还有多久（`CallerGaveUp` 报零：那一刻没有算出一个数）。
    #[must_use]
    pub fn next_slot(self) -> Duration {
        match self {
            Self::NoSlot { next_slot } => next_slot,
            Self::CallerGaveUp => Duration::ZERO,
        }
    }
}

// =====================================================================
// 窗口与桶（上游 `quotaWindow` / `sendQuota`）—— 拆到 `rate_limit/quota.rs`
// =====================================================================

mod quota;

pub use quota::{QuotaShards, QuotaWindow, SendQuota};

// =====================================================================
// 一次退避重试
// =====================================================================

/// 一次退避重试的延迟策略（上游 `wsSender.retryBackoff` + `retryDelay`）。
///
/// 上游逐字：抖动是因为一次**并发**拒绝会同时打到每一个并发调用方，而一个固定延迟会把它们
/// **在同一瞬间**一起送回去 —— 那正是"调低并发数"的反面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPlan {
    backoff: Duration,
}

impl Default for RetryPlan {
    fn default() -> Self {
        Self::new(SEND_RETRY_BACKOFF)
    }
}

impl RetryPlan {
    /// 用一个基础延迟建（生产用 [`SEND_RETRY_BACKOFF`]）。
    #[must_use]
    pub fn new(backoff: Duration) -> Self {
        Self { backoff }
    }

    /// 这一次重试该等多久：基础延迟在 ±50% 上摊开（上游 `retryDelay`）。
    ///
    /// 基础延迟为 0（或没给）时报 0 —— 它**不是**"抖一个零"的问题：一个 0 的退避就是立刻重试。
    #[must_use]
    pub fn delay(&self) -> Duration {
        retry_delay(self.backoff, next_entropy())
    }

    /// 基础延迟。
    #[must_use]
    pub fn backoff(&self) -> Duration {
        self.backoff
    }
}

/// 抖动的**纯函数**那一半：`base/2 + entropy % (base/2)`（上游 `base/2 + mathrand.Int64N(base)`，
/// 后者的半开区间就是 `[base/2, base)`）。
///
/// 拆出来是为了让用例能**逐字**钉住上下界，而不必相信一个随机数。
#[must_use]
pub fn retry_delay(base: Duration, entropy: u64) -> Duration {
    let half = base / 2;
    if half.is_zero() {
        return base;
    }
    let half_nanos = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
    let offset = entropy % half_nanos;
    half + Duration::from_nanos(offset)
}

/// 抖动的熵（xorshift64\*：没有依赖、够用、且**从不**回到 0 —— 全零是一个固定的不动点）。
static ENTROPY: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn next_entropy() -> u64 {
    let mut state = ENTROPY.load(Ordering::Relaxed);
    if state == 0 {
        state = 0x9E37_79B9_7F4A_7C15;
    }
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    ENTROPY.store(state.wrapping_mul(0x2545_F491_4F6C_DD1D), Ordering::Relaxed);
    state
}

/// 这一次失败是不是一个"平台自己会解除"的限流（上游 `throttled`）。
#[must_use]
pub fn throttled(error: &SenderError) -> bool {
    matches!(
        error,
        SenderError::Api { code, .. }
            if *code == ERR_CODE_API_FREQ_LIMIT || *code == ERR_CODE_API_CONCURRENCY_LIMIT
    )
}

/// 那**一次**重试为什么不该做，或者 `""`（`None`）当它该做（上游 `retryUnaffordable`）。
///
/// 两条理由都从**本进程知道的东西**回答，而不是从希望：
///
/// 1. 调用方得坐得住。`request` 把调用方的上下文花在写上**也**花在等判决上，所以一次尝试需要
///    `delay + ackTimeout` 的剩余时间；起步时比这更少，它回来就是一个上下文错误 —— 一个**未知**
///    的结局站在那里，而那里本来是一个确定的拒绝。回复路径给整次投递十秒，一次**来得晚**的限流
///    可以留给它不到这个数。
/// 2. 这道闸得在延迟到点的时候能派出一个空位 —— [`SendQuota::next_slot`] 已经知道下一个什么时候
///    空出来，问它。一个在一分钟之后的空位意味着我们自己的计数也到顶了，即这次拒绝是**配额的**
///    而不是一个陈旧计数的，于是 `WeCom` 在那个时段剩下的时间里都握着频率拦截（上游引的
///    `doc 90313`）。再发一次花掉一帧、什么都改不了。
#[must_use]
pub fn retry_unaffordable(
    quota: &SendQuota,
    deadline: Deadline,
    chat_id: &str,
    delay: Duration,
) -> Option<String> {
    let now = Instant::now();
    if deadline.is_some_and(|deadline| {
        deadline
            .checked_duration_since(now)
            .is_none_or(|left| left < delay + ACK_TIMEOUT)
    }) {
        return Some("the caller has no budget for another attempt".to_string());
    }
    let wait = quota.next_slot(chat_id, now + delay);
    if wait > Duration::ZERO {
        return Some(format!(
            "this chat's own quota has no slot for it either, the next one is {wait:?} off"
        ));
    }
    None
}

// =====================================================================
// 那一扇门
// =====================================================================

/// 一条 `aibot_send_msg` 的**写入面**：配额门下游的那一层。
///
/// 本仓的 `request` 是 M7-16 的 [`WsSender::send_text`]（切分 + 每聊一把锁 + 等判决全在它里面）。
/// 收窄成一个 trait 是为了让"门 + 一次重试"的那条流程**可测**而不必造一把真 socket ——
/// 与 `stream_store` 的 `StreamSender`（`docs/32` §33 的 D6）同款。
#[async_trait]
pub trait SendMsgWriter: Send + Sync {
    /// 写**恰好一条逻辑消息**。
    ///
    /// # Errors
    ///
    /// 发送侧的任意失败（[`SenderError`]，含"确定没发出"与"结局未知"的分野）。
    async fn write_send_msg(
        &self,
        chat_id: &str,
        chat_type: i32,
        content: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError>;
}

/// [`WsSender::send_text`] 就是那个写入面（M7-16 的产物；本片只给它加 trait 实现，不改它一行）。
#[async_trait]
impl SendMsgWriter for WsSender {
    async fn write_send_msg(
        &self,
        chat_id: &str,
        chat_type: i32,
        content: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        self.send_text(chat_id, chat_type, content, deadline).await
    }
}

/// 在这一聊的配额下写一条 `aibot_send_msg`（上游 `sendMsgFrame`）。
///
/// 那个 `cmd` 的**唯一**一扇门：它的两个生产者 —— agent 回答的一段（`ws_sender.rs`）与一次媒体
/// 推送（`media_upload.rs`）—— 花的是**同一份**每会话额度，所以只对其中一个设闸等于两个都没设。
///
/// 它被写成**第一次尝试 / 判决 / 第二次尝试**，而不是一个循环，因为两次报告失败的方式**不一样**：
/// 第一次的拒绝是一个**事实**（服务端说了一个 `errcode`），而它在这个函数剩下的部分里一直握在手上。
/// 写成循环就不行：第二趟的返回值直接成了函数的返回值，于是一次在 `request` 里被抬起的上下文错误会
/// 用一个**未知**的结局替换掉一个确定的拒绝 —— 那正是这条路径上唯一会把人送去**手动重发**的东西。
///
/// 那个事实有一个界，而结尾那个 `match` 就建在它上面：拒绝是**第一帧**的结局。它只在第二次尝试
/// **什么都没新写到 wire 上**时才能替第二次作答；第二帧一旦写出去，拒绝就对它落在哪儿什么都说不了，
/// 而那次尝试带回来的未知是唯一诚实的报告。
///
/// # Errors
///
/// [`SenderError`]：第一次的拒绝（当第二次什么都没写出去时）、第二次的结局（当它是**未知**的时），
/// 或者第二次自己那一类失败。
pub async fn send_msg_frame(
    writer: &dyn SendMsgWriter,
    quota: &SendQuota,
    plan: &RetryPlan,
    chat_id: &str,
    chat_type: i32,
    content: &str,
    deadline: Deadline,
) -> Result<(), SenderError> {
    // 预留**整条**消息的段数（模块文档差异 4）：上游逐段预留，而切分在 M7-16 的写侧内部。
    for _ in 0..split_for_wire(content).len().max(1) {
        if let Err(refusal) = quota.reserve(deadline, chat_id).await {
            // 两行都**不带** chat id：单聊里那个 chat id **就是**那个人的 userid，而本包别的地方
            // 一个都不把它放进日志；运维需要知道的是这个 bot 到了它的顶，不是谁在跟它说话。
            tracing::warn!(
                per_minute = RATE_LIMIT_PER_MINUTE,
                per_hour = RATE_LIMIT_PER_HOUR,
                next_slot = ?refusal.next_slot(),
                "wecom: a chat is at its outbound quota, frame not written"
            );
            return Err(SenderError::NotAttempted);
        }
    }

    let refusal = match writer
        .write_send_msg(chat_id, chat_type, content, deadline)
        .await
    {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    if !throttled(&refusal) {
        return Err(refusal);
    }

    let delay = plan.delay();
    if let Some(reason) = retry_unaffordable(quota, deadline, chat_id, delay) {
        tracing::warn!(
            %reason,
            ?delay,
            error = %refusal,
            "wecom: push throttled, retry skipped"
        );
        return Err(refusal);
    }
    tracing::warn!(?delay, error = %refusal, "wecom: push throttled, retrying once");

    // 睡那一次退避，但**不许**睡过调用方的截止时刻：那一刻我们知道的是服务端**拒了**这一帧、
    // 而且什么都没落地（那正是 `throttled` 的含义）。交回一个上下文错误会把一个确定的结局降级成
    // 未知，而未知是唯一需要运维手动解决的那种。
    if let Some(deadline) = deadline {
        if tokio::time::timeout_at(deadline.into(), tokio::time::sleep(delay))
            .await
            .is_err()
        {
            return Err(refusal);
        }
    } else {
        tokio::time::sleep(delay).await;
    }

    // 第二次尝试前**再**过一道闸：第一次尝试之后窗口可能已经满了。
    for _ in 0..split_for_wire(content).len().max(1) {
        if quota.reserve(deadline, chat_id).await.is_err() {
            return Err(refusal);
        }
    }
    match writer
        .write_send_msg(chat_id, chat_type, content, deadline)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if second_attempt_outcome_is_unknown(&error) => {
            // 第二次尝试自己的结局**真的**未知，而未知在这里压过第一次的拒绝：第二帧可能此刻
            // 就在那个人眼前，说"被拒了"会否认一次**发生过**的投递。这是第一次的拒绝**不是**
            // 更好答案的唯一一个方向。
            Err(error)
        }
        Err(error) if nothing_new_reached_the_wire(&error) => {
            // 这一次什么都没写出去 —— 这道闸在 socket 之前把帧挡了回来（或者 `request` 的写前检查
            // 做了同一件事），所以第一次的拒绝仍然是全部故事，而它是其中更好的那一半。
            Err(refusal)
        }
        Err(error) => Err(error),
    }
}

/// 第二次尝试的失败意味着**结局未知**吗（上游结尾 `switch` 的第一臂）。
///
/// 上游逐字：`errAckAbandoned` 是最像下面那一臂、却属于这里的那一格。它**是**一个上下文错误
/// （`errors.Is` 对 `context.Canceled` 答是），但 `request` 只在 `s.write` 无错返回之后才抬起它，
/// 所以第二帧**已经在 wire 上**，而第一次尝试的 errcode 对它落在哪儿什么都定不了 ⇒ 它必须排在
/// "什么都没写出去"那一臂**前面**。
fn second_attempt_outcome_is_unknown(error: &SenderError) -> bool {
    matches!(
        error,
        SenderError::AckTimeout
            | SenderError::StreamAckTimeout
            | SenderError::AckAbandoned { .. }
            | SenderError::WriteAttempted { .. }
    )
}

/// 第二次尝试的失败**可证明**什么都没写出去吗（上游第二臂，见模块文档差异 2）。
///
/// 上游那一臂列的是 `errRateLimited` 加上**裸的**上下文错误（`context.Canceled` /
/// `context.DeadlineExceeded`）—— 而它自己的两个"预算先到"哨兵（`errNotAttempted` /
/// `errChatBusy`）落在 `default` 里。本仓没有"裸上下文错误"这个变体，而那两个哨兵的对应物
/// （[`SenderError::NotAttempted`] / [`SenderError::ChatBusy`]）的文档逐字说它们是"**一个字节都
/// 没出去**" ⇒ 把它们并进这一臂与上游那段注释的**意图**一致（*the first refusal is still the
/// whole story and it is the better half of it*），同时**不**顺手并进
/// [`SenderError::is_not_attempted`] 的全集（那会把 `FrameTooLarge` 这类**永久**缺陷也藏进一次
/// 拒绝里）。登记为 `docs/32` §38 的 D6。
fn nothing_new_reached_the_wire(error: &SenderError) -> bool {
    matches!(error, SenderError::NotAttempted | SenderError::ChatBusy)
}

#[cfg(test)]
mod tests;
