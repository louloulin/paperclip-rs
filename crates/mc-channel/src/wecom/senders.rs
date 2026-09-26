//! 一张进程级的 `installation_id → 活着的 wsSender` 表（上游
//! `internal/integrations/wecom/senders_registry.go`，**204 行**）。
//!
//! - **写者**：M7-20（`LUM-1785` / `docs/60-M7-PLAN.md` §3.3）。
//!
//! # 上游的定位（逐字）
//!
//! `wecomChannel.Connect` 进来时装一条、出去时清一条；`OutboundReplier` 与 `Outbound` 按
//! installation id 查它，好把 `aibot_send_msg` 推上**入站回路自己拥有的那把 socket**（aibot 没有
//! REST 出站路径；每一次写都走 WebSocket）。`wecomChannel.Send` **不是**读者 —— 它返回
//! `ErrSendNotSupported`。
//!
//! **为什么是注册表而不是把发送者存在 `wecomChannel` 上**（上游逐字）：`OutboundReplier` 在启动时
//! 用共享的 `engine.Router` 铸一次，它**没有**每条 installation 的 Channel 句柄。引擎调
//! `Replier.Reply` 时交过来的是带着 installation id 的 `ResolvedInstallation`，不是 Channel。
//! 注册表就是让那个启动期铸成的 Replier 够到每条 installation 活连接的**接缝**，而不必把 Channel
//! 穿过整个 engine。
//!
//! # 本文件的三个面（本片是它们的**唯一**落点）
//!
//! 上游那一个结构在一个文件里同时是写侧、读侧与流面。本仓的接缝把它们拆成三个已经存在的 trait，
//! 而本文件是它们共同的实现：
//!
//! | 面 | trait | 谁定契约 | 上游出处 |
//! | --- | --- | --- | --- |
//! | **写侧** | [`SenderRegistry`] | M7-19（`wecom_channel.rs`） | `set` / `clear` |
//! | **读侧** | [`SenderLookup`] | M7-17（`outbound/senders.rs`） | `get` / `stream_sender` |
//! | **流面** | [`StreamSender`] | M7-16（`stream_store/ports.rs`） | `stream` / `streamRewrite` / `recordEnding` |
//!
//! 上游 `senders_registry.go` 的其余方法各归各处：`sendTextCtx` 是 [`LiveSenders::send_text`]，而
//! 配额门（上游 `ws_sender.go` 的 `sendMsgFrame` 里那段）在 [`super::rate_limit`]。
//!
//! # 一处**不显然**的净清语义（上游逐字的缺陷报告，别"简化"掉）
//!
//! `clear` **只在这条安装当前登记的仍是它自己那把发送者时**才删。一个正在收尾的**代**不许把它的
//! **继任者**挤掉：`Connect` 进入时装上、在 `defer` 里清除，所以一次租约翻转会与还在排空的旧 socket
//! **重叠**，输的那一代的 `defer` 在赢的那一代的 `set` **之后**才跑。那里无条件删除会让注册表在一条
//! **健康连接**在运行时空着，于是每一次出站推送都解不出东西 —— bot 安静下来，而日志里没有任何东西
//! 说明为什么，直到下一次重连碰巧又重新登记。
//!
//! 上游逐字：`dingtalk_channel.go:74` 用 `CompareAndSwap(c, nil)` 守同一次交接；slack 与 lark
//! **根本没有**注册表（它们的出站是 REST）。`WeCom` 是那个无条件删除的平台。
//!
//! # 本仓的形态差异（登记 `docs/32` §38 的 D5 / D7）
//!
//! 1. **`errNoLiveConnection` → `SenderError::NotAttempted`**：那个哨兵的意思是"本副本这条安装没有
//!    活连接"（**不是**错误，是"连接还没准备好"）。本仓的 `SenderError`（M7-16 的封闭枚举）不得新增
//!    变体 ⇒ 落成 `NotAttempted`（"一个字节都没出去"的记号），日志里带上真实原因。
//! 2. **配额门在**这一层**、而不是 `send_one_text` 里**：M7-16 的 D4 把那段留给本片，但
//!    `ws_sender.rs` 不在本片写集里 ⇒ 门落在 [`LiveSenders::send_text`]（`LiveSender` 那一层），
//!    而上游那道门在 `send_one_text` 里面。**代价**：预留的粒度是**一条逻辑消息**而不是**一段**
//!    （见 [`super::rate_limit::send_msg_frame`] 的模块文档差异 4）。
//! 3. **收尾帧没有截止时刻**：上游把调用方的 `ctx` 带进 `stream`/`streamRewrite`，而本仓的
//!    [`StreamSender`] 端口（M7-16 定的）没有那个形参 ⇒ 传 `None`（"该等多久就等多久"）。这是放宽，
//!    而且是对的那一侧：一条被自己预算切断的收尾帧会变成**未知**结局，而 `stream_store::seal` 自己
//!    的重试策略（最多三次、每次隔 2s）已经把总时长界住了。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use mc_core::id::Id;

use super::metrics::{or_nop_metrics, Metrics};
use super::outbound::{LiveSender, SenderLookup};
use super::rate_limit::{send_msg_frame, QuotaShards, RetryPlan, SendQuota};
use super::stream_store::{StreamHandle, StreamSender};
use super::wecom_channel::SenderRegistry;
use super::ws_sender::{Deadline, SenderError, WsSender};

/// 本副本这条安装没有活连接（上游 `errNoLiveConnection`）。
///
/// 上游逐字：调用方**必须**把 `nil` 当成"连接还没准备好"—— `Supervisor` 可能正在一次租约翻转
/// 之后重连。本仓没有 `nil` 可用（[`SenderLookup::get`] 返回 `Option`），所以这一条只留在"拿到了
/// 一把发送者、但它在写之前就被撤下了"的路径上。
///
/// 落在 [`SenderError::NotAttempted`]：它逐字就是"本进程一个字节都没往 wire 上写"。
#[must_use]
pub fn no_live_connection() -> SenderError {
    SenderError::NotAttempted
}

// =====================================================================
// 一条安装的发送者
// =====================================================================

/// 一条安装的活连接**外加它的配额桶**：`LiveSender` 的实现体。
///
/// 上游 `sendersRegistry.sendTextCtx` 先查表、再调 `sender.sendTextCtx`，而配额在那把发送者**自己**
/// 的 `sendMsgFrame` 里。本仓把那两件事放在**一个**值上，于是"写一条消息"这条路上只有一个门，
/// 而且它拿到的桶**跟着 installation 走**（[`QuotaShards`]，见本片专属验收）。
pub struct InstallationSender {
    sender: Arc<WsSender>,
    quota: Arc<SendQuota>,
    plan: RetryPlan,
}

impl fmt::Debug for InstallationSender {
    /// 手写：**不报**任何 socket 或凭据形态的东西 —— 只报"有一条连接"与它的配额桶在不在。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallationSender")
            .field("sender", &"<live socket>")
            .field("quota", &"<per-installation>")
            .field("retry_backoff", &self.plan.backoff())
            .finish()
    }
}

impl InstallationSender {
    /// 装配一条安装的发送面。
    #[must_use]
    pub fn new(sender: Arc<WsSender>, quota: Arc<SendQuota>, plan: RetryPlan) -> Self {
        Self {
            sender,
            quota,
            plan,
        }
    }

    /// 这台写侧本身（流帧面与诊断用）。
    #[must_use]
    pub fn sender(&self) -> &Arc<WsSender> {
        &self.sender
    }

    /// 这条安装的配额桶。
    #[must_use]
    pub fn quota(&self) -> &Arc<SendQuota> {
        &self.quota
    }
}

#[async_trait]
impl LiveSender for InstallationSender {
    /// 往 `chat_id` 推一条文本 —— **先过这一聊的配额门**（上游 `sendMsgFrame` 的那两半）。
    async fn send_text(
        &self,
        chat_id: &str,
        chat_type: i32,
        text: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        send_msg_frame(
            self.sender.as_ref(),
            &self.quota,
            &self.plan,
            chat_id,
            chat_type,
            text,
            deadline,
        )
        .await
    }
}

// =====================================================================
// 注册表
// =====================================================================

/// 一张 goroutine-safe 的 `installation_id → 活着的 wsSender` 表（上游 `sendersRegistry`）。
///
/// `RwLock` 而不是 `Mutex`：读（每一次出站推送、每一帧收尾）比写（一次连接起来 / 结束）多几个数量级，
/// 而**一条**读不许排在另一条读后面。
pub struct LiveSenders {
    by_key: RwLock<HashMap<Id, Arc<WsSender>>>,
    /// 每一条写经这里记账的汇（上游 `WithMetrics`；没配时是 no-op）。存 `Option` 是因为
    /// "配没配"是**部署事实**，得能被 `Debug` 报出来。
    metrics: RwLock<Option<&'static dyn Metrics>>,
    /// 按 installation 分片的配额表（上游一个 `sendQuota` 跟着一把 socket 走 ⇒ 本仓跟着 installation）。
    quotas: QuotaShards,
    retry: RetryPlan,
}

impl fmt::Debug for LiveSenders {
    /// 手写：只报**形状**（几条连接、汇配没配、桶几张），不报任何 id —— 它们是路由身份。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveSenders")
            .field("installations", &self.len())
            .field("has_metrics", &self.has_metrics())
            .field("quota_shards", &self.quotas.len())
            .field("retry_backoff", &self.retry.backoff())
            .finish_non_exhaustive()
    }
}

impl Default for LiveSenders {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveSenders {
    /// 铸一张空表（上游 `newSendersRegistry`）：一个没配汇的表**丢掉**每一个计数器 —— 那正是
    /// `/metrics` 关掉的部署拿到的东西。
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            metrics: RwLock::new(None),
            quotas: QuotaShards::default(),
            retry: RetryPlan::default(),
        }
    }

    /// 换掉退避策略（用例把一次重试跑得不用等两秒）。
    #[must_use]
    pub fn with_retry_plan(mut self, retry: RetryPlan) -> Self {
        self.retry = retry;
        self
    }

    /// 换掉配额表（用例把窗口缩到毫秒）。
    #[must_use]
    pub fn with_quotas(mut self, quotas: QuotaShards) -> Self {
        self.quotas = quotas;
        self
    }

    /// 把汇指到一个真的去处（上游 `WithMetrics`）。启动时调一次，在任何连接存在**之前**。
    ///
    /// 它属于**注册表**而不是每个调用方的构造器（上游逐字）：这是每一次出站写**已经**要经过的
    /// 那**一个**对象，而且它已经被需要报告的那一位（出站订阅者）拿着 —— 那位没有别的理由知道
    /// metrics 存在。
    pub fn with_metrics(&self, metrics: &'static dyn Metrics) {
        match self.metrics.write() {
            Ok(mut guard) => *guard = Some(metrics),
            Err(poisoned) => *poisoned.into_inner() = Some(metrics),
        }
    }

    /// 汇（**永远**可调用：没配时是 no-op）。
    #[must_use]
    pub fn metrics(&self) -> &'static dyn Metrics {
        let configured = match self.metrics.read() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        };
        or_nop_metrics(configured)
    }

    /// 配了汇吗（诊断与用例用）。
    #[must_use]
    pub fn has_metrics(&self) -> bool {
        match self.metrics.read() {
            Ok(guard) => guard.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        }
    }

    fn lock(&self) -> std::sync::RwLockReadGuard<'_, HashMap<Id, Arc<WsSender>>> {
        match self.by_key.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn lock_mut(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<Id, Arc<WsSender>>> {
        match self.by_key.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 这台发送者此刻在册吗（诊断与用例用；**不**铸桶、也不产生任何副作用）。
    #[must_use]
    pub fn holds(&self, installation_id: Id) -> bool {
        self.lock().contains_key(&installation_id)
    }

    /// 表里几条活连接。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 空吗。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 配额表（本片专属验收那一条的接缝：宿主与用例都从这里读"哪条安装的桶"）。
    #[must_use]
    pub fn quotas(&self) -> &QuotaShards {
        &self.quotas
    }

    /// 这台发送者自己的那把 socket（**只**给本 crate 里需要 `WsSender` 具体类型的地方：
    /// `route_response` 那一类）。表里没有时是 `None`。
    #[must_use]
    pub(crate) fn raw(&self, installation_id: Id) -> Option<Arc<WsSender>> {
        self.lock().get(&installation_id).map(Arc::clone)
    }

    /// 一条安装的发送面（读侧 + 配额门）。
    fn install(&self, installation_id: Id) -> Option<InstallationSender> {
        let sender = self.raw(installation_id)?;
        Some(InstallationSender::new(
            sender,
            self.quotas.bucket(installation_id),
            self.retry,
        ))
    }

    /// 记下一个气泡正在屏幕上、并且有人欠它一个收尾（上游 `recordOpened`）。
    ///
    /// 它与 [`StreamSender::record_ending`] 分开，是因为两者从**相反的两侧**被写：一次结束在
    /// `seal` 里面就知道了，而一次**开场**只有决定留下句柄的那个调用方知道。
    pub fn record_opened(&self) {
        self.metrics().record_stream_opened();
    }

    /// 推一条 `aibot_send_msg` 纯文本 —— 每一个收尾帧降级到的那个兜底（上游 `sendTextCtx`）。
    ///
    /// 与流帧分开是因为一条消息**没有** `req_id` 会过期：这是气泡已经救不回来时仍然好使的那条路。
    ///
    /// # Errors
    ///
    /// 没有活连接时 [`SenderError::NotAttempted`]；否则与 [`send_msg_frame`] 同。
    pub async fn send_text(
        &self,
        installation_id: Id,
        chat_id: &str,
        chat_type: i32,
        content: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        let Some(install) = self.install(installation_id) else {
            tracing::warn!(
                installation_id = %installation_id,
                "wecom: no live connection for this installation"
            );
            return Err(no_live_connection());
        };
        install
            .send_text(chat_id, chat_type, content, deadline)
            .await
    }

    /// 这条安装的配额桶（诊断与用例用）。
    #[must_use]
    pub fn quota_for(&self, installation_id: Id) -> Arc<SendQuota> {
        self.quotas.bucket(installation_id)
    }
}

// =====================================================================
// 写侧（上游 `set` / `clear`）
// =====================================================================

impl SenderRegistry for LiveSenders {
    /// 装上这条安装的活 socket（上游 `senders.set`）。
    fn set(&self, installation_id: Id, sender: Arc<WsSender>) {
        self.lock_mut().insert(installation_id, sender);
    }

    /// 撤下它 —— **只在这条安装当前登记的仍是 `sender` 时**（见模块文档那一段）。
    fn clear(&self, installation_id: Id, sender: &Arc<WsSender>) {
        let mut table = self.lock_mut();
        let still_ours = table
            .get(&installation_id)
            .is_some_and(|current| Arc::ptr_eq(current, sender));
        if !still_ours {
            // 一个正在收尾的**代**碰上了它的**继任者**：留着继任者（上游逐字的两条后果）。
            return;
        }
        table.remove(&installation_id);
        // 配额桶**故意不动**：它跟着 installation 走，一次重连不该把已经花掉的额度清零
        // （上游自己那条缝；见 `rate_limit` 的模块文档差异 1）。
    }
}

// =====================================================================
// 读侧（上游 `get` + `streamSender`）
// =====================================================================

impl SenderLookup for LiveSenders {
    /// 活着的发送者，或者 `None`（上游 `get`）。
    ///
    /// `None` = 本副本这条安装没有活连接（监管器丢了租约 / 正在重连）—— **不是**错误。
    fn get(&self, installation_id: Id) -> Option<Arc<dyn LiveSender>> {
        let install = self.install(installation_id)?;
        Some(Arc::new(install))
    }

    /// 收尾帧要的那一半（上游 `stream` / `streamRewrite` / `recordEnding`）。
    fn stream_sender(&self) -> Option<&dyn StreamSender> {
        Some(self)
    }
}

// =====================================================================
// 流面（上游 `stream` / `streamRewrite` / `recordEnding`）
// =====================================================================

#[async_trait]
impl StreamSender for LiveSenders {
    /// 写一帧流帧（上游 `sendersRegistry.stream`）。
    ///
    /// ⚠️ 上游逐字：发送者是在**这里**、**按帧**解出来的，而不是在气泡打开时捕获的，这一条是
    /// **承重**的而不是多余的 —— 一个回调的 `req_id` 属于 `WeCom` 那一侧的**轮次**，不属于它到达的
    /// 那把连接。断开之前开的气泡因此在断开**之后**由那条安装**当时**握着的那把 socket 写完，
    /// 而那正是让一次运行比它的问题到达时的那把连接活得更久的东西。把连接绑在流打开的那一刻读起来
    /// 像一次显然的收紧，却会让每一个重连都把气泡搁浅 —— 一个只在连接抖动时出现、**永远**不会在
    /// 用例里出现的失败。
    ///
    /// # Errors
    ///
    /// 没有活连接时 [`SenderError::NotAttempted`]；否则与发送侧同。
    async fn stream(
        &self,
        handle: &StreamHandle,
        text: &str,
        finish: bool,
    ) -> Result<(), SenderError> {
        let Some(installation_id) = handle.installation_id else {
            return Err(no_live_connection());
        };
        let Some(sender) = self.raw(installation_id) else {
            return Err(no_live_connection());
        };
        // `None` = 上游 `context.Background()`（模块文档差异 3）。
        sender
            .respond_stream(&handle.req_id, &handle.stream_id, text, finish, None)
            .await
    }

    /// 把**同一帧**再写一遍 —— `seal` 对一次判决没回来的收尾帧的重试（上游 `streamRewrite`）。
    ///
    /// # Errors
    ///
    /// 同 [`StreamSender::stream`]。
    async fn stream_rewrite(
        &self,
        handle: &StreamHandle,
        text: &str,
        finish: bool,
    ) -> Result<(), SenderError> {
        let Some(installation_id) = handle.installation_id else {
            return Err(no_live_connection());
        };
        let Some(sender) = self.raw(installation_id) else {
            return Err(no_live_connection());
        };
        sender
            .respond_stream_rewrite(&handle.req_id, &handle.stream_id, text, finish, None)
            .await
    }

    /// 记一次结束（上游 `recordEnding`）：**两半在一条线上**，由这一帧最后的错误决定。
    ///
    /// 上游逐字：**每一个**收尾器都从这里过 —— 回答、打字指示写的失败与取消告知、以及落定的 flush。
    /// 一对（`finished` / `fell_back`）记在一起是因为**这一对就是信号**（比值才说明气泡还管不管用），
    /// 而一个只从一处喂的计数器读起来像一个健康的比值，同时一整类结束已经悄悄不再用那个气泡了。
    ///
    /// 一个**被服务端收下**的收尾帧是一个以文字结束的气泡；一个被它拒了的会把调用方送去一条普通
    /// 消息，那在**每一个**调用点都毫无例外是一次降级。
    fn record_ending(&self, error: Option<&SenderError>) {
        match error {
            None => self.metrics().record_stream_finished(),
            Some(_) => self.metrics().record_stream_fell_back(),
        }
    }
}

/// `LiveSender` 那一层要的截止时刻默认值（"该等多久就等多久"，上游 `context.Background()`）。
///
/// 它是给调用方读的常量而不是一个魔数：**没有一个**内置调用方该在收尾或推送时挂一个自己的预算
/// 上去（预算属于**发起那件事**的人）。
pub const NO_DEADLINE: Deadline = None;

#[cfg(test)]
mod tests;
