//! 回复投递的**所有权账**（上游 `internal/integrations/telegram/delivery.go`，457 行）。
//!
//! - **写者**：M7-6（`LUM-1771`；`docs/60-M7-PLAN.md` §3.3）。
//!
//! # 这一行是干什么的
//!
//! 一个任务的回复会从三条路径到达 Telegram：**流式占位消息**、**最终答案**、**失败告知**。
//! 多副本部署里它们还跑在**三个进程**里（daemon 的 transcript 上报与完成回调是两个独立 HTTP
//! 请求，可能落在不同副本），而事件总线是**进程内**的。看不见占位消息的那个进程会**自己再发
//! 一份**同样的答案 —— 那就是用户报的重复消息（上游 GH #8049 / #7750）。
//!
//! `channel_reply_delivery` 是它们达成一致的**唯一**地方，而本文件是**唯一**碰它的路径。
//! 三条规则贯穿全文件（上游注释逐字）：
//!
//! 1. **一个用户轮次，一个所有者**：任何路径都要**先取这一轮的租约**再调 Telegram，
//!    并在记录结果时证明自己**仍然**持有租约。"UPDATE 是原子的"**不等于**"只有一个进程在投递"；
//! 2. **占位不是进度**：有一条可编辑的消息与"最终答案投了多少"毫无关系；把两者混起来
//!    会**静默截断**回复（`chunks_sent` 与 `message_id` 是两个字段的理由就是这条）；
//! 3. **结果丢了的发送就让它丢着**：`sendMessage` **没有**调用方给的幂等键，所以重发**无法**
//!    由平台去重。结果未知 ⇒ 投递结束，并把证据（`send_state = 'unknown'`）留在行里。
//!
//! # 与上游的两处形态差异（登记 `docs/32` §18）
//!
//! - **端口化**：上游直接依赖生成的 `db.Queries`（12 条 SQL）；本仓的 `mc-channel` 不得直接
//!   写 DB（`docs/60` §2.6 第 1 条）⇒ 这里定义 [`DeliveryStore`] 端口，PG 实现
//!   （[`PgDeliveryStore`]）只是 `mc_repos::channel::ChannelDeliveryRepo` 的薄适配
//!   （那几条语句是 M7-6 在 `mc-repos` 追加的，写集勘误见 `docs/32` §18 D1）。
//!   于是**语义逐条照搬**（含"不围栏在 owner 上的 unknown 写"这种细节），而"adapter 不碰 DB"
//!   仍是类型层面的事实；
//! - **无后台调度器**：上游的 `Outbound` 自带 terminal worker 池 / 重试堆 / 清扫器（它们只服务
//!   事件总线，而本仓没有那条总线 ⇒ 见 `outbound.rs` 的偏离登记）。本文件只留**状态机本身**，
//!   所有方法都是可 `await` 的显式调用。
//!
//! # 时间预算（不是装饰，是可测的不变式）
//!
//! 租约 [`LEASE_TTL`] 必须**大于**一次 Telegram 往返（租约**跨调用**持有：短了会让第二个进程
//! 在第一个还在说话时接管）；而一次调用必须**短于**租约，否则调用可能在租约失效**之后**才落地。
//! [`DeliveryLedger::call_budget`] 把这条关系写成代码，`delivery/tests.rs` 有对应用例。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::delivery::{
    ChannelDeliveryRepo, ChannelReplyDeliveryRow, CloseReplyDeliveryTurn, NewReplyDeliveryAttempt,
};

use crate::telegram::api::ApiError;

/// `phase`：流式占位消息就是活着的回复，还可以被编辑。
pub const PHASE_STREAMING: &str = "streaming";
/// `phase`：最终答案已经接管，占位消息不许再被重开。
pub const PHASE_TERMINAL: &str = "terminal";
/// `phase`：投递结束，之后这个 turn 谁都不许再发/编辑。
pub const PHASE_SETTLED: &str = "settled";

/// `send_state`：什么都没发出去。
pub const SEND_NONE: &str = "none";
/// `send_state`：有一条发送在飞（平台**可能**已经收下了）。
pub const SEND_IN_FLIGHT: &str = "in_flight";
/// `send_state`：平台收下了，`message_id` 指着那条可编辑的消息。
pub const SEND_KNOWN: &str = "known";
/// `send_state`：**响应丢了** —— 收没收下不知道。投递停下并保留证据。
pub const SEND_UNKNOWN: &str = "unknown";

/// 投递租约的长度（上游 `deliveryLeaseTTL = 30s`）。
///
/// 两个方向都被它约束：必须**长过**一次 Telegram 往返（租约跨调用持有），又必须是
/// "一个死掉的进程最多能挡住这一轮多久"。
pub const LEASE_TTL: Duration = Duration::from_secs(30);

/// 一次持有租约的**平台调用**的超时（上游 `deliveryCallTimeout = 20s`）。
///
/// 共用的 Bot API 客户端允许 65 秒（`getUpdates` 的长轮询需要），但投递调用**绝不能**这么长：
/// 超出租约的请求可能在另一个进程已经接管并回答完之后才落地。`call + record` 必须装进租约。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// 记录"平台做了什么"的那几条写的超时（上游 `deliveryRecordTimeout = 5s`）。
///
/// 它们跑在**脱离调用方**的上下文上：把发送掐死的那个 deadline 不该顺便把"这次发送发生过"
/// 也抹掉。
pub const RECORD_TIMEOUT: Duration = Duration::from_secs(5);

/// 抢一个被别的进程持有的轮次时的重试间隔（上游 `deliveryBusyRetry = 250ms`）。
pub const BUSY_RETRY: Duration = Duration::from_millis(250);

/// 抢租约的**最多**尝试次数（上游 `maxDeliveryAcquireAttempts = 240`）。
///
/// 它必须能**等过一个租约的长度**：租约是"上一个进程死了"的唯一自由机制。
pub const MAX_ACQUIRE_ATTEMPTS: u32 = 240;

/// 所有权写**自己**失败时的预算（上游 `maxDeliveryClaimErrorAttempts = 5`）。
///
/// 那是**数据库**问题，不是"轮次被别人占着"：为一个不听话的库把会话队列按住一分钟，
/// 到最后一秒回复也还是投不出去。
pub const MAX_CLAIM_ERROR_ATTEMPTS: u32 = 5;

/// 投递账端口失败：**只带一条不含凭据的说明**（`docs/60` §2.3 第 2 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("telegram delivery store: {message}")]
pub struct DeliveryStoreError {
    /// 说明（不含 token / URL / chat 级凭据）。
    pub message: String,
}

impl DeliveryStoreError {
    /// 装配。
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// 本文件的 `Result` 别名。
pub type DeliveryResult<T> = Result<T, DeliveryStoreError>;

/// 一个 turn 的身份：它的目标行信息（上游 `replyTarget` 的最小面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTarget {
    /// 产生这一轮的**任务**（自动重试是新任务，但轮次相同）。
    pub task_id: Id,
    /// 会话绑定（`channel_chat_session_binding.id`）。
    pub binding_id: Id,
    /// 安装行。
    pub installation_id: Id,
    /// 平台判别式（本 adapter 恒 `Telegram`，留着是为了行里的判别字段）。
    pub kind: ChannelKind,
    /// **路由键**形态的会话 id（不是数值 chat id）。
    pub chat_id: String,
}

/// 一个用户轮次：自动重试链的**根**，以及本次尝试在链上的深度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyTurn {
    /// 轮次 id（链的根；任务没有队列行时就是它自己）。
    pub id: Id,
    /// 深度（0 = 根）。只前进 —— 迟到的旧尝试不能把轮次抢回去。
    pub depth: i32,
}

/// 本进程对**一条**轮次回复的持有，连同它接手时看到的行状态。
///
/// 每一笔写都经过租约 ⇒ 调用方**无法**针对一个自己已经不再拥有的轮次记录结果。
#[derive(Debug, Clone)]
pub struct DeliveryLease {
    turn_id: Id,
    token: Id,
    row: ChannelReplyDeliveryRow,
}

impl DeliveryLease {
    /// 轮次 id。
    #[must_use]
    pub fn turn_id(&self) -> Id {
        self.turn_id
    }

    /// 本进程的令牌（围栏用）。
    #[must_use]
    pub fn token(&self) -> Id {
        self.token
    }

    /// 接手时看到的那一行。
    #[must_use]
    pub fn row(&self) -> &ChannelReplyDeliveryRow {
        &self.row
    }

    /// 这一轮拥有的那条可编辑消息（上游 `deliveryLease.messageID`）。
    ///
    /// 在飞或丢了的发送**没有**可用的 id（`0` = 没有可编辑的消息）。
    #[must_use]
    pub fn message_id(&self) -> i64 {
        if self.row.send_state != SEND_KNOWN || self.row.message_id.is_empty() {
            return 0;
        }
        self.row.message_id.parse().unwrap_or(0)
    }

    /// 最终答案已经进聊天的**片数**（上游 `chunksSent`）。
    #[must_use]
    pub fn chunks_sent(&self) -> i32 {
        self.row.chunks_sent
    }

    /// 当前的 `send_state`。
    #[must_use]
    pub fn send_state(&self) -> &str {
        &self.row.send_state
    }

    /// 这一轮是否已经收口。
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.row.phase == PHASE_SETTLED
    }
}

/// 平台对**一次发送**做了什么（上游 `deliveryOutcome`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// 平台给出了 message id。
    Accepted,
    /// 平台**回答并拒绝**了。聊天里什么都没有 ⇒ 这一轮可以再试。
    Refused,
    /// 没有答案回来。消息**可能**已经在聊天里。
    Unknown,
}

impl DeliveryOutcome {
    /// 稳定的字面量（日志与用例共用）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }
}

/// 一次取租约**为什么没有交出去**（上游 `deliveryStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// 拿到了。
    Acquired,
    /// 另一个进程持有**活着的**租约。等一会儿重试。
    Busy,
    /// 这一轮已经收口，或者最终答案接管了一个流式帧想要的回复。**停手**。
    Closed,
}

/// 取租约的入参（上游 `AcquireChannelReplyDeliveryParams` 的本仓形态）。
#[derive(Debug, Clone)]
pub struct AcquireDelivery {
    /// 轮次。
    pub turn: ReplyTurn,
    /// 目标。
    pub target: DeliveryTarget,
    /// [`PHASE_STREAMING`] / [`PHASE_TERMINAL`]。
    pub phase: String,
    /// 本进程的令牌（每次取租约都换一把新的）。
    pub token: Id,
    /// 租约长度（秒）。
    pub lease_seconds: f64,
}

/// 收口一个没有答案的轮次（上游 `CloseChannelReplyDeliveryTurnParams`）。
#[derive(Debug, Clone)]
pub struct CloseDeliveryTurn {
    /// 轮次。
    pub turn: ReplyTurn,
    /// 目标。
    pub target: DeliveryTarget,
    /// 收口原因（`cancelled` / `completed_empty` / `delivered` …）。
    pub reason: String,
}

/// 投递账的**端口**：状态机只认它，于是"adapter 不得直接写 DB"仍是类型层面的事实。
///
/// 十二个方法逐一对应上游 `outboundQueries` 里那十二条语句（序号见 `docs/32` §18）。
#[async_trait]
pub trait DeliveryStore: Send + Sync {
    /// 解析任务所属的**用户轮次**。
    ///
    /// `Ok(None)` = 这个任务没有队列行 —— 上游注释：那是**事实，不是失败**
    /// （调用方按"它就是自己的轮次、深度 0"处理）。别的错误必须报：猜"它是自己的轮次"
    /// 会在前一次尝试仍持有的回复**旁边**再开一个，而那正是这条血缘要防的 bug。
    async fn turn_for(&self, task_id: Id) -> DeliveryResult<Option<ReplyTurn>>;

    /// 取/建轮次的投递租约，`Ok(None)` = 三种"不许碰平台"的情形之一。
    async fn acquire(
        &self,
        acquire: &AcquireDelivery,
    ) -> DeliveryResult<Option<ChannelReplyDeliveryRow>>;

    /// 只读一行（不取租约）⇒ 丢掉竞争的那一方能区分"别人在干"与"这一轮结束了"。
    async fn read(&self, turn_id: Id) -> DeliveryResult<Option<ChannelReplyDeliveryRow>>;

    /// 交还租约。
    async fn release(&self, turn_id: Id, token: Id) -> DeliveryResult<bool>;

    /// 续租（`false` = 已被接管 ⇒ 停手）。
    async fn renew(&self, turn_id: Id, token: Id, lease_seconds: f64) -> DeliveryResult<bool>;

    /// 在**发出之前**公开一条发送。
    async fn claim_send(&self, turn_id: Id, token: Id) -> DeliveryResult<bool>;

    /// 占位消息落地（不动 `chunks_sent`）。
    async fn record_placeholder(
        &self,
        turn_id: Id,
        token: Id,
        message_id: &str,
    ) -> DeliveryResult<bool>;

    /// 最终答案的一片落地。
    async fn record_chunk(
        &self,
        turn_id: Id,
        token: Id,
        message_id: &str,
        chunks_sent: i32,
    ) -> DeliveryResult<bool>;

    /// 平台拒绝 ⇒ 回到可再试的状态。
    async fn reset_send(&self, turn_id: Id, token: Id) -> DeliveryResult<bool>;

    /// 把在飞的发送记成**结果未知**（不围栏在 token 上）。
    async fn mark_send_unknown(&self, turn_id: Id) -> DeliveryResult<bool>;

    /// 收口。
    async fn settle(&self, turn_id: Id, token: Id, reason: &str) -> DeliveryResult<bool>;

    /// 收口一个**没有答案**的轮次（必要时建行）。
    async fn close_turn(
        &self,
        close: &CloseDeliveryTurn,
    ) -> DeliveryResult<Option<ChannelReplyDeliveryRow>>;
}

/// PG 实现（上游直接依赖生成查询的那一层的薄适配）。
#[derive(Clone)]
pub struct PgDeliveryStore {
    repo: ChannelDeliveryRepo,
}

impl fmt::Debug for PgDeliveryStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 仓储里只有连接池；端口**没有**任何凭据字段。
        formatter.debug_struct("PgDeliveryStore").finish()
    }
}

impl PgDeliveryStore {
    /// 装配。
    ///
    /// 收的是**仓储**而不是连接池：`mc-channel` 的依赖面（anchor 冻结）里没有 `mc-db`
    /// ⇒ 连接池的构造留在宿主（`mc-http` 的 `AppState`）那一侧，这里只做端口适配。
    #[must_use]
    pub fn new(repo: ChannelDeliveryRepo) -> Self {
        Self { repo }
    }
}

/// 仓储错误 → 端口错误（只保留说明文本：`RepoError` 的 `Display` 不含凭据）。
#[allow(clippy::needless_pass_by_value)]
fn store_error(error: mc_repos::RepoError) -> DeliveryStoreError {
    DeliveryStoreError::new(error.to_string())
}

#[async_trait]
impl DeliveryStore for PgDeliveryStore {
    async fn turn_for(&self, task_id: Id) -> DeliveryResult<Option<ReplyTurn>> {
        let row = self
            .repo
            .get_reply_turn(task_id)
            .await
            .map_err(store_error)?;
        Ok(row.and_then(|row| {
            row.turn_id.map(|turn_id| ReplyTurn {
                id: Id(turn_id),
                depth: row.attempt_depth,
            })
        }))
    }

    async fn acquire(
        &self,
        acquire: &AcquireDelivery,
    ) -> DeliveryResult<Option<ChannelReplyDeliveryRow>> {
        let attempt = NewReplyDeliveryAttempt {
            turn_id: acquire.turn.id,
            task_id: acquire.target.task_id,
            attempt_depth: acquire.turn.depth,
            binding_id: acquire.target.binding_id,
            installation_id: acquire.target.installation_id,
            kind: acquire.target.kind,
            chat_id: acquire.target.chat_id.clone(),
            phase: acquire.phase.clone(),
            owner_token: acquire.token,
            lease_seconds: acquire.lease_seconds,
        };
        self.repo
            .acquire_reply_delivery(&attempt)
            .await
            .map_err(store_error)
    }

    async fn read(&self, turn_id: Id) -> DeliveryResult<Option<ChannelReplyDeliveryRow>> {
        self.repo
            .get_reply_delivery(turn_id)
            .await
            .map_err(store_error)
    }

    async fn release(&self, turn_id: Id, token: Id) -> DeliveryResult<bool> {
        self.repo
            .release_reply_delivery(turn_id, token)
            .await
            .map_err(store_error)
    }

    async fn renew(&self, turn_id: Id, token: Id, lease_seconds: f64) -> DeliveryResult<bool> {
        self.repo
            .renew_reply_delivery(turn_id, token, lease_seconds)
            .await
            .map_err(store_error)
    }

    async fn claim_send(&self, turn_id: Id, token: Id) -> DeliveryResult<bool> {
        self.repo
            .mark_reply_delivery_sending(turn_id, token)
            .await
            .map_err(store_error)
    }

    async fn record_placeholder(
        &self,
        turn_id: Id,
        token: Id,
        message_id: &str,
    ) -> DeliveryResult<bool> {
        self.repo
            .record_reply_delivery_placeholder(turn_id, token, message_id)
            .await
            .map_err(store_error)
    }

    async fn record_chunk(
        &self,
        turn_id: Id,
        token: Id,
        message_id: &str,
        chunks_sent: i32,
    ) -> DeliveryResult<bool> {
        self.repo
            .record_reply_delivery_chunk(turn_id, token, message_id, chunks_sent)
            .await
            .map_err(store_error)
    }

    async fn reset_send(&self, turn_id: Id, token: Id) -> DeliveryResult<bool> {
        self.repo
            .reset_reply_delivery_send(turn_id, token)
            .await
            .map_err(store_error)
    }

    async fn mark_send_unknown(&self, turn_id: Id) -> DeliveryResult<bool> {
        self.repo
            .mark_reply_delivery_send_unknown(turn_id)
            .await
            .map_err(store_error)
    }

    async fn settle(&self, turn_id: Id, token: Id, reason: &str) -> DeliveryResult<bool> {
        // 仓储层的 `settle_reply_delivery` **故意不围栏在 owner 上**（超时清扫也必须能收口），
        // 所以这里的 `token` 只用于端口签名的一致性：写成功了就是收口了。
        let _ = token;
        Ok(self
            .repo
            .settle_reply_delivery(turn_id, reason)
            .await
            .map_err(store_error)?
            .is_some())
    }

    async fn close_turn(
        &self,
        close: &CloseDeliveryTurn,
    ) -> DeliveryResult<Option<ChannelReplyDeliveryRow>> {
        self.repo
            .close_reply_delivery_turn(&CloseReplyDeliveryTurn {
                turn_id: close.turn.id,
                task_id: close.target.task_id,
                attempt_depth: close.turn.depth,
                binding_id: close.target.binding_id,
                installation_id: close.target.installation_id,
                kind: close.target.kind,
                chat_id: close.target.chat_id.clone(),
                settled_reason: close.reason.clone(),
            })
            .await
            .map_err(store_error)
    }
}

/// 平台是否**确实**拒绝了（上游 `isDefiniteRejection`）：只有客户端错误（4xx）算。
///
/// **5xx 不算**：Telegram 可能收下了 `sendMessage` 却在回程上失败，把那种情况当成
/// "什么都没发"正是答案进聊天两次的成因。4xx（含 429，它是**直接拒绝**）才是证明。
#[must_use]
pub fn is_definite_rejection(error: &ApiError) -> bool {
    error
        .http_code()
        .is_some_and(|code| (400..500).contains(&code))
}

/// 把一次 Bot API 结果归类成"这一轮现在知道了什么"（上游 `classifySend`）。
#[must_use]
pub fn classify_send(result: Result<(), &ApiError>) -> DeliveryOutcome {
    match result {
        Ok(()) => DeliveryOutcome::Accepted,
        Err(error) if is_definite_rejection(error) => DeliveryOutcome::Refused,
        Err(_) => DeliveryOutcome::Unknown,
    }
}

/// Telegram 的 `message is not modified` —— **良性**（快照一样），调用方吞掉。
#[must_use]
pub fn is_not_modified(error: &ApiError) -> bool {
    error.http_code() == Some(400) && description_contains(error, "message is not modified")
}

/// Telegram 确认那条消息没了（上游 `isEditTargetMissing`）。
///
/// **只有**这一种编辑失败才值得**另发一条新消息**：其余的失败都可能让原消息留在聊天里，
/// 在它旁边再发一条就是重复。
#[must_use]
pub fn is_edit_target_missing(error: &ApiError) -> bool {
    error.http_code() == Some(400) && description_contains(error, "message to edit not found")
}

/// 重试**修不好**的编辑拒绝（上游 `isPermanentEditRejection`）：bot 被封 / 失权 / 消息不再可编。
///
/// 调用方必须**先**排除掉可恢复的 400（markup 错、目标没了）——它们共用状态码。
#[must_use]
pub fn is_permanent_edit_rejection(error: &ApiError) -> bool {
    matches!(error.http_code(), Some(400 | 401 | 403 | 404))
}

/// 描述文本里是否含某个片段（大小写无关）；非 `Api` 变体恒 `false`。
#[must_use]
fn description_contains(error: &ApiError, needle: &str) -> bool {
    match error {
        ApiError::Api { description, .. } => description.to_lowercase().contains(needle),
        _ => false,
    }
}

/// 投递状态机（上游 `Outbound` 里那半边"所有权 + 记账"的代码）。
///
/// 它**不**做 send / edit（那是 `outbound.rs` 与 `sender.rs` 的事），只保证：
/// 谁持有轮次、什么能写、写完记成什么。
pub struct DeliveryLedger {
    store: Arc<dyn DeliveryStore>,
    lease_ttl: Duration,
}

impl fmt::Debug for DeliveryLedger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeliveryLedger")
            .field("store", &"<dyn DeliveryStore>")
            .field("lease_ttl", &self.lease_ttl)
            .finish()
    }
}

impl DeliveryLedger {
    /// 装配（生产租约长度）。
    #[must_use]
    pub fn new(store: Arc<dyn DeliveryStore>) -> Self {
        Self {
            store,
            lease_ttl: LEASE_TTL,
        }
    }

    /// 注入更短的租约（用例用它检验"接管"）。
    #[must_use]
    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Self {
        self.lease_ttl = lease_ttl;
        self
    }

    /// 本 ledger 发出的租约长度（秒）。
    #[must_use]
    pub fn lease_seconds(&self) -> f64 {
        self.lease_ttl.as_secs_f64()
    }

    /// 一次平台调用允许占用的预算（上游 `callContext`）。
    ///
    /// 默认 [`CALL_TIMEOUT`]；租约被调短时（用例）预算按租约的**三分之一**收缩，
    /// 否则"调用装得进租约"这条被检验的关系就不成立了。
    #[must_use]
    pub fn call_budget(&self) -> Duration {
        if self.lease_ttl < LEASE_TTL {
            self.lease_ttl / 3
        } else {
            CALL_TIMEOUT
        }
    }

    /// 解析任务所属的轮次（上游 `turnFor`）。
    pub async fn turn_for(&self, task_id: Id) -> DeliveryResult<ReplyTurn> {
        Ok(self.store.turn_for(task_id).await?.unwrap_or(ReplyTurn {
            id: task_id,
            depth: 0,
        }))
    }

    /// 取这一轮在某个阶段（streaming / terminal）的租约（上游 `acquireDelivery`）。
    ///
    /// 拿不到时**必须区分三种原因**（上游逐条）：收口、被最终答案接管、别人活着占着 ——
    /// 前两种是终局，第三种值得等。
    pub async fn acquire(
        &self,
        target: &DeliveryTarget,
        turn: ReplyTurn,
        phase: &str,
    ) -> DeliveryResult<(Option<DeliveryLease>, DeliveryStatus)> {
        let token = Id::new();
        let acquire = AcquireDelivery {
            turn,
            target: target.clone(),
            phase: phase.to_string(),
            token,
            lease_seconds: self.lease_seconds(),
        };
        if let Some(row) = self.store.acquire(&acquire).await? {
            return Ok((
                Some(DeliveryLease {
                    turn_id: turn.id,
                    token,
                    row,
                }),
                DeliveryStatus::Acquired,
            ));
        }

        // 没有行 = 轮次被拒了，而三种原因需要三种答案（上游逐条）。
        let Some(current) = self.store.read(turn.id).await? else {
            // 与一次并发收口擦肩而过；没有可投递的东西。
            return Ok((None, DeliveryStatus::Closed));
        };
        if current.phase == PHASE_SETTLED {
            return Ok((None, DeliveryStatus::Closed));
        }
        if phase == PHASE_STREAMING && current.phase == PHASE_TERMINAL {
            // 最终答案已经接管：流式帧不许重写用户正在读的东西。
            return Ok((None, DeliveryStatus::Closed));
        }
        if turn.depth < current.attempt_depth {
            // 这一轮已经被更深的尝试接管 —— 它不是"在等"，而是已经被**取代**了。
            return Ok((None, DeliveryStatus::Closed));
        }
        Ok((None, DeliveryStatus::Busy))
    }

    /// 交还租约（上游 `releaseDelivery`）：尽力而为，没交还的租约自己过期。
    pub async fn release(&self, lease: &DeliveryLease) -> DeliveryResult<bool> {
        self.store.release(lease.turn_id(), lease.token()).await
    }

    /// 续租（上游 `renewDelivery`）：`false` = 别的进程现在拥有这一轮，必须停手。
    pub async fn renew(&self, lease: &DeliveryLease) -> DeliveryResult<bool> {
        self.store
            .renew(lease.turn_id(), lease.token(), self.lease_seconds())
            .await
    }

    /// 在发出之前公开这条发送（上游 `claimSend`）：`false` = **不要发**。
    pub async fn claim_send(&self, lease: &DeliveryLease) -> DeliveryResult<bool> {
        self.store.claim_send(lease.turn_id(), lease.token()).await
    }

    /// 接手时读一次"上一条发送的结局"（上游 `inheritedSend`）。
    ///
    /// 接手一个前持有者**死在发送中途**的轮次**不等于**什么都没发 —— 它等于**没人知道**。
    /// 把这条记下来，后继者才不会发出第二份；这一轮也才能收尾而不是永远等一个租约。
    pub async fn inherited_send(&self, lease: &DeliveryLease) -> DeliveryResult<bool> {
        match lease.send_state() {
            SEND_UNKNOWN => Ok(true),
            SEND_IN_FLIGHT => {
                self.store.mark_send_unknown(lease.turn_id()).await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// 记下平台做了什么（上游 `recordSend`）。
    ///
    /// `placeholder` 区分**流式占位消息**（给这一轮一条可编辑的消息）与**最终答案的一片**
    /// （那是进度）；`chunks_sent` 对占位消息被忽略。
    pub async fn record_send(
        &self,
        lease: &DeliveryLease,
        placeholder: bool,
        message_id: i64,
        chunks_sent: i32,
        outcome: DeliveryOutcome,
    ) -> DeliveryResult<DeliveryOutcome> {
        let turn_id = lease.turn_id();
        let token = lease.token();
        let message_id = message_id.to_string();
        match outcome {
            DeliveryOutcome::Accepted => {
                if placeholder {
                    self.store
                        .record_placeholder(turn_id, token, &message_id)
                        .await?;
                } else {
                    self.store
                        .record_chunk(turn_id, token, &message_id, chunks_sent)
                        .await?;
                }
            }
            DeliveryOutcome::Refused => {
                self.store.reset_send(turn_id, token).await?;
            }
            DeliveryOutcome::Unknown => {
                // **故意不围栏在 owner 上**：这条写必须落在"我们的租约在请求挂死时过期了"
                // 之后 —— 后继者读这一行时不能得出"什么都没发过"。
                self.store.mark_send_unknown(turn_id).await?;
            }
        }
        Ok(outcome)
    }

    /// 收口这一轮（上游 `settleDelivery`）：之后谁都不许再为它发/编辑，
    /// 包括任务结束时仍在飞的那一帧文本。
    pub async fn settle(&self, lease: &DeliveryLease, reason: &str) -> DeliveryResult<bool> {
        self.store
            .settle(lease.turn_id(), lease.token(), reason)
            .await
    }

    /// 收口一个**没有答案**的轮次（上游 `closeTurn`）。
    ///
    /// 取消 / 完成但为空时用。它在轮次**从没碰过平台**时也要建行：没有这一条，取消之后才到的
    /// 第一帧文本会"找不到行 ⇒ 开出占位消息 ⇒ 永远没人收尾"。
    ///
    /// 返回值 = "这一轮现在确实收口了"（已被更深的尝试接管也算 —— 收口不是这次尝试的活）。
    pub async fn close_turn(
        &self,
        target: &DeliveryTarget,
        turn: ReplyTurn,
        reason: &str,
    ) -> DeliveryResult<bool> {
        let close = CloseDeliveryTurn {
            turn,
            target: target.clone(),
            reason: reason.to_string(),
        };
        if self.store.close_turn(&close).await?.is_some() {
            return Ok(true);
        }
        let Some(current) = self.store.read(turn.id).await? else {
            return Ok(false);
        };
        if turn.depth < current.attempt_depth {
            // 更深的尝试拥有这一轮：收口不是这次尝试的活（而且等着只会把会话队列按住）。
            return Ok(true);
        }
        Ok(current.phase == PHASE_SETTLED)
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
