//! `outbound` 的**发送面端口**（上游 `sendersRegistry` 的消费面，落点 = M7-20 的 `senders.rs`）
//! 加上"推到脱离任务"那个小工具。
//!
//! 本文件是 `outbound.rs` 的子模块：拆分依据是门 ⑩（`scripts/file_size_check.py` 的 800 行硬限）
//! 加上"一格 = 一个面"的记法纪律（`docs/60-M7-PLAN.md` §3.3）。逐条清单见 `docs/32` §34 的 D12。

use std::sync::Arc;

use async_trait::async_trait;

use mc_core::id::Id;

use crate::wecom::ws_sender::{Deadline, SenderError, WsSender};

// =====================================================================
// 脱离任务
// =====================================================================

/// 把一段 future 推到**脱离任务**上（有运行时 ⇒ 起任务；没有 ⇒ 打一条 warn）。
///
/// engine 的两个**同步**接缝（[`crate::engine::OutboundReplier`] 与
/// [`crate::engine::TypingNotifier`]）在这一侧都落到网络 I/O 上，而引擎的调用点绝不应该阻塞在
/// 那里（`docs/60` §2.6 第 5 条：出站不阻塞 ACK）⇒ 与 `telegram::spawn_detached` /
/// `dingtalk::outbound::spawn_detached` 同款（本文件自己一份，免得让 wecom 依赖 dingtalk）。
pub(crate) fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        tracing::warn!("wecom: no async runtime; skipping the detached task");
    }
}

// =====================================================================
// 发送面端口（上游 `sendersRegistry`，M7-20 的 `senders.rs`）
// =====================================================================

/// 一条安装**活着的** socket 上，出站面要的那一条能力（上游 `*wsSender` 的
/// `sendTextCtx`）。
#[async_trait]
pub trait LiveSender: Send + Sync {
    /// 往 `chat_id` 推一条文本。
    ///
    /// # Errors
    ///
    /// 发送侧的任意失败（[`SenderError`]，含"确定没发出"与"结局未知"的分野）。
    async fn send_text(
        &self,
        chat_id: &str,
        chat_type: i32,
        text: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError>;
}

/// 按 installation 找活的 socket（上游 `sendersRegistry.get`）。
///
/// `None` = 本副本这个安装没有活连接（监管器丢了租约 / 正在重连）——**不是**错误。
pub trait SenderLookup: Send + Sync {
    /// 活着的发送者，或者 `None`。
    fn get(&self, installation_id: Id) -> Option<Arc<dyn LiveSender>>;

    /// 收尾帧要的那一半（上游 `sendersRegistry.stream` / `streamRewrite` / `recordEnding`）。
    ///
    /// `None` = 这个注册表不提供流面 ⇒ 收尾拿不到发送者，回答退回普通消息。
    /// 默认实现给 `None`：M7-20 的注册表会覆盖它，而一个**只有** 出站发送能力的替身
    /// （用例）不必实现任何东西。
    fn stream_sender(&self) -> Option<&dyn crate::wecom::stream_store::StreamSender> {
        None
    }
}

/// `WsSender` 就是一条安装活着的那把 socket（M7-16 的产物）⇒ 它就是 [`LiveSender`]。
///
/// 这条 `impl` 在**本片**（`WsSender` 属于只读面，本片只给它加 trait 实现，不改它一行）。
#[async_trait]
impl LiveSender for WsSender {
    async fn send_text(
        &self,
        chat_id: &str,
        chat_type: i32,
        text: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        WsSender::send_text(self, chat_id, chat_type, text, deadline).await
    }
}
