//! lark 的**丢弃审计**写入面（上游 `internal/integrations/lark/audit.go`，45 行）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - **上游定位**：`dbAuditLogger` —— `AuditLogger` 的**具体**实现，落在
//!   `lark_inbound_audit` 上。上游注释逐字：*It deliberately holds no caches and no
//!   indirection — the `RecordLarkInboundDrop` query rejects any column that could carry a
//!   message body, and this struct does too.*
//!
//! # 本片为什么还要它（M7-12 已经落了 `ChannelAuditor::lark`）
//!
//! M7-12 给 engine 的 [`crate::engine::resolvers::Auditor`] 端口接的是 `ChannelAuditor::lark`
//! （`mc-repos` 的遗留审计仓储）——那是**流水线内部**的丢弃记账。本文件落的是上游
//! `AuditLogger` 这**另一条**形状：它由**平台侧**的调用点（注册 / 卸载 / 陈旧事件的清理路径）
//! 使用，参数是**平台事件的原生字段**（`event_type` / `drop_reason` / `chat_id` /
//! `lark_event_id` / `lark_message_id`），而不是归一化后的 [`mc_core::channel::message::InboundMessage`]。
//! 两者写**同一张表**（遗留 `lark_inbound_audit`），但入口与入参形状不同 ⇒ 各自留住。
//! （M7-12 的写集里没有本文件，所以它只落了前者；这不是重复实现，登记为 D9。）
//!
//! # 类型层面的"不带正文"
//!
//! 上游注释逐字的两半本仓都保留：
//!
//! 1. **接口签名不收正文** —— [`AuditLogger::record_drop`] 的入参里没有 `text` / `body` /
//!    `content`，调用方**无法**把正文递进来；
//! 2. **空串写成 `NULL` 而不是 `''`** —— [`super::store::text_if_non_empty`] 逐字照搬上游
//!    `textIfNonEmpty`：*Avoids storing literal empty strings in the audit table, which would
//!    mask the difference between "the event lacked this field" and "the field was deliberately
//!    empty."*
//!
//! # 凭据面
//!
//! 本文件的入参里**没有**任何凭据字段，日志只插值 `installation_id` / `event_type` /
//! `drop_reason` ⇒ [`LarkAuditLogger`] 的手写 `Debug` 只报端口存在性。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::inbound_audit::{LarkInboundAuditRepo, NewChannelInboundDrop};
use mc_repos::RepoError;

use super::resolvers::TYPE_LARK;
use super::store::text_if_non_empty;
use super::types::{ChatId, DropReason};
use crate::engine::resolvers::{EngineError, EngineResult};

// =====================================================================
// 入参（上游 `AuditDropParams`）
// =====================================================================

/// 一条丢弃事件的原生字段（上游 `AuditDropParams`）。
///
/// **没有正文列是设计**（见模块文档）：这里加一个 `body: String` 就等于把 §4.7 的"非内容审计"
/// 口径拆了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditDropParams {
    /// 平台给的事件类型（`im.message.receive_v1` 一类）。
    pub event_type: String,
    /// 丢弃类别（[`DropReason`] 的取值）。
    pub reason: DropReason,
    /// 没解析出安装的事件留 `None`。
    pub installation_id: Option<Id>,
    /// 平台会话 id（空 ⇒ 落 `NULL`）。
    pub chat_id: ChatId,
    /// 平台事件 id（空 ⇒ 落 `NULL`）。
    pub lark_event_id: String,
    /// 平台消息 id（空 ⇒ 落 `NULL`）。
    pub lark_message_id: String,
}

impl AuditDropParams {
    /// 最小形态：只有类别与安装。
    #[must_use]
    pub fn new(reason: DropReason, installation_id: Option<Id>) -> Self {
        Self {
            event_type: String::new(),
            reason,
            installation_id,
            chat_id: ChatId::default(),
            lark_event_id: String::new(),
            lark_message_id: String::new(),
        }
    }

    /// 补事件类型。
    #[must_use]
    pub fn with_event_type(mut self, event_type: impl Into<String>) -> Self {
        self.event_type = event_type.into();
        self
    }

    /// 补会话 id。
    #[must_use]
    pub fn with_chat_id(mut self, chat_id: ChatId) -> Self {
        self.chat_id = chat_id;
        self
    }

    /// 补平台事件 / 消息 id。
    #[must_use]
    pub fn with_ids(mut self, event_id: impl Into<String>, message_id: impl Into<String>) -> Self {
        self.lark_event_id = event_id.into();
        self.lark_message_id = message_id.into();
        self
    }

    /// 转成 `mc-repos` 的写入入参（**两族共用的唯一形状**：`NewChannelInboundDrop`）。
    ///
    /// `channel_chat_id` / `channel_event_id` / `channel_message_id` 三格走
    /// [`super::store::text_if_non_empty`] ⇒ 空串落 `NULL`；`installation_id` 原样（`None` =
    /// `NULL`）。
    #[must_use]
    pub fn to_drop(&self) -> NewChannelInboundDrop {
        NewChannelInboundDrop {
            installation_id: self.installation_id,
            kind: TYPE_LARK,
            channel_chat_id: text_if_non_empty(self.chat_id.as_str()),
            event_type: self.event_type.clone(),
            channel_event_id: text_if_non_empty(&self.lark_event_id),
            channel_message_id: text_if_non_empty(&self.lark_message_id),
            drop_reason: self.reason.as_str(),
        }
    }
}

// =====================================================================
// 端口
// =====================================================================

/// 审计写入面（上游 `AuditLogger`）。
///
/// 上游签名逐字：`RecordDrop(ctx, p AuditDropParams) error`。本仓加 `Send + Sync`
/// 与 `async_trait`，其余不变。
#[async_trait]
pub trait AuditLogger: Send + Sync {
    /// 记一条丢弃（**只记元数据，不记正文**）。
    ///
    /// # Errors
    ///
    /// 链路失败 ⇒ [`EngineError::Infra`]（调用方按"审计没落上"记 warn —— **不**回滚入站判决：
    /// 上游的审计是旁路，不是投递的一部分）。
    async fn record_drop(&self, params: AuditDropParams) -> EngineResult<()>;
}

/// 写一行审计的**唯一**语句（上游 `ChannelStore::RecordLarkInboundDrop` 的契约子集）。
#[async_trait]
pub trait AuditQueries: Send + Sync {
    /// 落一行；返回主键。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn record_drop(&self, drop: &NewChannelInboundDrop) -> Result<Id, RepoError>;

    /// 按安装列出丢弃（诊断 / 用例：断言"落的是**这一条**"）。
    ///
    /// # Errors
    ///
    /// 链路失败。
    async fn list_by_installation(
        &self,
        installation_id: Id,
        limit: i64,
    ) -> Result<Vec<super::store::InboundAuditRow>, RepoError>;
}

#[async_trait]
impl AuditQueries for LarkInboundAuditRepo {
    async fn record_drop(&self, drop: &NewChannelInboundDrop) -> Result<Id, RepoError> {
        LarkInboundAuditRepo::record_drop(self, drop).await
    }

    async fn list_by_installation(
        &self,
        installation_id: Id,
        limit: i64,
    ) -> Result<Vec<super::store::InboundAuditRow>, RepoError> {
        LarkInboundAuditRepo::list_by_installation(self, installation_id, limit, 0)
            .await
            .map(|rows| {
                rows.iter()
                    .map(super::store::InboundAuditRow::from)
                    .collect()
            })
    }
}

// =====================================================================
// 生产实现（上游 `dbAuditLogger`）
// =====================================================================

/// 落 `lark_inbound_audit` 的审计器（上游 `dbAuditLogger`）。
///
/// 上游注释逐字：*holds no caches and no indirection* ⇒ 本结构只有一个端口字段。
pub struct LarkAuditLogger {
    queries: Arc<dyn AuditQueries>,
}

impl fmt::Debug for LarkAuditLogger {
    /// 手写：端口只报存在性（表名与判别式，**没有**凭据）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LarkAuditLogger")
            .field("queries", &"<dyn AuditQueries>")
            .field("kind", &TYPE_LARK)
            .finish()
    }
}

impl LarkAuditLogger {
    /// 任意接缝（用例注入替身）。
    #[must_use]
    pub fn new(queries: Arc<dyn AuditQueries>) -> Self {
        Self { queries }
    }

    /// 生产形态：直接用遗留审计仓储。
    #[must_use]
    pub fn from_repo(repo: LarkInboundAuditRepo) -> Self {
        Self::new(Arc::new(repo))
    }
}

#[async_trait]
impl AuditLogger for LarkAuditLogger {
    async fn record_drop(&self, params: AuditDropParams) -> EngineResult<()> {
        self.queries
            .record_drop(&params.to_drop())
            .await
            .map(|_id| ())
            .map_err(|error| EngineError::infra(format!("lark audit: {error}")))
    }
}

/// 本 auditor 落哪一族表（诊断 / `docs/32` 的两族对照表要它）。
///
/// 上游在本文件里硬编码 `NewChannelStore(queries)` ⇒ 走的是 `channel_inbound_audit`；
/// 本仓按 M7-1 的硬约束走**遗留** `lark_inbound_audit`（见 [`super::store`] 的表）。
/// 登记为 D10。
#[must_use]
pub fn audit_kind() -> ChannelKind {
    TYPE_LARK
}

/// 丢弃类别的字面量（审计列写的就是它；与 engine 的
/// [`crate::engine::resolvers::DropReason::as_str`] **取值逐字相同**）。
#[must_use]
pub fn drop_reason_str(reason: DropReason) -> &'static str {
    reason.as_str()
}

#[cfg(test)]
mod tests;
