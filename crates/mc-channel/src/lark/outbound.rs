//! lark 出站面：任务生命周期 → 文本 / markdown 卡 / 错误卡 / 卡片 patch
//! （上游 `internal/integrations/lark/outbound.go`，745 行）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - **上游定位**：`Patcher` —— 订阅总线上的 `task:failed` / `chat:done` / `task:cancelled`，
//!   再把结果投回 Lark。上游注释逐字的三条 scope 判据，本仓逐条保留：
//!   ① 只有 `chat_session` 带 lark 会话绑定的任务才出站（web 界面 / autopilot 的任务原样穿过）；
//!   ② 每条 `chat:done` 产出**一条**消息（没有 streaming、没有节流、没有卡状态行）；
//!   ③ 多副本安全**继承自入站 WS 租约**（同一时刻至多一个副本持有安装租约）。
//!
//! # 本仓的形态差异一：显式的生命周期入口，不是进程内事件总线（同 M7-6 / M7-8 先例）
//!
//! 上游 `Patcher::Register(bus)` 订阅事件总线。本仓**没有**进程内事件总线（M7-6 先例：
//! `telegram` 给出显式投递入口；M7-8 的 [`crate::dingtalk::outbound::OutboundDelivery`] 同款），
//! 所以这里把上游 `processEvent` 的**判决**原样搬进三个显式方法：
//! [`LarkOutboundDelivery::on_chat_done`] / [`LarkOutboundDelivery::on_task_failed`] /
//! [`LarkOutboundDelivery::on_task_cancelled`]。**接线点**（谁在任务终态时调它们）是交接项 ——
//! 上游由 `Register` 承担的那一步（见 `docs/32` §32.4 的 H1）。
//!
//! # 本仓的形态差异二：上游的用例类名 / `any` 载荷 → 强类型形参
//!
//! 上游从 `events.Event` 的 `any` 载荷里 `type switch` 出 `ChatDonePayload` / `map[string]any`
//! （`chatDoneContent` / `errorMessageFromPayload`）。本仓两个入口直接收 `&str` —— 上游那两个
//! 取值函数因此**不必存在**（载荷形状是**发布方**的事，不是出站面的事）。登记为 D3。
//!
//! # 从上游逐字搬来的四条语义（每条都有用例）
//!
//! 1. **`update_multi: true` 必须出现在每一张卡上**（含 `thinking` / `error`）—— 上游注释逐字：
//!    *Lark refuses to apply `PatchInteractiveCard` to a card whose config does not declare it a
//!    "shared, updatable" card*；缺了它，第二次之后的 patch 会在 Lark 侧**静默 no-op**，而本地
//!    状态行照样翻成 `streaming`。
//! 2. **回复目标三档**（`threadReplyTarget`）：话题里 ⇒ `reply_in_thread`；普通群 ⇒ 原生回复；
//!    p2p 或无触发消息 ⇒ 会话层发送。
//! 3. **话题会话没有触发消息 ⇒ 不发送**（`topicSendWithoutTrigger`）：Lark 进话题的**唯一**路径
//!    是回复话题里的某条消息，没有触发就只会落到父群 —— 那会静默撤销话题隔离，所以**拒发**
//!    （比 [`send_with_reply_fallback`] 的会话层回落**更窄**：那里话题本身不可用，投递胜过丢失）。
//! 4. **分类过的会话层回落**（`sendWithReplyFallback`）：**只有**当失败是"这条触发消息确实收不到
//!    回复"（撤回 / 不可见 / 阅后即焚 / 话题已删 / 话题被关 / 聚合消息 ——
//!    [`crate::lark::client::THREAD_REPLY_UNSUPPORTED_CODES`]）才回落一次；传输失败 / 5xx / 超时 /
//!    限流 / "服务器可能已经收到"一律**不**回落（盲目重投会重复回复或把话题内的回复泄进主群）。
//! 5. **先选 wire 再挂提及**（`sendChatReply`）：`containsMarkdown` 判的是 agent 自己的正文，
//!    **之后**才挂 `@`；否则一个 `@_user_1` 占位会把纯散文翻到卡片路径上。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 明文 `app_secret` 只在一次调用期间存在于 [`InstallationCredentials`] 里（它自带脱敏
//! `Debug`）；卡片 JSON 与正文**可以**进日志吗 —— 不可以：`tracing::*` 只插值
//! `installation_id` / `task_id` / `chat_session_id` / `op` / **错误类别**，从不插值正文、
//! 卡片体或凭据。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::id::Id;
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;

use super::client::ApiClient;
use super::feishu_channel::credentials::Decrypter;
use super::params::{
    InstallationCredentials, SendCardParams, SendMarkdownCardParams, SendTextParams,
};
use super::resolvers::LarkInstallation;
use super::store::{CardStatus, ChatSessionBinding, NewOutboundCard, OutboundCardMessage};
use crate::engine::resolvers::{EngineError, EngineResult};

mod cards;
mod reply;

pub use cards::{
    render_notice_card, CardKind, CardRender, DefaultRenderer, LarkCardPatcher, RenderError,
    RenderInput, Renderer, DEFAULT_CARD_HEADER,
};
pub use reply::{
    binding_config_json, credentials_error, delivery_config_with_sender, installation_credentials,
    mention_open_id, outbound_chat_id, send_with_reply_fallback, thread_reply_target,
    topic_send_without_trigger, FallbackError,
};

// =====================================================================
// 数据端口（上游 `PatcherQueries`）
// =====================================================================

/// 出站面要的**全部**读侧接缝（上游 `PatcherQueries`）。
///
/// 上游把它声明在 `outbound.go` 里、由 `*db.Queries` 满足；本仓照落（生产者是
/// [`super::channel_store::LarkChannelStore`]，用例注入替身）。
///
/// # 与上游接口的逐条对照
///
/// | 上游方法 | 本 trait | 备注 |
/// | --- | --- | --- |
/// | `GetChannelTaskDelivery` | [`PatcherQueries::task_delivery`] | 泛化 `channel_task_delivery` |
/// | `TaskHasChannelIngestedMessages` + `GetAgentTask` 的 `chat_input_task_id` | [`PatcherQueries::task_origin`] | **可注入**，见 D2 |
/// | `GetAgent` | [`PatcherQueries::agent_name`] | 只要名字（卡片头） |
/// | `GetLarkInstallation` | [`PatcherQueries::installation`] | 遗留 `lark_installation` |
/// | `GetLarkChatSessionBindingBySession` | [`PatcherQueries::binding_for_task`] | 见下 |
/// | `GetLarkOutboundCardByTask` / `Create…` / `Update…` | [`PatcherQueries::card_by_task`] / [`PatcherQueries::upsert_card`] / [`PatcherQueries::mark_card_status`] | 泛化 `channel_outbound_card_message` |
/// | `GetChatSession` | **不落** | 它在上游**没有任何调用点**（死接口成员，见 D1） |
///
/// `binding_for_task` 是上游 `processEvent` 里"读投递行 → 就地造 `ChatSessionBinding`"的
/// **一次读**（本仓只有一个往返，不先读绑定表再读投递表）。
#[async_trait]
pub trait PatcherQueries: Send + Sync {
    /// 这条任务的渠道投递路由；没有行 = 直接任务（**失败关闭**，不出站）。
    ///
    /// # Errors
    ///
    /// 链路失败（调用方只记 warn，不上抛成"投递失败"）。
    async fn task_delivery(&self, task_id: Id) -> EngineResult<Option<ChannelTaskDeliveryRow>>;

    /// 这条任务的**输入出处**两半（上游 `TaskInputIsChannelIngested` 的入参）。
    ///
    /// 判据本身是 engine 的 [`crate::engine::task_input_is_channel_ingested`]（M7-2 的纯函数）；
    /// 这里给的是它的两个输入 —— 见 [`TaskOrigin`] 与 D2。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn task_origin(&self, task_id: Id) -> EngineResult<TaskOrigin>;

    /// agent 的显示名（卡片头）；查不到 ⇒ `None`（回落 [`DEFAULT_CARD_HEADER`]）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn agent_name(&self, agent_id: Id) -> EngineResult<Option<String>>;

    /// 安装行（遗留 `lark_installation`；见 [`super::store`] 的两族对照表）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn installation(&self, installation_id: Id) -> EngineResult<Option<LarkInstallation>>;

    /// 这条任务的会话绑定 + 冻结的回复目标（上游 `processEvent` 的组装结果）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn binding_for_task(&self, task_id: Id) -> EngineResult<Option<ChatSessionBinding>>;

    /// 这条任务已落的卡片行（没有 ⇒ 还没开卡）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn card_by_task(&self, task_id: Id) -> EngineResult<Option<OutboundCardMessage>>;

    /// 建 / 覆盖一张卡片行（上游 `CreateLarkOutboundCardMessage`；`ON CONFLICT (task_id)`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn upsert_card(&self, new: &NewOutboundCard) -> EngineResult<OutboundCardMessage>;

    /// 翻卡片行的状态（上游 `UpdateLarkOutboundCardStatus`；返回是否改动了行）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn mark_card_status(&self, card_id: Id, status: CardStatus) -> EngineResult<bool>;

    /// 推进会话绑定上的"最近一条触发"游标（上游 `UpdateLarkChatSessionBindingReplyTarget`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn update_reply_target(
        &self,
        session_id: Id,
        message_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> EngineResult<u64>;
}

// =====================================================================
// 输入出处（上游 `TaskInputIsChannelIngested` 的两半）
// =====================================================================

/// 一条任务的输入出处（上游 `TaskInputIsChannelIngested(ctx, queries, task)` 的两个输入）。
///
/// 拆成结构而不是一个 `bool`，是为了让"哪一半拿不到"**看得见**（见 D2）：
/// `chat_input_task_id` 来自任务行、`batch_has_channel_ingested_messages` 来自那条输入批次上
/// 不可变的 `channel_ingested` 戳。本仓 `mc-repos` 的**任务面**今天两个查询都没有
/// （`engine::commands::task_input_is_channel_ingested` 的文档把同一条登记成缺口）⇒ 生产适配器
/// 只能给出 `None` + "有投递行"，于是 [`TaskOrigin::deliverable`] 恒为真。这**不是**"假实现"，
/// 而是**上游 MUL-4988 之前的行为**被显式写出来 —— 换一个能给出两半的适配器即可收紧。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskOrigin {
    /// 任务拥有的输入批次（上游 `task.chat_input_task_id`）；`None` = 密封之前的渠道任务。
    pub chat_input_task_id: Option<Id>,
    /// 那条批次上有没有 `channel_ingested` 的消息。
    pub batch_has_channel_ingested_messages: bool,
}

impl TaskOrigin {
    /// 一个只给"批次戳"的出处（`chat_input_task_id` 未知）。
    #[must_use]
    pub fn from_batch_flag(batch_has_channel_ingested_messages: bool) -> Self {
        Self {
            chat_input_task_id: None,
            batch_has_channel_ingested_messages,
        }
    }

    /// 判决（**照抄** engine 的纯函数，一处实现）。
    #[must_use]
    pub fn deliverable(self) -> bool {
        crate::engine::task_input_is_channel_ingested(
            self.chat_input_task_id,
            self.batch_has_channel_ingested_messages,
        )
    }
}

// =====================================================================
// 判决词表（上游 `processEvent` 的收尾）
// =====================================================================

/// 这次出站**什么都没发**的原因（上游每个 `return nil` 都对应一格）。
///
/// 单独列出来是为了让"为什么这条任务没有回复"可查 —— 上游只有一行 `return nil`，
/// 值班时只能靠日志猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// 投递行不存在 ⇒ 直接任务（上游：*Direct Multica task or violated snapshot invariant —
    /// fail closed*）。
    NoDeliveryRow,
    /// 投递行的渠道判别式不是 feishu。
    NotFeishu,
    /// 输入批次没有渠道入站戳 ⇒ 这条回复属于 Multica，不属于 Lark（MUL-4988）。
    NotChannelIngested,
    /// 安装行没了（会话删除 / 运行时拆除）。
    InstallationMissing,
    /// 安装已撤销（触发与事件之间被撤）。
    InstallationRevoked,
    /// 话题会话但没有触发消息（[`topic_send_without_trigger`]）。
    TopicWithoutTrigger,
    /// 正文空（上游逐字：*we'd rather show nothing than "Done."*）。
    EmptyContent,
    /// 卡片行已经收口（终态之后不再 patch）。
    CardSettled,
    /// 这个任务还没有卡片行（没有开过卡）。
    NoCard,
}

impl SkipReason {
    /// 稳定字串（日志 / 看板）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoDeliveryRow => "no_delivery_row",
            Self::NotFeishu => "not_feishu",
            Self::NotChannelIngested => "not_channel_ingested",
            Self::InstallationMissing => "installation_missing",
            Self::InstallationRevoked => "installation_revoked",
            Self::TopicWithoutTrigger => "topic_without_trigger",
            Self::EmptyContent => "empty_content",
            Self::CardSettled => "card_settled",
            Self::NoCard => "no_card",
        }
    }
}

/// 一次出站的结论（可断言、可记账）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// 没发（原因见 [`SkipReason`]）。
    Skipped(SkipReason),
    /// 发了一条纯文本（无 markdown 语法的那条路）。
    Text {
        /// Lark 侧消息 id。
        message_id: String,
        /// 正文前面是否挂了原生 `@`。
        mentioned: bool,
    },
    /// 发了一张 markdown 卡（正文含 markdown 时那条路）。
    MarkdownCard {
        /// Lark 侧消息 id。
        message_id: String,
        /// 正文前面是否挂了原生 `@`。
        mentioned: bool,
    },
    /// 发了一张错误卡。
    ErrorCard {
        /// Lark 侧消息 id。
        message_id: String,
    },
    /// 原地 patch 了一张卡片（[`LarkCardPatcher`] 的路径）。
    Patched {
        /// 卡片行主键。
        card_id: Id,
        /// patch 之后的状态。
        status: CardStatus,
    },
    /// 撤掉了某会话的"处理中"指示（[`LarkOutboundDelivery::on_task_cancelled`] 的路径）。
    TypingCleared {
        /// 被清的会话。
        session_id: Id,
    },
}

/// 渠道判别式的存储口径（Lark 是 `feishu`，**不是** `lark`；见 `docs/32` §10 的 R-M7-10）。
pub const CHANNEL_TYPE_FEISHU: &str = "feishu";

/// 安装状态里"仍可承载消息"的那个字面量（上游 `InstallationActive`）。
pub const INSTALLATION_ACTIVE: &str = "active";

// =====================================================================
// 出站投递（上游 `Patcher`）
// =====================================================================

/// 任务生命周期 → Lark 消息（上游 `Patcher` 的三个订阅）。
pub struct LarkOutboundDelivery {
    queries: Arc<dyn PatcherQueries>,
    client: Arc<dyn ApiClient>,
    decrypt: Option<Decrypter>,
    renderer: Arc<dyn Renderer>,
    typing: Option<Arc<super::typing::TypingIndicatorManager>>,
}

impl fmt::Debug for LarkOutboundDelivery {
    /// 手写：端口只报存在性，解密器自带脱敏 `Debug`，**没有任何凭据字段**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LarkOutboundDelivery")
            .field("client", &"<dyn ApiClient>")
            .field("has_decrypter", &self.decrypt.is_some())
            .field("renderer", &"<dyn Renderer>")
            .field("has_typing", &self.typing.is_some())
            .finish_non_exhaustive()
    }
}

impl LarkOutboundDelivery {
    /// 装配。
    #[must_use]
    pub fn new(queries: Arc<dyn PatcherQueries>, client: Arc<dyn ApiClient>) -> Self {
        Self {
            queries,
            client,
            decrypt: None,
            renderer: Arc::new(DefaultRenderer),
            typing: None,
        }
    }

    /// 接上解密器（`None` ⇒ 有安装的路径全部响"解不开"）。
    #[must_use]
    pub fn with_decrypter(mut self, decrypt: Decrypter) -> Self {
        self.decrypt = Some(decrypt);
        self
    }

    /// 换卡片模板。
    #[must_use]
    pub fn with_renderer(mut self, renderer: Arc<dyn Renderer>) -> Self {
        self.renderer = renderer;
        self
    }

    /// 接上打字指示器 ⇒ 每条回复发出**之前**先撤掉"处理中"表情。
    ///
    /// 上游 `SetTypingIndicatorManager` 逐字：*so that replies clear the "processing" reaction
    /// before they are sent*。`None` 关掉这一步。
    #[must_use]
    pub fn with_typing(mut self, typing: Arc<super::typing::TypingIndicatorManager>) -> Self {
        self.typing = Some(typing);
        self
    }

    /// `chat:done`：把 agent 的回复投回 Lark（上游 `EventChatDone` 那一支）。
    ///
    /// # Errors
    ///
    /// 链路失败 / 凭据解不开 ⇒ `Err`（调用方只记 warn：回复投递不是入站流水线的一部分，
    /// **失败不得回滚**已经落库的命令状态与消息）。
    pub async fn on_chat_done(&self, task_id: Id, content: &str) -> EngineResult<DeliveryOutcome> {
        self.deliver(task_id, DeliverPayload::ChatDone(content))
            .await
    }

    /// `task:failed`：发一张短的错误卡（上游 `EventTaskFailed` 那一支）。
    ///
    /// 上游注释逐字：失败**留在卡片形态**（而不是像成功路径那样退回纯文本），因为视觉上的
    /// 区分对用户真的有用 —— 带红色头的卡片比普通气泡难忽视得多，而失败足够罕见，卡片外壳不吵。
    ///
    /// # Errors
    ///
    /// 同 [`LarkOutboundDelivery::on_chat_done`]。
    pub async fn on_task_failed(
        &self,
        task_id: Id,
        error_message: &str,
    ) -> EngineResult<DeliveryOutcome> {
        self.deliver(task_id, DeliverPayload::Failed(error_message))
            .await
    }

    /// `task:cancelled`：**只**撤打字指示，什么都不发（上游 `EventTaskCancelled` 那一支）。
    ///
    /// 上游注释逐字：取消的行没有答案要放，所以对用户欠的只有把"处理中"撤掉；这一步跑在
    /// 后面**每一个**查库之前，因为那几个查询对一个**仍有表情挂在屏幕上**的运行都可能答"不"：
    /// 会话删除的取消广播到达时绑定行已经没了；而"这次回答该不该落在 Lark"的出处分类对
    /// 一个因输入批次为空而被取消的任务会答"没有渠道入站消息"。
    ///
    /// # Errors
    ///
    /// 撤表情走 best-effort（[`super::typing::TypingIndicatorManager::clear`] 自己吞错误）
    /// ⇒ 本方法只在会话 id 为空时返回 `Skipped`，**从不**因表情失败而报错。
    pub async fn on_task_cancelled(&self, session_id: Option<Id>) -> EngineResult<DeliveryOutcome> {
        let Some(session_id) = session_id else {
            return Ok(DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow));
        };
        if let Some(typing) = &self.typing {
            typing.clear_now(session_id).await;
        }
        Ok(DeliveryOutcome::TypingCleared { session_id })
    }

    /// 上游 `processEvent` 的主体（两个订阅共用同一条前置链）。
    async fn deliver(
        &self,
        task_id: Id,
        payload: DeliverPayload<'_>,
    ) -> EngineResult<DeliveryOutcome> {
        let binding = match self.binding_for_task(task_id).await? {
            BindingVerdict::Ready(binding) => binding,
            BindingVerdict::Skip(reason) => return Ok(DeliveryOutcome::Skipped(reason)),
        };
        if topic_send_without_trigger(&binding) {
            tracing::warn!(
                chat_session_id = binding
                    .chat_session_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                channel_chat_id = binding.channel_chat_id,
                "lark: no trigger for a topic-isolated session; skipping the reply rather than \
                 posting it to the parent group"
            );
            return Ok(DeliveryOutcome::Skipped(SkipReason::TopicWithoutTrigger));
        }
        let Some(installation) = self.queries.installation(binding.installation_id).await? else {
            return Ok(DeliveryOutcome::Skipped(SkipReason::InstallationMissing));
        };
        if installation.status != INSTALLATION_ACTIVE {
            // 触发与事件之间被撤；没有可 patch 的东西。
            return Ok(DeliveryOutcome::Skipped(SkipReason::InstallationRevoked));
        }
        let credentials = installation_credentials(&installation, self.decrypt.as_ref())?;
        let agent_name = self
            .queries
            .agent_name(installation.agent_id)
            .await?
            .unwrap_or_default();
        // 回复可见**之前**撤掉"处理中"表情，让用户看到一个干净的过渡（best-effort）。
        if let Some(typing) = &self.typing {
            if let Some(session_id) = binding.chat_session_id {
                typing.clear_now(session_id).await;
            }
        }
        match payload {
            DeliverPayload::ChatDone(content) => {
                self.send_chat_reply(&credentials, &binding, content).await
            }
            DeliverPayload::Failed(message) => {
                self.send_error_card(&credentials, &binding, task_id, &agent_name, message)
                    .await
            }
        }
    }

    /// 读投递行并走**出处分类**（上游 `processEvent` 的前半）。
    ///
    /// 每一步"不归 Lark"的判决都给出**具体**原因（上游只有一行 `return nil`，值班时只能靠日志猜）。
    async fn binding_for_task(&self, task_id: Id) -> EngineResult<BindingVerdict> {
        let Some(delivery) = self.queries.task_delivery(task_id).await? else {
            // 直接 Multica 任务，或快照不变式被破坏 ⇒ 失败关闭。
            return Ok(BindingVerdict::Skip(SkipReason::NoDeliveryRow));
        };
        if delivery.channel_type != CHANNEL_TYPE_FEISHU {
            return Ok(BindingVerdict::Skip(SkipReason::NotFeishu));
        }
        // 只对已绑定的会话走到这里 ⇒ 在花任何发送预算之前先按**不可变的**渠道出处分类。
        // Web / 移动端的直接聊天任务可以复用一条源于 Lark 的会话，但它们的回复只属于 Multica。
        let origin = self.queries.task_origin(task_id).await?;
        if !origin.deliverable() {
            tracing::debug!(
                task_id = %task_id,
                "lark patcher: task input is not channel-ingested; skipping the reply"
            );
            return Ok(BindingVerdict::Skip(SkipReason::NotChannelIngested));
        }
        let Some(binding) = self.queries.binding_for_task(task_id).await? else {
            return Ok(BindingVerdict::Skip(SkipReason::NoDeliveryRow));
        };
        Ok(BindingVerdict::Ready(binding))
    }

    /// `ChatDonePayload.Content` → 一条 Lark 消息（上游 `sendChatReply`）。
    ///
    /// wire 形态按正文**是否含 markdown 语法**逐条选：纯散文 ⇒ `msg_type=text`；含 markdown
    /// ⇒ schema 2.0 互动卡里的 `tag: "markdown"` 块。空正文**静默丢弃** —— 宁可什么都不显示，
    /// 也不要 "Done."（上游逐字：*the prior card fallback that confused Bohan in the live dev
    /// env*）。
    ///
    /// `@` 前缀在**选完形态之后**才挂（模块文档第 5 条）。
    async fn send_chat_reply(
        &self,
        credentials: &InstallationCredentials,
        binding: &ChatSessionBinding,
        content: &str,
    ) -> EngineResult<DeliveryOutcome> {
        if content.is_empty() {
            return Ok(DeliveryOutcome::Skipped(SkipReason::EmptyContent));
        }
        let mention = mention_open_id(binding);
        let mentioned = !mention.is_empty();
        let chat_id = outbound_chat_id(binding);
        let target = thread_reply_target(binding);
        if super::content_flatten::contains_markdown(content) {
            let markdown = super::content_flatten::prepend_markdown_mention(&mention, content);
            let params = SendMarkdownCardParams {
                credentials: credentials.clone(),
                chat_id,
                markdown,
                summary: String::new(),
                reply_target: target.clone(),
            };
            let client = Arc::clone(&self.client);
            let message_id =
                send_with_reply_fallback("send markdown card", target, |reply_target| {
                    let client = Arc::clone(&client);
                    let mut params = params.clone();
                    params.reply_target = reply_target;
                    async move { client.send_markdown_card(params).await }
                })
                .await
                .map_err(|error| fallback_error(&error))?;
            return Ok(DeliveryOutcome::MarkdownCard {
                message_id,
                mentioned,
            });
        }
        let text = super::content_flatten::prepend_text_mention(&mention, content);
        let params = SendTextParams {
            credentials: credentials.clone(),
            chat_id,
            text,
            reply_target: target.clone(),
        };
        let client = Arc::clone(&self.client);
        let message_id = send_with_reply_fallback("send text message", target, |reply_target| {
            let client = Arc::clone(&client);
            let mut params = params.clone();
            params.reply_target = reply_target;
            async move { client.send_text_message(params).await }
        })
        .await
        .map_err(|error| fallback_error(&error))?;
        Ok(DeliveryOutcome::Text {
            message_id,
            mentioned,
        })
    }

    /// 失败卡（上游 `Patcher::fail`）。
    ///
    /// 一次性发送（不 patch、不落卡行）：如果同一个任务失败两次，就发第二张卡 —— 那没关系，
    /// 失败通常是**单个**终态事件。
    async fn send_error_card(
        &self,
        credentials: &InstallationCredentials,
        binding: &ChatSessionBinding,
        task_id: Id,
        agent_name: &str,
        error_message: &str,
    ) -> EngineResult<DeliveryOutcome> {
        let render = self
            .renderer
            .render(&RenderInput {
                kind: CardKind::Error,
                agent_name: agent_name.to_string(),
                task_id: Some(task_id),
                error_message: error_message.to_string(),
                ..RenderInput::default()
            })
            .map_err(|error| crate::engine::resolvers::EngineError::infra(error.to_string()))?;
        let params = SendCardParams {
            credentials: credentials.clone(),
            chat_id: outbound_chat_id(binding),
            card_json: render.json,
            reply_target: thread_reply_target(binding),
        };
        let client = Arc::clone(&self.client);
        let target = params.reply_target.clone();
        let message_id = send_with_reply_fallback("send error card", target, |reply_target| {
            let client = Arc::clone(&client);
            let mut params = params.clone();
            params.reply_target = reply_target;
            async move { client.send_interactive_card(params).await }
        })
        .await
        .map_err(|error| fallback_error(&error))?;
        Ok(DeliveryOutcome::ErrorCard { message_id })
    }
}

/// `binding_for_task` 的判决（"能用"或"具体为什么不发"）。
///
/// 上游把这两种收尾压成同一个 `return nil`；本仓分开，于是"为什么这条任务没有回复"可查。
#[derive(Debug, Clone)]
enum BindingVerdict {
    /// 已绑定，可以发。
    Ready(ChatSessionBinding),
    /// 不归 Lark（原因见 [`SkipReason`]）。
    Skip(SkipReason),
}

/// 两个订阅的载荷（上游 `events.Event.Payload` 的 `any` 的**强类型**等价物）。
#[derive(Debug, Clone, Copy)]
enum DeliverPayload<'a> {
    /// `chat:done` 的正文。
    ChatDone(&'a str),
    /// `task:failed` 的错误文案。
    Failed(&'a str),
}

/// [`FallbackError`] → engine 的错误（**只带类别**，不带平台 `msg`）。
fn fallback_error(error: &FallbackError) -> EngineError {
    EngineError::infra(format!(
        "lark: {}: {} failed: {}",
        error.op(),
        if error.fell_back() {
            "chat-level fallback"
        } else {
            "send"
        },
        error.original().class().as_str()
    ))
}

#[cfg(test)]
mod tests;
