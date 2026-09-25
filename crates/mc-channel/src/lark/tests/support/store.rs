//! `MemoryStore`：内存数据层（六个仓储端口 + 两族各一张表）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求，切点是「**数据替身** ∥ 平台替身」：
//!   本文件只实现 [`crate::lark::channel_store`] 的六个端口，**不**知道任何 wire 形状。
//!
//! # 两族表是真的两张
//!
//! `installations` / `legacy_bindings` / `user_bindings` 是**遗留** `lark_*`，
//! `generic_bindings` / `deliveries` / `cards` 是**泛化** `channel_*` —— 于是"哪一行归哪一族"
//! 在用例里也是可断言的（`channel_store/tests.rs` 里有一条专测）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use mc_core::id::Id;
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;
use mc_repos::channel::installation::LarkInstallationRow;
use mc_repos::channel::outbound::ChannelOutboundCardMessageRow;
use mc_repos::channel::session::{ChannelChatSessionBindingRow, LarkChatSessionBindingRow};
use mc_repos::RepoError;
use uuid::Uuid;

use super::{installation_row, legacy_binding_row};
use crate::lark::channel_store::{
    AgentNameLookup, CardStore, InstallationLookup, LarkChannelStore, SessionBindingStore,
    TaskDeliveryStore, UserBindingLookup,
};
use crate::lark::outbound::TaskOrigin;
use crate::lark::resolvers::LarkInstallation;
use crate::lark::store::{CardStatus, NewOutboundCard, UserBinding};
use crate::lark::types::ChatType;

// =====================================================================
// 内存数据层（六个仓储端口 + 两族各一张表）
// =====================================================================

/// 内存数据层：**两族表各一份**（见模块文档）。
///
/// 单一结构实现六个端口 ⇒ 用例装配 [`LarkChannelStore`] 只需三行。
#[derive(Default)]
pub(crate) struct MemoryStore {
    /// 遗留 `lark_installation`（键 = id）。
    pub(crate) installations: Mutex<HashMap<Uuid, LarkInstallationRow>>,
    /// 遗留 `lark_chat_session_binding`（键 = `chat_session_id`）。
    pub(crate) legacy_bindings: Mutex<HashMap<Uuid, LarkChatSessionBindingRow>>,
    /// 泛化 `channel_chat_session_binding`（键 = `chat_session_id`）。
    pub(crate) generic_bindings: Mutex<HashMap<Uuid, ChannelChatSessionBindingRow>>,
    /// 泛化 `channel_task_delivery`（键 = `task_id`）。
    pub(crate) deliveries: Mutex<HashMap<Uuid, ChannelTaskDeliveryRow>>,
    /// 泛化 `channel_outbound_card_message`（键 = `task_id`）。
    pub(crate) cards: Mutex<HashMap<Uuid, ChannelOutboundCardMessageRow>>,
    /// 遗留 `lark_user_binding`（键 = `(installation, open_id)`）。
    pub(crate) user_bindings: Mutex<HashMap<(Uuid, String), UserBinding>>,
    /// agent 名字（键 = `agent_id`）。
    pub(crate) agent_names: Mutex<HashMap<Uuid, String>>,
    /// 出处两半里"批次戳"那一半的脚本（缺省 ⇒ 照抄生产默认值，见 D2）。
    pub(crate) channel_ingested: Mutex<Option<bool>>,
    /// 出处两半里 `chat_input_task_id` 的脚本（缺省 ⇒ `None`，即"未知"）。
    pub(crate) chat_input_task_id: Mutex<Option<Id>>,
    /// 是否让安装行查询"看起来像行被删了"（运行时拆除那一支）。
    pub(crate) installation_gone: Mutex<bool>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryStore")
            .field("tables", &"lark_* + channel_* (in memory)")
            .finish_non_exhaustive()
    }
}

impl MemoryStore {
    /// 空库。
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 放进一条安装行。
    pub(crate) fn put_installation(&self, installation: &LarkInstallation) {
        self.installations
            .lock()
            .expect("poisoned")
            .insert(installation.id.0, installation_row(installation));
    }

    /// 放进一条投递行。
    pub(crate) fn put_delivery(&self, row: ChannelTaskDeliveryRow) {
        self.deliveries
            .lock()
            .expect("poisoned")
            .insert(row.task_id, row);
    }

    /// 放进一条遗留会话绑定行。
    pub(crate) fn put_legacy_binding(&self, row: LarkChatSessionBindingRow) {
        self.legacy_bindings
            .lock()
            .expect("poisoned")
            .insert(row.chat_session_id, row);
    }

    /// 放进一条泛化会话绑定行。
    pub(crate) fn put_generic_binding(&self, row: ChannelChatSessionBindingRow) {
        self.generic_bindings
            .lock()
            .expect("poisoned")
            .insert(row.chat_session_id, row);
    }

    /// 放进一个 agent 名字。
    pub(crate) fn put_agent_name(&self, agent_id: Id, name: &str) {
        self.agent_names
            .lock()
            .expect("poisoned")
            .insert(agent_id.0, name.to_string());
    }

    /// 装成 [`LarkChannelStore`]（桥本身是真代码）。
    pub(crate) fn store(self: &Arc<Self>) -> Arc<LarkChannelStore> {
        Arc::new(
            LarkChannelStore::new(
                Arc::clone(self) as Arc<dyn InstallationLookup>,
                Arc::clone(self) as Arc<dyn SessionBindingStore>,
                Arc::clone(self) as Arc<dyn TaskDeliveryStore>,
                Arc::clone(self) as Arc<dyn CardStore>,
                Arc::clone(self) as Arc<dyn UserBindingLookup>,
            )
            .with_agents(Arc::clone(self) as Arc<dyn AgentNameLookup>),
        )
    }

    /// 当前卡片行（按任务）。
    pub(crate) fn card(&self, task_id: Id) -> Option<ChannelOutboundCardMessageRow> {
        self.cards
            .lock()
            .expect("poisoned")
            .get(&task_id.0)
            .cloned()
    }
}

#[async_trait]
impl InstallationLookup for MemoryStore {
    async fn get(&self, id: Id) -> Result<Option<LarkInstallation>, RepoError> {
        if *self.installation_gone.lock().expect("poisoned") {
            return Ok(None);
        }
        Ok(self
            .installations
            .lock()
            .expect("poisoned")
            .get(&id.0)
            .map(LarkInstallation::from))
    }
}

#[async_trait]
impl SessionBindingStore for MemoryStore {
    async fn legacy_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<LarkChatSessionBindingRow>, RepoError> {
        Ok(self
            .legacy_bindings
            .lock()
            .expect("poisoned")
            .get(&session_id.0)
            .cloned())
    }

    async fn legacy_by_chat(
        &self,
        installation_id: Id,
        chat_id: &str,
    ) -> Result<Option<LarkChatSessionBindingRow>, RepoError> {
        Ok(self
            .legacy_bindings
            .lock()
            .expect("poisoned")
            .values()
            .find(|row| row.installation_id == installation_id.0 && row.lark_chat_id == chat_id)
            .cloned())
    }

    async fn insert_legacy(
        &self,
        session_id: Id,
        installation_id: Id,
        chat_id: &str,
        chat_type: ChatType,
    ) -> Result<LarkChatSessionBindingRow, RepoError> {
        if !self
            .installations
            .lock()
            .expect("poisoned")
            .contains_key(&installation_id.0)
        {
            // 真库的 `REFERENCES lark_installation(id)` 会拒这个插入。
            return Err(RepoError::Conflict);
        }
        let row = legacy_binding_row(session_id, installation_id, chat_id, chat_type);
        self.legacy_bindings
            .lock()
            .expect("poisoned")
            .insert(session_id.0, row.clone());
        Ok(row)
    }

    async fn update_legacy_reply_target(
        &self,
        session_id: Id,
        message_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<u64, RepoError> {
        let mut table = self.legacy_bindings.lock().expect("poisoned");
        let Some(row) = table.get_mut(&session_id.0) else {
            return Ok(0);
        };
        row.last_lark_message_id = message_id.map(str::to_string);
        row.last_lark_thread_id = thread_id.map(str::to_string);
        Ok(1)
    }

    async fn generic_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<ChannelChatSessionBindingRow>, RepoError> {
        Ok(self
            .generic_bindings
            .lock()
            .expect("poisoned")
            .get(&session_id.0)
            .cloned())
    }
}

#[async_trait]
impl TaskDeliveryStore for MemoryStore {
    async fn task_delivery(
        &self,
        task_id: Id,
    ) -> Result<Option<ChannelTaskDeliveryRow>, RepoError> {
        Ok(self
            .deliveries
            .lock()
            .expect("poisoned")
            .get(&task_id.0)
            .cloned())
    }

    /// 缺省**照抄生产默认值**（D2）：有投递行 ⇒ 批次戳为真；脚本非空时以脚本为准。
    async fn task_origin(&self, task_id: Id) -> Result<TaskOrigin, RepoError> {
        let flag = match *self.channel_ingested.lock().expect("poisoned") {
            Some(scripted) => scripted,
            None => self
                .deliveries
                .lock()
                .expect("poisoned")
                .contains_key(&task_id.0),
        };
        Ok(TaskOrigin {
            chat_input_task_id: *self.chat_input_task_id.lock().expect("poisoned"),
            batch_has_channel_ingested_messages: flag,
        })
    }
}

#[async_trait]
impl CardStore for MemoryStore {
    async fn by_task(
        &self,
        task_id: Id,
    ) -> Result<Option<ChannelOutboundCardMessageRow>, RepoError> {
        Ok(self.card(task_id))
    }

    async fn upsert(
        &self,
        new: &NewOutboundCard,
    ) -> Result<ChannelOutboundCardMessageRow, RepoError> {
        let mut table = self.cards.lock().expect("poisoned");
        // 上游 `ON CONFLICT (task_id) DO UPDATE SET … = channel_outbound_card_message.…`
        // ⇒ 冲突时**保留既有行**（含它的状态与 id）。
        if let Some(existing) = table.get(&new.task_id.0) {
            return Ok(existing.clone());
        }
        let row = ChannelOutboundCardMessageRow {
            id: Uuid::new_v4(),
            chat_session_id: new.chat_session_id.0,
            task_id: Some(new.task_id.0),
            channel_type: "feishu".to_string(),
            channel_chat_id: new.channel_chat_id.clone(),
            channel_card_message_id: new.channel_card_message_id.clone(),
            status: new.status.as_str().to_string(),
            last_patched_at: None,
            created_at: Utc::now(),
        };
        table.insert(new.task_id.0, row.clone());
        Ok(row)
    }

    async fn mark_status(&self, card_id: Id, status: CardStatus) -> Result<bool, RepoError> {
        let mut table = self.cards.lock().expect("poisoned");
        let Some(row) = table.values_mut().find(|row| row.id == card_id.0) else {
            return Ok(false);
        };
        if row.status == "final" || row.status == "error" {
            // 上游 SQL 的 `WHERE … AND status NOT IN ('final','error')` ⇒ 0 行。
            return Ok(false);
        }
        row.status = status.as_str().to_string();
        row.last_patched_at = Some(Utc::now());
        Ok(true)
    }

    async fn by_session(
        &self,
        session_id: Id,
    ) -> Result<Vec<ChannelOutboundCardMessageRow>, RepoError> {
        Ok(self
            .cards
            .lock()
            .expect("poisoned")
            .values()
            .filter(|row| row.chat_session_id == session_id.0)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl UserBindingLookup for MemoryStore {
    async fn by_open_id(
        &self,
        installation_id: Id,
        open_id: &str,
    ) -> Result<Option<UserBinding>, RepoError> {
        Ok(self
            .user_bindings
            .lock()
            .expect("poisoned")
            .get(&(installation_id.0, open_id.to_string()))
            .cloned())
    }
}

#[async_trait]
impl AgentNameLookup for MemoryStore {
    async fn agent_name(&self, agent_id: Id) -> Result<Option<String>, RepoError> {
        Ok(self
            .agent_names
            .lock()
            .expect("poisoned")
            .get(&agent_id.0)
            .cloned())
    }
}
