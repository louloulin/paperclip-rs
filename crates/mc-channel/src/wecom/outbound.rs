//! `WeCom` **出站面**：agent 的回答怎么回到 `WeCom`
//! （上游 `internal/integrations/wecom/outbound.go`，**887 行**）。
//!
//! - **写者**：M7-17（`LUM-1782` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：agent 的回答离开本进程的方式，与**每一条**别的
//!   `WeCom` 写一样 —— 走 `sendersRegistry` 里握着的那条 aibot WebSocket。
//!   **aibot 没有出站 REST**，所以没有别的东西可以退回去。
//!
//! # 两种写，按这个顺序试
//!
//! 1. 问题到达时**开过气泡**的一轮，它的回答被**写进那个气泡**并封口 —— 这正是流式回复存在
//!    的全部意义，也正是"一次空完成也得走到这里"的原因：一个谁都不收尾的转圈**比**一句短回答
//!    更糟（`stream_store.rs`、M7-20 的打字指示）。
//! 2. 没有气泡可写的一轮（运行中途重启、流过了协议窗口、服务端拒掉的帧）走一条**普通消息**，
//!    由这一轮 task 自己的投递行寻址。
//!
//! # 副本拓扑（上游逐字，本仓的替身是 `relay.rs`）
//!
//! 上游 `EventChatDone` 走在进程内的事件总线上 ⇒ 发布事件的那个副本**不一定**是握着 bot WS
//! 租约的那个。有中继时，离租约产出的回复被转发给握着 socket 的副本，单副本约束不再适用于
//! 路由；没有中继时约束成立（见 `docs/60` §2.5 的 R-M7-1）。**任何模式下**，在**没有一个**副本
//! 握着活连接（全都在重连中）时产出的投递都会丢：那段残余窗口是**持久性**问题，
//! 中继刻意不解它。
//!
//! 气泡路径与中继**从不**争同一轮：气泡只在画它的那个副本上可写，而那正是握着 socket 的副本。
//! 它们**相遇**在接过一条中继回复的那个副本上（见 [`crate::wecom::relay`]）。
//!
//! # 与上游的形态差异（**逐条登记** `docs/32` §34）
//!
//! 1. **没有进程内事件总线 ⇒ 入口是显式调用**（与 M7-6 / M7-8 的同一先例）：本仓只落
//!    **投递那一段**（[`Outbound::handle_chat_done`] / [`Outbound::handle_inbox_new`]），
//!    "哪条任务完成了""哪条收件箱通知到了"由宿主（`apps/mc-server/src/channels.rs`）驱动。
//! 2. **一切库读都走端口**：上游 `outboundQueries` 是一个 `*db.Queries`；本仓的 adapter
//!    **不得**直接写/读 DB（`docs/60` §2.6 第 1 条）⇒ [`OutboundQueries`] 只声明要什么，
//!    PG 实现落在装配层（与 M7-15 的 `store.rs` 同手法）。窄的那一半（[`DeliveryLookup`]）
//!    同时服务 M7-20 的打字指示 —— 它寻址一轮的方式与回答**完全一样**。
//! 3. **活动 socket 也是端口**：`sendersRegistry` 是 M7-20 的 `senders.rs` ⇒ 本片只声明
//!    [`SenderLookup`] / [`LiveSender`] 并给 `WsSender` 一份实现（`impl` 在**本片**，
//!    `WsSender` 只读）。收尾帧要的那一半经 [`SenderLookup::stream_sender`]。
//! 4. **附件投递是接缝，不是本片的实现**：上游 `deliverAttachments` / `sendAttachments` /
//!    `readObject` 在 `outbound_media.go`（**M7-18**）⇒ 本片只落**准入与记账骨架**
//!    （[`AttachmentGates`]，上游那两个计数器本来就是 `Outbound` 的字段）并把逐文件的工作
//!    交给 [`AttachmentDelivery`]。
//! 5. **收件箱卡片的渲染是接缝**：上游 `buildInboxMarkdown` 在 `inbox_message.go`（**M7-19**）
//!    ⇒ 本片落投递路径 + [`InboxRenderer`] 端口；没有渲染器时这条推送**不投递**
//!    （失败关闭，见 D7）。
//! 6. **`classifySeal` / `fallbackBudget` 曾落在本文件，现已**收敛**到 `seal.rs`**：上游在
//!    `seal_outcome.go`（**M7-19**）落地，但那两个函数是**三个收尾器**共用的判据，而本片就有
//!    两个收尾器（本地回答 + 中继回答）—— 拆到 M7-19 会让本片自己写第三份读法，正是上游那份
//!    文件的头注释要消灭的东西。⇒ 本片先落、**M7-19 收敛**（交接项 H2，已于 `LUM-1784` 执行）：
//!    判据现在只有一份（[`crate::wecom::seal`]），本文件把两个名字**再导出**，好让下面三处调用点
//!    （`outbound/pipeline.rs` / `relay/relayed.rs` / 用例）一字未改。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 本文件**不碰凭据**：句子是 agent 写的、地址来自任务投递行、socket 由端口给。日志里只有
//! 会话 / 安装 / task 标识与原因标签；错误值（[`OutboundError`]）不带密钥、不带密文、不带正文。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::wecom::metrics::Metrics;
use crate::wecom::stream_store::{NothingToSay, RoundAddress, RoundTaker};
use crate::wecom::ws_sender::SenderError;

// =====================================================================
// 子模块（门 ⑩ 的 800 行硬限；逐条清单见 `docs/32` §34 的 D12）
// =====================================================================
//
// 上游那一个 `outbound.go` 在这里按**面**拆：库面端口 / 发送面端口 / 事件与预算 / 附件面 /
// 判决链。公开名字在下面**统一再导出**，于是 `crate::wecom::outbound::X` 与拆分前一样。

mod attachments;
mod events;
mod pipeline;
mod ports;
mod senders;

pub use attachments::{AttachmentAdmission, AttachmentDelivery, AttachmentGates, AttachmentTarget};
pub use events::{parse_uuid, uuid_string, ChatDone, DeliveryBudget, InboxPush, InboxRenderer};
pub use pipeline::{empty_address, TaskAddressOutcome};
pub use ports::{
    AgentTask, ChatSessionBinding, DeliveryLookup, InstallationRecord, MemberBinding,
    OutboundQueries, TaskDelivery,
};
pub(crate) use senders::spawn_detached;
pub use senders::{LiveSender, SenderLookup};

/// 上游 `handleEvent` 的预算：总线投递是**同步**的，一条卡住的 WS 写不许楔住发布点。
pub const EVENT_BUDGET: Duration = Duration::from_secs(10);

/// 上游 `handleInboxNew` 的预算（同一条理由，更短：一条收件箱推送不值得等）。
pub const INBOX_BUDGET: Duration = Duration::from_secs(5);

/// 上游 `fallbackSendTimeout`（在 `typing_indicator.go`）：气泡的收尾重试可以花掉调用方
/// 大部分预算，所以"退回普通消息"那一步要拿一份**气泡花不掉**的预算。
pub const FALLBACK_SEND_TIMEOUT: Duration = Duration::from_secs(6);

/// 上游 `maxPendingAttachmentDeliveries`：**已经查到有文件**的投递并发上限。
pub const MAX_PENDING_ATTACHMENT_DELIVERIES: usize = 32;

/// 上游 `maxAdmittedAttachmentDeliveries`：投递**任务本身**（含它的查表）的上限。
///
/// 上游逐字：故意是 pending 的两倍 —— 一个**带着**文件积压的队列应该先填满 pending 上限、
/// 在那条能说清丢了什么的路径上被削减；而触到 admitted 上限**不**蕴含 pending 满了
/// （还没查到文件的那些轮次也占着 admitted，且从不占 pending）。
pub const MAX_ADMITTED_ATTACHMENT_DELIVERIES: usize = 2 * MAX_PENDING_ATTACHMENT_DELIVERIES;

// =====================================================================
// 错误
// =====================================================================

/// 出站路径的错误（上游那一组哨兵错误 + `fmt.Errorf` 的形态）。
///
/// **不带正文、不带凭据**：`Send` 里包的是 M7-16 的错误词表（`Api` 只带 `errcode`/`cmd`）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutboundError {
    /// 上游 `errOutcomeRecorded`：调用方**不得**再分类这个错误 —— 产出它的那个分支已经通过
    /// [`Outbound::record_send`] 归档了结局，而 [`Outbound::handle_chat_done`] 会给
    /// `process_event` 返回的每一个错误记账。
    ///
    /// 上游的哨兵是**不透明**的（它只带"已经记过"这一个比特）；本仓把那个记录**带出来**，
    /// 纯粹是为了让判决（[`Verdict`]）对调用方诚实 —— 计数器仍是权威，语义一字未改。
    #[error("wecom: outcome already recorded")]
    OutcomeRecorded { recorded: Recorded },
    /// 上游 `errNothingToSay`：这一轮没什么可说的（本地复用 `stream_store` 的那一个）。
    ///
    /// 它说的是"**什么都没记**"：一条没有路由、没有气泡、也没有文件的完成从来不是一条消息，
    /// 所以它既不是丢弃也不是跳过。
    #[error(transparent)]
    NothingToSay(#[from] NothingToSay),
    /// 上游 `skipped` 的那些分支：这个 adapter **本来就不会**送这一轮，而且**已经记过**
    /// `record_outbound_skipped`（与 [`OutboundError::NothingToSay`] 的区别就在这里）。
    #[error("wecom: reply not owed to WeCom ({reason:?})")]
    Skipped { reason: super::outcome::SkipReason },
    /// 上游 `errNoLiveConnection`：本副本这个安装**没有活的 WebSocket**。
    /// 用哨兵而不是在每个调用点新造一个，是为了让 [`super::outcome::classify_drop`] 能说出
    /// 它的名字，而不是去匹配散文。
    #[error("wecom: connection not ready on this replica")]
    NoLiveConnection,
    /// 发送侧的失败。
    #[error(transparent)]
    Send(#[from] SenderError),
    /// 库/端口失败（上游 `fmt.Errorf("wecom: load agent task: %w")`）。
    #[error("wecom: {context}: {message}")]
    Lookup {
        context: &'static str,
        message: String,
    },
    /// 装配缺失（上游 `errors.New("wecom: sender registry not configured")`）。
    #[error("wecom: sender registry not configured")]
    SenderRegistryMissing,
}

impl OutboundError {
    /// 一次库读失败的构造。
    pub(crate) fn lookup(context: &'static str, message: impl Into<String>) -> Self {
        Self::Lookup {
            context,
            message: message.into(),
        }
    }
}

// =====================================================================
// 收尾的判决（上游 `seal_outcome.go`；**判据在 `seal.rs`**，见模块文档第 6 条）
// =====================================================================
//
// 交接 H2 的收敛：M7-17 先把这两个名字落在这里，因为那时只有它有收尾器；M7-19（`LUM-1784`）
// 把判据搬去它自己的写集 [`crate::wecom::seal`]，这里只再导出 —— 调用点因此一字未改。
pub use crate::wecom::seal::{classify_seal, fallback_budget, SealVerdict};

// =====================================================================
// 结论
// =====================================================================

/// 上游 `answerOutcome`：一次回答的"话去哪儿了"，供调用方还要做的两个决定用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerOutcome {
    /// 话到了哪儿（没有就是零值）。它也是**紧随其后的文件**要去的地址。
    pub addr: RoundAddress,
    /// 话**从本进程**到了用户那儿。附件路径要靠它知道文件是不是整条回答：
    /// 一个封好的气泡已经把字放到屏幕上了，所以它后面的文件失败是**文件**问题，
    /// 不是这个 adapter 欠下又丢了的一条回复。
    pub spoke: bool,
    /// 这一轮被**交给了**握着 socket 的副本。那个副本也发文件（`CarriesFiles`），
    /// 所以本副本不许发，否则用户会把每个附件收到两遍。
    pub routed: bool,
}

impl AnswerOutcome {
    /// 什么都没发生（事件与本 adapter 无关）。
    #[must_use]
    pub fn ignored() -> Self {
        Self {
            addr: RoundAddress {
                installation_id: None,
                chat_id: String::new(),
                chat_type: 0,
            },
            spoke: false,
            routed: false,
        }
    }
}

/// 一条 `chat:done` 处理完之后的**记账结论**（诊断与用例读它；**权威是计数器**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// 事件与本 adapter 无关（没有会话、或者一轮被 origin 门跳过并已记账）。
    Nothing,
    /// 一条回复到了用户手里（`outbound_delivered`）。
    Delivered,
    /// 结局未知（`outbound_unconfirmed` + label）。
    Unconfirmed(&'static str),
    /// 一条该到没到的回复（`outbound_dropped` + 原因）。
    Dropped(super::outcome::DropReason),
    /// 一条本来就不会送的完成（`outbound_skipped` + 原因）。
    Skipped(super::outcome::SkipReason),
}

/// `process_event` 的返回值：回答去了哪儿，再加上**已经在内层记过的那一笔账**（如果有）。
///
/// 上游把"记过账"藏在一个不透明的哨兵里（`errOutcomeRecorded`），于是只有计数器知道发生了什么；
/// 本仓把那一笔**带出来**，好让 [`Verdict`] 对调用方诚实。**权威仍然是计数器**：
/// `Processed` 只是同一件事的第二个读者。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Processed {
    pub answer: AnswerOutcome,
    pub recorded: Option<Recorded>,
}

impl Processed {
    /// 什么都没发生（事件与本 adapter 无关）。
    #[must_use]
    pub fn ignored() -> Self {
        Self {
            answer: AnswerOutcome::ignored(),
            recorded: None,
        }
    }

    /// 已经记过一笔账、而且没有回答可谈（丢弃 / 跳过）。
    #[must_use]
    pub fn recorded(recorded: Recorded) -> Self {
        Self {
            answer: AnswerOutcome::ignored(),
            recorded: Some(recorded),
        }
    }

    /// 一次有回答的投递，附上它记的那笔账。
    #[must_use]
    pub fn recorded_at(recorded: Recorded, answer: AnswerOutcome) -> Self {
        // `Recorded::Nothing` = **没记**：别把它伪装成一笔账。
        let recorded = match recorded {
            Recorded::Nothing => None,
            other => Some(other),
        };
        Self { answer, recorded }
    }
}

/// [`Outbound::handle_chat_done`] 的返回值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub recorded: Recorded,
    pub answer: AnswerOutcome,
}

impl Verdict {
    fn nothing() -> Self {
        Self {
            recorded: Recorded::Nothing,
            answer: AnswerOutcome::ignored(),
        }
    }

    fn of(recorded: Recorded, answer: AnswerOutcome) -> Self {
        Self { recorded, answer }
    }
}

// =====================================================================
// 订阅者
// =====================================================================

/// `WeCom` 的出站订阅者：把 agent 的聊天回答送回 `WeCom`，能用气泡就用气泡
/// （上游 `Outbound`）。
pub struct Outbound {
    q: Arc<dyn OutboundQueries>,
    senders: Option<Arc<dyn SenderLookup>>,
    streams: Option<Arc<crate::wecom::stream_store::StreamStore>>,
    tasks: Option<Arc<dyn crate::wecom::stream_store::RootResolver>>,
    relay: Option<Arc<dyn crate::wecom::relay::NoticeRouter>>,
    metrics: Option<&'static dyn Metrics>,
    attachments: Option<Arc<dyn AttachmentDelivery>>,
    /// 文件面**是否开着**（上游 `o.objects != nil`）：没有它，一条回答永远不带文件，
    /// 那次查表也就省了。
    attachment_storage: bool,
    inbox: Option<Arc<dyn InboxRenderer>>,
    gates: AttachmentGates,
}

impl fmt::Debug for Outbound {
    /// 手写：端口都是 trait 对象，只报**存在性**（与 `ChannelDeps` 同款）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Outbound")
            .field("senders", &self.senders.is_some())
            .field("streams", &self.streams.is_some())
            .field("relay", &self.relay.is_some())
            .field("attachments", &self.attachments.is_some())
            .field("inbox", &self.inbox.is_some())
            .finish_non_exhaustive()
    }
}

impl Outbound {
    /// 建一个出站订阅者。
    ///
    /// `senders` 与 `wecom.ChannelDeps` / [`super::replier::OutboundReplier`] 用的是**同一个**
    /// 进程级注册表：回复投递走绑定所属安装那把活着的 `wsSender`，所以一个监管器中途丢了租约的
    /// 会话会被路由出去或丢弃，**绝不**去开第二条连接。
    ///
    /// `streams` 与打字指示写的是**同一个**存储；`None` 关掉就地回复，于是每条回答都以新消息
    /// 发出。
    #[must_use]
    pub fn new(q: Arc<dyn OutboundQueries>, senders: Option<Arc<dyn SenderLookup>>) -> Self {
        Self {
            q,
            senders,
            streams: None,
            tasks: None,
            relay: None,
            metrics: None,
            attachments: None,
            attachment_storage: false,
            inbox: None,
            gates: AttachmentGates::default(),
        }
    }

    /// 接上气泡存储（上游 `NewOutbound` 的 `streams` 形参）。
    #[must_use]
    pub fn with_streams(mut self, streams: Arc<crate::wecom::stream_store::StreamStore>) -> Self {
        self.streams = Some(streams);
        self
    }

    /// 接上自动重试的血缘查询（上游 `Outbound.tasks`）。
    #[must_use]
    pub fn with_tasks(mut self, tasks: Arc<dyn crate::wecom::stream_store::RootResolver>) -> Self {
        self.tasks = Some(tasks);
        self
    }

    /// 接上附件投递（上游 `WithAttachments`：**唯一**一个改变"能送到什么"的选项）。
    #[must_use]
    pub fn with_attachments(mut self, attachments: Arc<dyn AttachmentDelivery>) -> Self {
        self.attachments = Some(attachments);
        self.attachment_storage = true;
        self
    }

    /// 接上收件箱卡片的渲染器（上游 `buildInboxMarkdown` 的落点，M7-19）。
    #[must_use]
    pub fn with_inbox_renderer(mut self, renderer: Arc<dyn InboxRenderer>) -> Self {
        self.inbox = Some(renderer);
        self
    }

    /// 换掉附件面的两个上限（用例把 `1` 塞进去，让削减路径不用真起 65 个任务）。
    #[must_use]
    pub fn with_attachment_gates(mut self, gates: AttachmentGates) -> Self {
        self.gates = gates;
        self
    }

    /// 装上中继（上游 `WithRelay` 在 `relay_outbound.go` 里；本仓的公开构造器在
    /// `relay.rs`，这里只给那个文件一个能在**本模块**里写私字段的入口）。
    #[must_use]
    pub fn with_outbound_metrics(mut self, metrics: &'static dyn Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// 装上中继（`relay.rs` 的 `Outbound::with_relay` 调它 —— 字段是私有的，
    /// 而 `wecom::relay` 不是 `wecom::outbound` 的后代 ⇒ 需要一个在本模块里的入口）。
    pub(crate) fn set_relay(&mut self, relay: Arc<dyn crate::wecom::relay::NoticeRouter>) {
        self.relay = Some(relay);
    }

    /// 汇（给 `outcome.rs` 的记账用）。
    #[must_use]
    pub fn metrics(&self) -> Option<&'static dyn Metrics> {
        self.metrics
    }

    /// 发送者注册表。
    pub(crate) fn senders(&self) -> Option<&Arc<dyn SenderLookup>> {
        self.senders.as_ref()
    }

    /// 附件面开着吗。
    pub(crate) fn has_attachment_storage(&self) -> bool {
        self.attachment_storage && self.attachments.is_some()
    }

    /// 上游 `rounds()`：把事件上的 task id 解回它所属那一轮的那个匹配器。
    #[must_use]
    pub fn rounds(&self) -> RoundTaker {
        match self.streams.as_ref() {
            Some(streams) => RoundTaker::new(Arc::clone(streams), self.tasks.clone()),
            None => RoundTaker::disabled(),
        }
    }

    // =================================================================
    // 入口
    // =================================================================

    /// 上游 `handleEvent` 的聊天那一半：处理一条 `chat:done`，**唯一**的记账点。
    ///
    /// 一处记一条没送到的回复，所以一次丢弃只被数**一次**、且永远带着原因：
    /// `process_event` 内部那些"自己没有错误就结束了一轮"的分支各自记账并返回 `Ok`，
    /// 而这里出现的一切都从错误上分类。
    pub async fn handle_chat_done(&self, event: &ChatDone) -> Verdict {
        let budget = DeliveryBudget::lasting(EVENT_BUDGET);
        match self.process_event(budget, event).await {
            Ok(processed) => Verdict::of(
                processed.recorded.unwrap_or(Recorded::Nothing),
                processed.answer,
            ),
            Err(OutboundError::OutcomeRecorded { recorded }) => {
                Verdict::of(recorded, AnswerOutcome::ignored())
            }
            Err(OutboundError::Skipped { reason }) => {
                Verdict::of(Recorded::Skipped(reason), AnswerOutcome::ignored())
            }
            Err(OutboundError::NothingToSay(_)) => Verdict::nothing(),
            Err(error) => {
                if let Some(reason) = super::outcome::unconfirmed_reason(&error) {
                    self.unconfirmed_for(
                        &event.chat_session_id,
                        &event.event_type,
                        reason,
                        Some(&error),
                    );
                    return Verdict::of(Recorded::Unconfirmed(reason), AnswerOutcome::ignored());
                }
                let reason = super::outcome::classify_drop(&error);
                self.dropped_for(
                    &event.chat_session_id,
                    &event.event_type,
                    reason,
                    Some(&error),
                );
                Verdict::of(Recorded::Dropped(reason), AnswerOutcome::ignored())
            }
        }
    }

    /// 一个只有汇、没有任何端口的 `Outbound`（`outcome.rs` 的记账用例用）。
    #[cfg(test)]
    pub(crate) fn outcome_test_double(metrics: &'static dyn Metrics) -> Self {
        Self {
            q: Arc::new(NoQueries),
            senders: None,
            streams: None,
            tasks: None,
            relay: None,
            metrics: Some(metrics),
            attachments: None,
            attachment_storage: false,
            inbox: None,
            gates: AttachmentGates::default(),
        }
    }
}

/// 一个什么都不答的查询端口（`outcome.rs` 的记账用例只需要一个非空的 `Arc`）。
#[cfg(test)]
pub(crate) struct NoQueries;

#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use mc_core::id::Id;

#[cfg(test)]
#[async_trait]
impl OutboundQueries for NoQueries {
    async fn get_task_delivery(&self, _task_id: Id) -> Result<Option<TaskDelivery>, String> {
        Ok(None)
    }
    async fn get_agent_task(&self, _task_id: Id) -> Result<Option<AgentTask>, String> {
        Ok(None)
    }
    async fn task_has_channel_ingested_messages(&self, _task_id: Id) -> Result<bool, String> {
        Ok(false)
    }
    async fn get_installation(
        &self,
        _installation_id: Id,
    ) -> Result<Option<InstallationRecord>, String> {
        Ok(None)
    }
    async fn find_binding_for_member(
        &self,
        _workspace_id: Id,
        _multica_user_id: Id,
    ) -> Result<Option<MemberBinding>, String> {
        Ok(None)
    }
    async fn workspace_slug(&self, _workspace_id: Id) -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
