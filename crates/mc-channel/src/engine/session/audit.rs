//! 丢弃审计端口（`Auditor`）：**非内容**口径的唯一落点。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session.rs` 一度 826 行 > 800 硬限）。
//!
//! ⚠️ 与上游的一处**形态**差异（不是语义差异）：上游的 `event_type` 是**平台事件名**
//! （`im.message.receive_v1`…），归一化信封里没有它（平台原始载荷留在
//! [`InboundMessage::raw`]，只有 adapter 读）⇒ 这里用**归一化后的消息种类**
//! （`text` / `image` / …）当 `event_type`。`channel_event_id` / `channel_message_id` 照旧取
//! 信封的两个平台 id，所以"哪个事件被丢了"依然可查。
//!
//! 〔以下为搬过来的实现，`use` 由本文件自带。〕

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::inbound_audit;
use mc_repos::channel::inbound_audit::NewChannelInboundDrop;

use std::sync::Arc;

use super::{infra, non_empty_owned};
use crate::engine::resolvers::{Auditor, DropReason, EngineResult};

// =====================================================================
// 丢弃审计端口（`Auditor`）
// =====================================================================

/// 丢弃审计的写入接缝（**只记非内容**：路由 + 身份 + 原因 + 时刻）。
#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn record_drop(&self, drop: &NewChannelInboundDrop) -> EngineResult<()>;
}

#[async_trait]
impl AuditStore for inbound_audit::ChannelInboundAuditRepo {
    async fn record_drop(&self, drop: &NewChannelInboundDrop) -> EngineResult<()> {
        self.record_drop(drop).await.map(|_| ()).map_err(infra)
    }
}

#[async_trait]
impl AuditStore for inbound_audit::LarkInboundAuditRepo {
    async fn record_drop(&self, drop: &NewChannelInboundDrop) -> EngineResult<()> {
        self.record_drop(drop).await.map(|_| ()).map_err(infra)
    }
}

/// 把一条入站消息 + 判决摊成审计行（**非内容**口径的唯一落点）。
///
/// ⚠️ 与上游的一处**形态**差异（不是语义差异）：上游的 `event_type` 是**平台事件名**
/// （`im.message.receive_v1`…），归一化信封里没有它（平台原始载荷留在
/// [`InboundMessage::raw`]，只有 adapter 读）⇒ 这里用**归一化后的消息种类**
/// （`text` / `image` / …）当 `event_type`。`channel_event_id` / `channel_message_id` 照旧取
/// 信封的两个平台 id，所以"哪个事件被丢了"依然可查。
pub fn drop_from_message(
    installation_id: Option<Id>,
    message: &InboundMessage,
    reason: DropReason,
) -> NewChannelInboundDrop {
    NewChannelInboundDrop {
        installation_id,
        kind: message.source.channel_type,
        channel_chat_id: Some(message.source.chat_id.clone()),
        event_type: message.kind.as_str().to_string(),
        channel_event_id: non_empty_owned(&message.event_id),
        channel_message_id: non_empty_owned(&message.message_id),
        drop_reason: reason.as_str(),
    }
}

/// `Auditor` 的通用实现。
pub struct ChannelAuditor {
    store: Arc<dyn AuditStore>,
    kind: ChannelKind,
}

impl std::fmt::Debug for ChannelAuditor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelAuditor")
            .field("kind", &self.kind)
            .field("store", &"<dyn AuditStore>")
            .finish()
    }
}

impl ChannelAuditor {
    /// 用泛化审计表。
    pub fn generalized(repo: inbound_audit::ChannelInboundAuditRepo, kind: ChannelKind) -> Self {
        Self {
            store: Arc::new(repo),
            kind,
        }
    }

    /// 用 lark **遗留**审计表。
    pub fn lark(repo: inbound_audit::LarkInboundAuditRepo) -> Self {
        Self {
            store: Arc::new(repo),
            kind: ChannelKind::Lark,
        }
    }

    /// 任意接缝（用例注入替身）。
    pub fn with_store(store: Arc<dyn AuditStore>, kind: ChannelKind) -> Self {
        Self { store, kind }
    }

    /// 平台判别式（诊断）。
    pub fn kind(&self) -> ChannelKind {
        self.kind
    }
}

#[async_trait]
impl Auditor for ChannelAuditor {
    async fn record_drop(
        &self,
        installation_id: Option<Id>,
        message: &InboundMessage,
        reason: DropReason,
    ) -> EngineResult<()> {
        let drop = drop_from_message(installation_id, message, reason);
        debug_assert_eq!(drop.kind, self.kind);
        self.store.record_drop(&drop).await
    }
}
