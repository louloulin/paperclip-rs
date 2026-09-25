//! `outbound` 的**库面端口**（上游 `outboundQueries` / `deliveryLookup` 的那几个接口）。
//!
//! 本文件是 `outbound.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。
//! 端口存在的理由在模块文档里：adapter **不得**直接写 DB（`docs/60` §2.6 第 1 条）。

use std::sync::Arc;

use async_trait::async_trait;

use mc_core::channel::InstallationStatus;
use mc_core::id::Id;

// =====================================================================
// 库面端口（上游 `outboundQueries` / `deliveryLookup`）
// =====================================================================

/// `channel_task_delivery` 的一行的 `WeCom` 投影（上游 `db.ChannelTaskDelivery`）。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskDelivery {
    pub task_id: Id,
    pub binding_id: Id,
    pub installation_id: Id,
    pub channel_type: String,
    pub channel_chat_id: String,
    pub chat_type: String,
    pub channel_message_id: Option<String>,
    pub channel_thread_id: Option<String>,
    pub route_revision: i64,
    pub config: serde_json::Value,
}

impl TaskDelivery {
    /// 从泛化仓储的行结构投影（`mc_repos::channel::delivery::ChannelTaskDeliveryRow`）。
    pub fn from_row(row: &mc_repos::channel::delivery::ChannelTaskDeliveryRow) -> Self {
        Self {
            task_id: Id(row.task_id),
            binding_id: Id(row.binding_id),
            installation_id: Id(row.installation_id),
            channel_type: row.channel_type.clone(),
            channel_chat_id: row.channel_chat_id.clone(),
            chat_type: row.chat_type.clone(),
            channel_message_id: row.channel_message_id.clone(),
            channel_thread_id: row.channel_thread_id.clone(),
            route_revision: row.route_revision,
            config: row.config.clone(),
        }
    }

    /// 本轮的家聊（上游 `wecomBindingFromTaskDelivery` 的投影）。
    #[must_use]
    pub fn binding(&self) -> ChatSessionBinding {
        ChatSessionBinding {
            binding_id: self.binding_id,
            installation_id: self.installation_id,
            channel_type: self.channel_type.clone(),
            channel_chat_id: self.channel_chat_id.clone(),
            chat_type: self.chat_type.clone(),
            last_message_id: self.channel_message_id.clone(),
            last_thread_id: self.channel_thread_id.clone(),
            route_revision: self.route_revision,
            config: self.config.clone(),
        }
    }
}

/// `channel_chat_session_binding` 的 `WeCom` 投影（上游 `db.ChannelChatSessionBinding`）。
///
/// 本片只用它把投递行翻成一个**地址**；它的其余列留给需要它们的片。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatSessionBinding {
    pub binding_id: Id,
    pub installation_id: Id,
    pub channel_type: String,
    pub channel_chat_id: String,
    pub chat_type: String,
    pub last_message_id: Option<String>,
    pub last_thread_id: Option<String>,
    pub route_revision: i64,
    pub config: serde_json::Value,
}

/// `agent_task_queue` 的一行的出站投影（上游 `db.AgentTaskQueue`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTask {
    pub id: Id,
    /// 这个 task 所属的输入批次（**自动重试**的 clone 指的是它父级的那一个）；
    /// 上游靠它把 origin 门与轮次匹配对齐（`engine::commands` 的同名判据）。
    pub chat_input_task_id: Option<Id>,
    /// 入站批次里是否有渠道递进来的消息（上游 `TaskHasChannelIngestedMessages`）。
    pub batch_has_channel_ingested_messages: bool,
}

/// `channel_installation` 的**状态投影**（上游只用 `id` + `status` 两列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallationRecord {
    pub id: Id,
    pub status: InstallationStatus,
}

impl InstallationRecord {
    /// 上游逐字：`revoked` 行跳过一个已被路由的安装（触发与回复之间被撤销）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status.is_live()
    }
}

/// 收件人成员的 `WeCom` 绑定投影（上游 `db.ChannelUserBinding` 的三列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberBinding {
    pub installation_id: Id,
    pub workspace_id: Id,
    pub multica_user_id: Id,
    /// 机器人域内的用户 id（单聊推送的 `chatid` 就是它）。
    pub channel_user_id: String,
}

/// `WeCom` 出站订阅者要的那一片查询（上游 `outboundQueries`；`*db.Queries` 满足它）。
///
/// 每一条都**必须**按 `channel_type = 'wecom'` 收窄 —— 泛化表是五个 adapter 共享的。
#[async_trait]
pub trait OutboundQueries: Send + Sync {
    /// 这次完成走的**投递行**：提问被 ingest 时盖的章。**按 task 读而不是按会话**，
    /// 因为 `/new` 与 `/clear` 会把一个会话重指到另一个绑定上：跨过其中之一的、还在飞的
    /// 回答属于**当初问的那个聊**，不属于落地时会话指向的东西。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn get_task_delivery(&self, task_id: Id) -> Result<Option<TaskDelivery>, String>;

    /// 这个 task 那一行。origin 门读它取渠道入站的章；轮次匹配器读它把一次自动重试 clone
    /// 解回拥有它输入批次的那一轮。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn get_agent_task(&self, task_id: Id) -> Result<Option<AgentTask>, String>;

    /// 入站批次里是否有渠道递进来的消息。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn task_has_channel_ingested_messages(&self, task_id: Id) -> Result<bool, String>;

    /// 安装行的状态（收窄到 `wecom`）。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn get_installation(
        &self,
        installation_id: Id,
    ) -> Result<Option<InstallationRecord>, String>;

    /// 收件人成员在这个 workspace 里的 `WeCom` 绑定（上游 `FindChannelBindingForMember`）。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn find_binding_for_member(
        &self,
        workspace_id: Id,
        multica_user_id: Id,
    ) -> Result<Option<MemberBinding>, String>;

    /// workspace 的 slug（收件箱消息里的 Web 深链要它）。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn workspace_slug(&self, workspace_id: Id) -> Result<Option<String>, String>;
}

/// 把 task id 翻成 `WeCom` 聊的那**两条**读（上游 `deliveryLookup`）。
///
/// 与打字指示的失败告知**共用**：它寻址一轮无气泡的 run 的方式与回答**完全一样**
/// （`task_address`）。
#[async_trait]
pub trait DeliveryLookup: Send + Sync {
    /// 投递行。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn task_delivery(&self, task_id: Id) -> Result<Option<TaskDelivery>, String>;

    /// 安装状态。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn installation_record(
        &self,
        installation_id: Id,
    ) -> Result<Option<InstallationRecord>, String>;
}

/// 上游的两个接口由同一个 `*db.Queries` 满足。本仓的对应物是这条**明确的**转发 impl：
/// 一个 `Arc<dyn OutboundQueries>` 就是一个 [`DeliveryLookup`]，于是 M7-20 的打字指示可以
/// 只依赖那个窄接口（它手上那份注册表与 `Outbound` 共用同一次装配）。
#[async_trait]
impl DeliveryLookup for Arc<dyn OutboundQueries> {
    async fn task_delivery(&self, task_id: Id) -> Result<Option<TaskDelivery>, String> {
        OutboundQueries::get_task_delivery(self.as_ref(), task_id).await
    }

    async fn installation_record(
        &self,
        installation_id: Id,
    ) -> Result<Option<InstallationRecord>, String> {
        OutboundQueries::get_installation(self.as_ref(), installation_id).await
    }
}
