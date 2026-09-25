//! lark 的**数据层桥**：把上游 `ChannelStore` 的方法名接到本仓真实存在的仓储上
//! （上游 `internal/integrations/lark/channel_store.go`，429 行）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - **上游定位**：上游注释逐字：*`ChannelStore` is the production data layer for the Feishu
//!   integration after MUL-3515 generalized `lark_*` into `channel_*`. It embeds `*db.Queries`
//!   (so every generic query — `chat_session`, `chat_message`, `member`, `workspace`, `agent` —
//!   is available unchanged) and adds the feishu-specific store methods, each backed by a
//!   `channel_*` query and translating at the JSONB-config boundary (`store.go`).*
//!
//! # 本文件与上游最要紧的一处不同：**行落在哪一族表**
//!
//! 上游那一侧运行时只走泛化 `channel_*`（`channel_type='feishu'`），`lark_*` 是退役面。
//! 本仓的合并树（`mc-repos/src/channel/installation.rs` 的硬约束，M7-1 **W**；M7-12 的
//! [`super::resolvers`] 也建在它上面）把这件事定成：**lark 的行走遗留 `lark_*`，不得并进
//! `channel_*`**。`super::store` 的模块文档是这条对照表的**唯一**权威来源；本文件逐方法执行它：
//!
//! | 上游方法 | 本仓落在 | 端口 |
//! | --- | --- | --- |
//! | `GetLarkInstallation` ○ `ListActive…` ○ `GetLarkInstallationByAppID` | 遗留 `lark_installation`（`ChannelInstallationRepo::{get_lark,find_lark_by_app_id,list_active_by_kind}`）| [`InstallationLookup`] |
//! | `GetLarkChatSessionBinding` ○ `…BySession` ○ `UpdateLarkChatSessionBindingReplyTarget` | 遗留 `lark_chat_session_binding`（`LarkChatSessionBindingRepo`）| [`SessionBindingStore`] |
//! | `GetChannelTaskDelivery` | 泛化 `channel_task_delivery`（`ChannelDeliveryRepo`）| [`TaskDeliveryStore`] |
//! | `GetLarkOutboundCardByTask` ○ `CreateLarkOutboundCardMessage` ○ `UpdateLarkOutboundCardStatus` | 泛化 `channel_outbound_card_message`（`ChannelOutboundRepo`）| [`CardStore`] |
//! | `GetLarkUserBindingByOpenID` | 遗留 `lark_user_binding`（`ChannelBindingRepo::find_lark_user_binding`）| [`UserBindingLookup`] |
//! | `RecordLarkInboundDrop` | 遗留 `lark_inbound_audit`（[`super::audit::LarkAuditLogger`]）| —— |
//!
//! 三行说明为什么**必须**跨族：① 会话绑定**两族都有** —— 入站侧写的是泛化那一行
//! （M7-12 的 `ChannelSessionBinder`），而 D11 把遗留那一行的**补写**责任交给本片 ⇒ 本文件的
//! 桥同时读泛化、写遗留；② 卡片行**只有**泛化那一族（没有 `LarkOutboundCardRepo`）；③ 安装行
//! **只有**遗留那一族可用（M7-1 的硬约束）。**"两套并存"在这里是可执行的代码，不是口号。**
//!
//! # 本片不落的上游方法（逐条登记，`docs/32` §32.2 的 D 项）
//!
//! 上游 `ChannelStore` 还有一批方法是**别的片**的写集与责任面 ⇒ 本文件不重复实现，只列出口：
//!
//! - `AcquireLarkWSLease` / `ReleaseLarkWSLease` ⇒ **M7-11** 的 `ws_connector` + `engine::lease`；
//! - `ClaimLarkInboundDedup` / `Mark…` / `Release…` ⇒ **M7-12** 的 `resolvers` 已接
//!   `ChannelDeduper::lark`；
//! - `CreateLarkBindingToken` / `ConsumeLarkBindingToken` / `UpsertLarkInstallation` /
//!   `SetLarkInstallationStatus` / `ReclaimDead…` ⇒ **M7-14** 的安装与绑定面；
//! - `BackfillLarkInstallationRegionToLark` / `SetLarkInstallationBotUnionID` ⇒ 运维回填
//!   （M7-14 的 H5 判据已落地）。
//!
//! # 依赖方向
//!
//! 本文件依赖 `mc-repos` 的具体仓储（它们都 `Clone` 且只裹一个 `Db`），但**每个**都藏在一个
//! 小 trait 后面：用例注入内存替身（本 crate 没有 `sqlx` 依赖），生产用 `from_repos` 装配。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::binding::ChannelBindingRepo;
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;
use mc_repos::channel::installation::ChannelInstallationRepo;
use mc_repos::channel::outbound::ChannelOutboundCardMessageRow;
use mc_repos::channel::outbound::ChannelOutboundRepo;
use mc_repos::channel::session::{
    ChannelChatSessionBindingRow, ChannelChatSessionRepo, LarkChatSessionBindingRepo,
    LarkChatSessionBindingRow,
};
use mc_repos::RepoError;

use super::outbound::{PatcherQueries, TaskOrigin, CHANNEL_TYPE_FEISHU};
use super::replier::OutcomeReplierQueries;
use super::resolvers::{LarkInstallation, TYPE_LARK};
use super::store::{
    CardStatus, ChatSessionBinding, NewOutboundCard, OutboundCardMessage, UserBinding,
};
use super::types::ChatType;
use super::typing::TypingIndicatorQueries;
use crate::engine::resolvers::{EngineError, EngineResult};

// =====================================================================
// 表名常量（**唯一**权威是 `super::store` 的对照表；这里给诊断与用例一个可断言的口）
// =====================================================================

/// 遗留安装表（本片读安装行走它）。
pub const LEGACY_INSTALLATION_TABLE: &str = "lark_installation";
/// 遗留会话绑定表（本片**补写**它，见 D11）。
pub const LEGACY_SESSION_BINDING_TABLE: &str = "lark_chat_session_binding";
/// 泛化会话绑定表（**入站写的就是它**；本片读它来对照）。
pub const GENERIC_SESSION_BINDING_TABLE: &str = "channel_chat_session_binding";
/// 泛化任务投递表。
pub const TASK_DELIVERY_TABLE: &str = "channel_task_delivery";
/// 泛化出站卡片表（lark 的卡片行**只有**这一族）。
pub const OUTBOUND_CARD_TABLE: &str = "channel_outbound_card_message";
/// 遗留成员绑定表。
pub const LEGACY_USER_BINDING_TABLE: &str = "lark_user_binding";

/// 与 lark **无关**的泛化表族（同一批表的另外三格）。
///
/// 它们是 slack / wecom / telegram 与 M7-12 的入站审计面走的表；本片**不**碰它们 —— 这条常量
/// 存在的意义就是让"两套并存、逐行按族落"这件事在断言里可枚举。
pub const NOT_USED_BY_LARK: [&str; 3] = [
    "channel_installation",
    "channel_user_binding",
    "channel_inbound_audit",
];

mod repos;

pub use repos::{
    RepoCards, RepoInstallations, RepoSessionBindings, RepoTaskDelivery, RepoUserBindings,
};

// =====================================================================
// 端口（薄：每个只包一条"上游那一族表"）
// =====================================================================

/// 安装行（**遗留** `lark_installation`）。
#[async_trait]
pub trait InstallationLookup: Send + Sync {
    /// 按 id 取投影；没有 ⇒ `Ok(None)`。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn get(&self, id: Id) -> Result<Option<LarkInstallation>, RepoError>;
}

/// 会话绑定（**遗留** `lark_chat_session_binding`，外加泛化表的**读**）。
///
/// 读泛化那一格是必要的：入站把绑定写在泛化表上（M7-12 的通用 binder），而遗留表是本片要
/// **补写**的镜像面（D11）。两族都读，"哪一族是本行"这件事在方法名上写清楚。
#[async_trait]
pub trait SessionBindingStore: Send + Sync {
    /// 遗留行：按 `chat_session` 读（上游 `GetLarkChatSessionBindingBySession`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn legacy_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<LarkChatSessionBindingRow>, RepoError>;

    /// 遗留行：按 `(installation, lark_chat_id)` 读（上游 `GetLarkChatSessionBinding`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn legacy_by_chat(
        &self,
        installation_id: Id,
        chat_id: &str,
    ) -> Result<Option<LarkChatSessionBindingRow>, RepoError>;

    /// 遗留行：**补写**（D11 交接的那一半）。
    ///
    /// # Errors
    ///
    /// 链路失败（外键不存在 / 唯一冲突都落这里）。
    async fn insert_legacy(
        &self,
        session_id: Id,
        installation_id: Id,
        chat_id: &str,
        chat_type: ChatType,
    ) -> Result<LarkChatSessionBindingRow, RepoError>;

    /// 遗留行：推进"最近一条触发"游标（上游 `UpdateLarkChatSessionBindingReplyTarget`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn update_legacy_reply_target(
        &self,
        session_id: Id,
        message_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<u64, RepoError>;

    /// 泛化行：入站写的**就是**这一行（诊断 / 补 `chat_session_id` 用）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn generic_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<ChannelChatSessionBindingRow>, RepoError>;
}

/// 任务投递（**泛化** `channel_task_delivery`）+ 出处的**查询那一半**。
#[async_trait]
pub trait TaskDeliveryStore: Send + Sync {
    /// 这条任务的投递路由；没有行 = 直接任务。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn task_delivery(&self, task_id: Id)
        -> Result<Option<ChannelTaskDeliveryRow>, RepoError>;

    /// 任务输入的渠道出处两半（见 [`RepoTaskDelivery::task_origin`]）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn task_origin(&self, task_id: Id) -> Result<TaskOrigin, RepoError>;
}

/// 出站卡片（**泛化** `channel_outbound_card_message`）。
#[async_trait]
pub trait CardStore: Send + Sync {
    /// 按任务读卡片（上游 `GetLarkOutboundCardByTask`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn by_task(
        &self,
        task_id: Id,
    ) -> Result<Option<ChannelOutboundCardMessageRow>, RepoError>;

    /// 建 / 取卡片行（上游 `CreateLarkOutboundCardMessage`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn upsert(
        &self,
        new: &NewOutboundCard,
    ) -> Result<ChannelOutboundCardMessageRow, RepoError>;

    /// 翻状态；`false` = 行已经是终态（上游 `UpdateLarkOutboundCardStatus` 的"改了 0 行"）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn mark_status(&self, card_id: Id, status: CardStatus) -> Result<bool, RepoError>;

    /// 按会话列卡片（诊断 / 收尾）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn by_session(
        &self,
        session_id: Id,
    ) -> Result<Vec<ChannelOutboundCardMessageRow>, RepoError>;
}

/// 成员绑定（**遗留** `lark_user_binding`）。
#[async_trait]
pub trait UserBindingLookup: Send + Sync {
    /// 按 `(installation, open_id)` 读（上游 `GetLarkUserBindingByOpenID`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn by_open_id(
        &self,
        installation_id: Id,
        open_id: &str,
    ) -> Result<Option<UserBinding>, RepoError>;
}

/// agent 显示名（上游 `GetAgent` 的 `Name` 那一格）。
#[async_trait]
pub trait AgentNameLookup: Send + Sync {
    /// 取显示名；查不到 ⇒ `Ok(None)`（回落默认卡片头）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn agent_name(&self, agent_id: Id) -> Result<Option<String>, RepoError>;
}

// =====================================================================
// 桥：一个结构，四个端口
// =====================================================================

/// 出站 / 打字 / 回复器共用的数据层（上游 `ChannelStore` 的**可注入**形态）。
///
/// 上游把它当 `*db.Queries` 的包装直接传给各处；本仓把它拆成五个小端口再收进一个结构，
/// 于是（a）用例完全不必起库，（b）"哪一族表"这件事在字段名上可读。
pub struct LarkChannelStore {
    installations: Arc<dyn InstallationLookup>,
    sessions: Arc<dyn SessionBindingStore>,
    deliveries: Arc<dyn TaskDeliveryStore>,
    cards: Arc<dyn CardStore>,
    bindings: Arc<dyn UserBindingLookup>,
    agents: Option<Arc<dyn AgentNameLookup>>,
}

impl fmt::Debug for LarkChannelStore {
    /// 手写：只报"哪几面在"（表名见各适配器的 `Debug`），**没有**任何凭据字段。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LarkChannelStore")
            .field("installations", &LEGACY_INSTALLATION_TABLE)
            .field(
                "sessions",
                &format!("{LEGACY_SESSION_BINDING_TABLE} + {GENERIC_SESSION_BINDING_TABLE}"),
            )
            .field("deliveries", &TASK_DELIVERY_TABLE)
            .field("cards", &OUTBOUND_CARD_TABLE)
            .field("bindings", &LEGACY_USER_BINDING_TABLE)
            .field("has_agents", &self.agents.is_some())
            .finish()
    }
}

impl LarkChannelStore {
    /// 任意接缝（用例注入替身）。
    #[must_use]
    pub fn new(
        installations: Arc<dyn InstallationLookup>,
        sessions: Arc<dyn SessionBindingStore>,
        deliveries: Arc<dyn TaskDeliveryStore>,
        cards: Arc<dyn CardStore>,
        bindings: Arc<dyn UserBindingLookup>,
    ) -> Self {
        Self {
            installations,
            sessions,
            deliveries,
            cards,
            bindings,
            agents: None,
        }
    }

    /// 接上 agent 名字查询（卡片头要它）。
    #[must_use]
    pub fn with_agents(mut self, agents: Arc<dyn AgentNameLookup>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// 生产形态：`mc-repos` 的仓储。
    ///
    /// `delivery` 把投递面与 agent 名字面**一起**给出（上游的 `ChannelStore` 也是把两者嵌在
    /// 同一个 `*db.Queries` 上的）。
    #[must_use]
    pub fn from_repos(
        installations: ChannelInstallationRepo,
        sessions: (LarkChatSessionBindingRepo, ChannelChatSessionRepo),
        delivery: RepoTaskDelivery,
        cards: ChannelOutboundRepo,
        bindings: ChannelBindingRepo,
    ) -> Self {
        let delivery = Arc::new(delivery);
        Self {
            installations: Arc::new(RepoInstallations::new(installations)),
            sessions: Arc::new(RepoSessionBindings::new(sessions.0, sessions.1)),
            deliveries: Arc::clone(&delivery) as Arc<dyn TaskDeliveryStore>,
            cards: Arc::new(RepoCards::new(cards)),
            bindings: Arc::new(RepoUserBindings::new(bindings)),
            agents: Some(delivery as Arc<dyn AgentNameLookup>),
        }
    }

    /// 安装投影（遗留行）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn installation(&self, id: Id) -> EngineResult<Option<LarkInstallation>> {
        self.installations
            .get(id)
            .await
            .map_err(store_error("installation"))
    }

    /// 一条遗留会话绑定（按 `chat_session` 读）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn session_binding(
        &self,
        session_id: Id,
    ) -> EngineResult<Option<ChatSessionBinding>> {
        let legacy = self
            .sessions
            .legacy_by_session(session_id)
            .await
            .map_err(store_error("session binding"))?;
        Ok(legacy.as_ref().map(ChatSessionBinding::from_legacy_row))
    }

    /// 成员绑定（遗留表）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn user_binding(
        &self,
        installation_id: Id,
        open_id: &str,
    ) -> EngineResult<Option<UserBinding>> {
        self.bindings
            .by_open_id(installation_id, open_id)
            .await
            .map_err(store_error("user binding"))
    }

    /// 卡片行（泛化表）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn card_by_task(&self, task_id: Id) -> EngineResult<Option<OutboundCardMessage>> {
        self.cards
            .by_task(task_id)
            .await
            .map(|row| row.as_ref().map(OutboundCardMessage::from))
            .map_err(store_error("card"))
    }

    /// 补写遗留会话绑定行（D11 交接的那一半）。
    ///
    /// 上游在**出站 / 存储面**维护 `lark_chat_session_binding`；M7-12 的通用 binder 只把它
    /// 接进端口、不写行，于是写行落在本方法。幂等：已经有行 ⇒ 返回既有的那一行（**不**重写，
    /// 否则会把上游的 `last_lark_message_id` 游标抹掉）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn bridge_session_binding(
        &self,
        session_id: Id,
        installation_id: Id,
        chat_id: &str,
        chat_type: ChatType,
    ) -> EngineResult<ChatSessionBinding> {
        if let Some(existing) = self
            .sessions
            .legacy_by_session(session_id)
            .await
            .map_err(store_error("session binding"))?
        {
            return Ok(ChatSessionBinding::from_legacy_row(&existing));
        }
        let inserted = self
            .sessions
            .insert_legacy(session_id, installation_id, chat_id, chat_type)
            .await
            .map_err(store_error("session binding insert"))?;
        Ok(ChatSessionBinding::from_legacy_row(&inserted))
    }

    /// 推进遗留绑定行的"最近一条触发"游标（上游 `UpdateLarkChatSessionBindingReplyTarget`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn remember_reply_target(
        &self,
        session_id: Id,
        message_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> EngineResult<u64> {
        self.sessions
            .update_legacy_reply_target(session_id, message_id, thread_id)
            .await
            .map_err(store_error("session binding cursor"))
    }

    /// 这条任务的会话绑定（上游 `processEvent` 的组装）。
    ///
    /// 两步：① 投递行给"**这次**触发的消息"与发件人；② 遗留绑定行补 `chat_session_id`
    /// （投递行没有那一列）、并且**只在遗留行确实存在时**才覆盖游标 —— 否则投递行的游标
    /// 更权威（上游同：投递行是**按任务冻结**的）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn binding_for_task(&self, task_id: Id) -> EngineResult<Option<ChatSessionBinding>> {
        let Some(delivery) = self
            .deliveries
            .task_delivery(task_id)
            .await
            .map_err(store_error("task delivery"))?
        else {
            return Ok(None);
        };
        if delivery.channel_type != CHANNEL_TYPE_FEISHU {
            return Ok(None);
        }
        let mut binding = ChatSessionBinding::from_delivery_row(&delivery);
        if let Some(legacy) = self
            .sessions
            .legacy_by_chat(Id(delivery.installation_id), &delivery.channel_chat_id)
            .await
            .map_err(store_error("session binding"))?
        {
            binding = binding.with_legacy_row(&legacy);
        }
        Ok(Some(binding))
    }
}

/// `RepoError` → engine 的错误（**只带类别与端口名**）。
fn store_error(port: &'static str) -> impl Fn(RepoError) -> EngineError {
    move |error| EngineError::infra(format!("lark store ({port}): {error}"))
}

#[async_trait]
impl PatcherQueries for LarkChannelStore {
    async fn task_delivery(&self, task_id: Id) -> EngineResult<Option<ChannelTaskDeliveryRow>> {
        self.deliveries
            .task_delivery(task_id)
            .await
            .map_err(store_error("task delivery"))
    }

    async fn task_origin(&self, task_id: Id) -> EngineResult<TaskOrigin> {
        self.deliveries
            .task_origin(task_id)
            .await
            .map_err(store_error("task origin"))
    }

    async fn agent_name(&self, agent_id: Id) -> EngineResult<Option<String>> {
        OutcomeReplierQueries::agent_name(self, agent_id).await
    }

    async fn installation(&self, installation_id: Id) -> EngineResult<Option<LarkInstallation>> {
        LarkChannelStore::installation(self, installation_id).await
    }

    async fn binding_for_task(&self, task_id: Id) -> EngineResult<Option<ChatSessionBinding>> {
        LarkChannelStore::binding_for_task(self, task_id).await
    }

    async fn card_by_task(&self, task_id: Id) -> EngineResult<Option<OutboundCardMessage>> {
        LarkChannelStore::card_by_task(self, task_id).await
    }

    async fn upsert_card(&self, new: &NewOutboundCard) -> EngineResult<OutboundCardMessage> {
        self.cards
            .upsert(new)
            .await
            .map(|row| OutboundCardMessage::from(&row))
            .map_err(store_error("card upsert"))
    }

    async fn mark_card_status(&self, card_id: Id, status: CardStatus) -> EngineResult<bool> {
        self.cards
            .mark_status(card_id, status)
            .await
            .map_err(store_error("card status"))
    }

    async fn update_reply_target(
        &self,
        session_id: Id,
        message_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> EngineResult<u64> {
        LarkChannelStore::remember_reply_target(self, session_id, message_id, thread_id).await
    }
}

#[async_trait]
impl TypingIndicatorQueries for LarkChannelStore {
    async fn installation(&self, id: Id) -> EngineResult<Option<LarkInstallation>> {
        LarkChannelStore::installation(self, id).await
    }
}

#[async_trait]
impl OutcomeReplierQueries for LarkChannelStore {
    async fn agent_name(&self, agent_id: Id) -> EngineResult<Option<String>> {
        let Some(agents) = &self.agents else {
            return Ok(None);
        };
        agents
            .agent_name(agent_id)
            .await
            .map_err(store_error("agent name"))
    }
}

/// 本文件落的平台判别式（诊断用；`feishu` 是**存储**口径）。
#[must_use]
pub fn storage_kind() -> ChannelKind {
    TYPE_LARK
}

#[cfg(test)]
mod tests;
