//! 入站路由：把一条 [`InboundMessage`] 变成"落地了什么"（上游 `channel/engine/` 的路由面）。
//!
//! **状态：M7-0 anchor 只落签名**（`LUM-1765`）—— 本文件是 `todo!()` 位，实现归 **M7-1**。
//!
//! # M7-1 要在这里落的六步（顺序有语义，`docs/60` §4.1 的 M7-1 行）
//!
//! 1. **dedup**：`channel_inbound_message_dedup` 两阶段幂等（`(installation, message_id)`
//!    主键 + `claim_token` 所有权围栏）。命中 ⇒ **丢弃且不报错**（上游 `handler.go` 契约）。
//! 2. **身份**：`channel_user_binding` 查发件人；未绑定 ⇒ 触发绑定卡（**不出错**）。
//! 3. **会话**：`channel_chat_session_binding` 查/建 `chat_session`（含代际
//!    `channel_chat_context_generation`）。
//! 4. **命令**：`/issue`、`/clear` 一类控制命令（`force_fresh` / `skip_agent_run` 的语义在
//!    `mc_core::channel::message::InboundMessage` 上已经定好）。
//! 5. **产物**：建 issue / task 行（渠道入站与 runtime 侧**经表**通信，见 crate 文档）。
//! 6. **触发 run**：默认触发一次 agent run；`skip_agent_run` 时只留产物。
//!
//! # 不做什么
//!
//! - 不碰平台 wire（那是 adapter）；
//! - 不直接写 SQL（走 `mc-repos` 的 `channel` 模块）；
//! - 不在这里决定"未配置"的 HTTP 响应（那是 route 层，逐 fixture 对齐）。

use std::sync::Arc;

use mc_core::channel::message::InboundMessage;

use crate::channel::ChannelResult;
use crate::engine::ChannelDeps;

/// 入站路由器。
///
/// ⚠️ anchor 期**不可构造**（[`Router::new`] 是 `todo!()`）：M7-1 会把它与仓储 / 会话 /
/// 媒体账本接起来。
pub struct Router {
    deps: Arc<ChannelDeps>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Router")
            .field("deps", &self.deps)
            .finish()
    }
}

impl Router {
    /// 装配（M7-1）。
    ///
    /// # Panics
    ///
    /// anchor 期未实现。
    pub fn new(_deps: Arc<ChannelDeps>) -> Self {
        todo!("M7-1：装配入站路由器（docs/60 §4.1 的 M7-1 行）")
    }

    /// 路由一条入站消息（M7-1）。
    ///
    /// 返回值与 [`crate::message::InboundHandler`] 的契约**同一个**：`Ok(())` = 已接受并分类
    /// （**包含**按产品理由丢弃），`Err(_)` = 基础设施失败。
    ///
    /// # Panics
    ///
    /// anchor 期未实现。
    pub async fn route(&self, _message: InboundMessage) -> ChannelResult<()> {
        todo!("M7-1：dedup → 身份 → 会话 → 命令 → 产物 → 触发 run")
    }
}
