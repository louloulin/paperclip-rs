//! `channel_store.rs` 的**生产适配器**：把五个 `mc-repos` 仓储接到桥的小端口上。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求，切点是「端口定义 ∥ 生产实现」：
//!   [`super`] 里没有一条 `mc-repos` 类型出现在端口签名上，本文件里没有一条业务判决。
//!
//! # 逐适配器的表族（`super::store` 的对照表在这里被执行）
//!
//! | 适配器 | 表 | 上游 |
//! | --- | --- | --- |
//! | [`RepoInstallations`] | 遗留 `lark_installation` | `GetLarkInstallation` ○ `…ByAppID` |
//! | [`RepoSessionBindings`] | 遗留 `lark_chat_session_binding`（写）+ 泛化 `channel_chat_session_binding`（读） | `…ChatSessionBinding` ○ `Update…ReplyTarget` |
//! | [`RepoTaskDelivery`] | 泛化 `channel_task_delivery` + `agent` | `GetChannelTaskDelivery` ○ `GetAgent` |
//! | [`RepoCards`] | 泛化 `channel_outbound_card_message` | `…OutboundCardByTask` ○ `Create…` ○ `Update…` |
//! | [`RepoUserBindings`] | 遗留 `lark_user_binding` | `GetLarkUserBindingByOpenID` |
//!
//! # 依赖方向
//!
//! 每个适配器只裹**一个**仓储（它们都 `Clone` 且只持一个 `Db`），并把它藏在一个小端口后面 ——
//! 于是用例可以注入内存替身（本 crate 没有 `sqlx` 依赖），生产用
//! [`super::LarkChannelStore::from_repos`] 装配。

use std::fmt;

use async_trait::async_trait;
use mc_core::id::Id;
use mc_repos::agent::AgentRepo;
use mc_repos::channel::binding::ChannelBindingRepo;
use mc_repos::channel::delivery::{ChannelDeliveryRepo, ChannelTaskDeliveryRow};
use mc_repos::channel::installation::{ChannelInstallationRepo, LarkInstallationRow};
use mc_repos::channel::outbound::{ChannelOutboundCardMessageRow, ChannelOutboundRepo};
use mc_repos::channel::session::{
    ChannelChatSessionBindingRow, ChannelChatSessionRepo, LarkChatSessionBindingRepo,
    LarkChatSessionBindingRow,
};
use mc_repos::RepoError;

use super::{
    AgentNameLookup, CardStore, InstallationLookup, SessionBindingStore, TaskDeliveryStore,
    UserBindingLookup, GENERIC_SESSION_BINDING_TABLE, LEGACY_INSTALLATION_TABLE,
    LEGACY_SESSION_BINDING_TABLE, LEGACY_USER_BINDING_TABLE, OUTBOUND_CARD_TABLE,
    TASK_DELIVERY_TABLE,
};
use crate::engine::resolvers::{EngineError, EngineResult};
use crate::lark::outbound::TaskOrigin;
use crate::lark::replier::OutcomeReplierQueries;
use crate::lark::resolvers::InstallationQueries;
use crate::lark::resolvers::LarkInstallation;
use crate::lark::resolvers::TYPE_LARK;
use crate::lark::store::{CardStatus, NewOutboundCard, UserBinding};
use crate::lark::types::ChatType;

/// 遗留安装行的适配器。
#[derive(Clone)]
pub struct RepoInstallations {
    repo: ChannelInstallationRepo,
}

impl RepoInstallations {
    /// 装配。
    #[must_use]
    pub fn new(repo: ChannelInstallationRepo) -> Self {
        Self { repo }
    }
}

impl fmt::Debug for RepoInstallations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepoInstallations")
            .field("table", &LEGACY_INSTALLATION_TABLE)
            .finish()
    }
}

#[async_trait]
impl InstallationLookup for RepoInstallations {
    async fn get(&self, id: Id) -> Result<Option<LarkInstallation>, RepoError> {
        match self.repo.get_lark(id).await {
            Ok(row) => Ok(Some(LarkInstallation::from(&row))),
            Err(RepoError::NotFound) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// 安装路由键的那一条查询（复用 M7-12 已经落好的 [`InstallationQueries`]）。
#[async_trait]
impl InstallationQueries for RepoInstallations {
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<LarkInstallation>, RepoError> {
        RepoInstallations::find_active_by_app_id(self, app_id).await
    }
}

impl RepoInstallations {
    /// 按 `app_id` 找活跃安装（上游 `GetLarkInstallationByAppID`）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<LarkInstallation>, RepoError> {
        self.repo
            .find_lark_by_app_id(app_id)
            .await
            .map(|row| row.as_ref().map(LarkInstallation::from))
    }

    /// 按 `app_id` 找**任何**安装（含已撤销；诊断用）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn find_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<LarkInstallationRow>, RepoError> {
        self.repo.find_lark_by_app_id(app_id).await
    }
}

/// 会话绑定的适配器（遗留写 + 泛化读）。
#[derive(Clone)]
pub struct RepoSessionBindings {
    legacy: LarkChatSessionBindingRepo,
    generic: ChannelChatSessionRepo,
}

impl RepoSessionBindings {
    /// 装配。
    #[must_use]
    pub fn new(legacy: LarkChatSessionBindingRepo, generic: ChannelChatSessionRepo) -> Self {
        Self { legacy, generic }
    }
}

impl fmt::Debug for RepoSessionBindings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepoSessionBindings")
            .field("legacy_table", &LEGACY_SESSION_BINDING_TABLE)
            .field("generic_table", &GENERIC_SESSION_BINDING_TABLE)
            .finish()
    }
}

#[async_trait]
impl SessionBindingStore for RepoSessionBindings {
    async fn legacy_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<LarkChatSessionBindingRow>, RepoError> {
        self.legacy.get_by_session(session_id).await
    }

    async fn legacy_by_chat(
        &self,
        installation_id: Id,
        chat_id: &str,
    ) -> Result<Option<LarkChatSessionBindingRow>, RepoError> {
        self.legacy.get_by_chat(installation_id, chat_id).await
    }

    async fn insert_legacy(
        &self,
        session_id: Id,
        installation_id: Id,
        chat_id: &str,
        chat_type: ChatType,
    ) -> Result<LarkChatSessionBindingRow, RepoError> {
        self.legacy
            .insert(session_id, installation_id, chat_id, chat_type)
            .await
    }

    async fn update_legacy_reply_target(
        &self,
        session_id: Id,
        message_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<u64, RepoError> {
        self.legacy
            .update_reply_target(session_id, message_id, thread_id)
            .await
    }

    async fn generic_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<ChannelChatSessionBindingRow>, RepoError> {
        self.generic
            .get_current_binding_by_session(session_id)
            .await
    }
}

/// 投递 + 出处 + agent 名字的适配器。
#[derive(Clone)]
pub struct RepoTaskDelivery {
    deliveries: ChannelDeliveryRepo,
    agents: Option<AgentRepo>,
}

impl RepoTaskDelivery {
    /// 装配（不接 agent 查询 ⇒ 卡片头回落默认值）。
    #[must_use]
    pub fn new(deliveries: ChannelDeliveryRepo) -> Self {
        Self {
            deliveries,
            agents: None,
        }
    }

    /// 接上 agent 查询（卡片头要名字）。
    #[must_use]
    pub fn with_agents(mut self, agents: AgentRepo) -> Self {
        self.agents = Some(agents);
        self
    }

    /// 任务输入的渠道出处两半（上游 `TaskInputIsChannelIngested` 的入参）。
    ///
    /// ⚠️ **登记为 D2（`docs/32` §32.1）**：上游要两样东西 —— 任务行上的
    /// `chat_input_task_id`，以及那条输入批次上有没有 `channel_ingested` 的消息
    /// （`TaskHasChannelIngestedMessages`）。**它们都在 `mc-repos` 的任务面**，而那里两个查询
    /// 都没有：`mc_repos::agent::tasks` 只有 `list_tasks` / `cancel_tasks` / `task_snapshot` /
    /// `run_counts_*`（**没有** `get(task_id)`），`chat_message` 也没有按任务查批次的查询 ——
    /// M7-2 的 `engine::commands::task_input_is_channel_ingested` 文档把同一条登记成缺口
    /// （*批次那条查询落在 `mc-repos` 的任务面，**不属于**本片写集 ⇒ 这里是纯判据，调用方把
    /// 查询结果传进来*）。
    ///
    /// ⇒ 生产默认值是上游 MUL-4988 **之前**的行为：
    ///
    /// > 有投递行 ⇒ 这条任务属于渠道。
    ///
    /// 这与上游当前的严格判据之间差一个"web 界面任务复用了 lark 会话、但输入不是渠道消息"的
    /// 场景 —— 那种情形下本仓会多投一条回复。**放宽**而非收紧（投递而非丢失），且是一条明确的
    /// 交接项：在 `mc-repos` 的任务面加两条查询，然后把这里换成读它们即可（端口形状不变）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    pub async fn task_origin(&self, task_id: Id) -> Result<TaskOrigin, RepoError> {
        Ok(TaskOrigin::from_batch_flag(
            self.deliveries.get_task_delivery(task_id).await?.is_some(),
        ))
    }
}

impl fmt::Debug for RepoTaskDelivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepoTaskDelivery")
            .field("table", &TASK_DELIVERY_TABLE)
            .field("has_agents", &self.agents.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl TaskDeliveryStore for RepoTaskDelivery {
    async fn task_delivery(
        &self,
        task_id: Id,
    ) -> Result<Option<ChannelTaskDeliveryRow>, RepoError> {
        self.deliveries.get_task_delivery(task_id).await
    }

    async fn task_origin(&self, task_id: Id) -> Result<TaskOrigin, RepoError> {
        RepoTaskDelivery::task_origin(self, task_id).await
    }
}

#[async_trait]
impl AgentNameLookup for RepoTaskDelivery {
    async fn agent_name(&self, agent_id: Id) -> Result<Option<String>, RepoError> {
        RepoTaskDelivery::agent_name(self, agent_id).await
    }
}

#[async_trait]
impl OutcomeReplierQueries for RepoTaskDelivery {
    async fn agent_name(&self, agent_id: Id) -> EngineResult<Option<String>> {
        AgentNameLookup::agent_name(self, agent_id)
            .await
            .map_err(|error| EngineError::infra(format!("lark store: {error}")))
    }
}

impl RepoTaskDelivery {
    /// agent 显示名（上游 `GetAgent` 的 `Name` 那一格）。
    ///
    /// # Errors
    ///
    /// 链路失败（`NotFound` ⇒ `Ok(None)`）。
    pub async fn agent_name(&self, agent_id: Id) -> Result<Option<String>, RepoError> {
        let Some(agents) = &self.agents else {
            return Ok(None);
        };
        match agents.get(agent_id).await {
            Ok(row) => Ok(Some(row.name)),
            Err(RepoError::NotFound) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// 出站卡片的适配器。
#[derive(Clone)]
pub struct RepoCards {
    cards: ChannelOutboundRepo,
}

impl RepoCards {
    /// 装配。
    #[must_use]
    pub fn new(cards: ChannelOutboundRepo) -> Self {
        Self { cards }
    }
}

impl fmt::Debug for RepoCards {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepoCards")
            .field("table", &OUTBOUND_CARD_TABLE)
            .finish()
    }
}

#[async_trait]
impl CardStore for RepoCards {
    async fn by_task(
        &self,
        task_id: Id,
    ) -> Result<Option<ChannelOutboundCardMessageRow>, RepoError> {
        self.cards.find_card_by_task(task_id).await
    }

    async fn upsert(
        &self,
        new: &NewOutboundCard,
    ) -> Result<ChannelOutboundCardMessageRow, RepoError> {
        self.cards
            .upsert_card(
                new.chat_session_id,
                Some(new.task_id),
                TYPE_LARK,
                &new.channel_chat_id,
                &new.channel_card_message_id,
            )
            .await
    }

    async fn mark_status(&self, card_id: Id, status: CardStatus) -> Result<bool, RepoError> {
        self.cards
            .mark_card_status(card_id, status.as_str())
            .await
            .map(|row| row.is_some())
    }

    async fn by_session(
        &self,
        session_id: Id,
    ) -> Result<Vec<ChannelOutboundCardMessageRow>, RepoError> {
        self.cards.list_cards_by_session(session_id).await
    }
}

/// 遗留成员绑定的适配器。
#[derive(Clone)]
pub struct RepoUserBindings {
    bindings: ChannelBindingRepo,
}

impl RepoUserBindings {
    /// 装配。
    #[must_use]
    pub fn new(bindings: ChannelBindingRepo) -> Self {
        Self { bindings }
    }
}

impl fmt::Debug for RepoUserBindings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepoUserBindings")
            .field("table", &LEGACY_USER_BINDING_TABLE)
            .finish()
    }
}

#[async_trait]
impl UserBindingLookup for RepoUserBindings {
    async fn by_open_id(
        &self,
        installation_id: Id,
        open_id: &str,
    ) -> Result<Option<UserBinding>, RepoError> {
        self.bindings
            .find_lark_user_binding(installation_id, open_id)
            .await
            .map(|row| row.as_ref().map(UserBinding::from))
    }
}
