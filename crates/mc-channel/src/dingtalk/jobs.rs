//! adapter 的**作业面**：bot 名来源 + 入站作业体 + 连接的回调汇
//! （上游 `dingtalk_channel.go` 的 `onMessage` / `runInbound` 两段）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 拆出来是**门 ⑩**（800 行硬限）的要求：这条边界是"连接怎么跑"与"一条回调怎么变成入库
//!   消息"之间的分界 —— 前者在 `mod.rs`，后者在这里。
//!
//! # 顺序是承重的
//!
//! [`CallbackJobHandler::handle`] 里三件事**必须**按这个顺序（上游逐字）：
//! 1. 解析 bot 的可读身份（只在"群 + 被 @ + 有名字源"时）：它要在归一化**之前**拿到，
//!    因为提及剥离需要那个经校验的名字；
//! 2. 归一化（[`crate::dingtalk::inbound`]）；
//! 3. 交给共享 engine handler，并**恒返回 `Ok`** —— `DingTalk` 从不重投机器人消息，engine 的
//!    `(installation, msgId)` 去重兜住任何重复投递。

use std::sync::Arc;

use async_trait::async_trait;

use crate::channel::ChannelResult;
use crate::message::SharedInboundHandler;

use super::dispatch::{Dispatcher, InboundJob, JobHandler};
use super::inbound::{inbound_from_callback_with_bot_name, BotCallbackData, CONV_TYPE_GROUP};
use super::stream::CallbackSink;

// =====================================================================
// bot 名字源（M7-9 的 `bot_identity.go` 面）
// =====================================================================

/// 本安装在某个会话里**可读的** bot 名（上游 `BotNameResolver.Resolve`）。
///
/// 名字的唯一用途是"把群里的 `@bot` 提及剥掉"（[`inbound::remove_dingtalk_bot_mention`]）：
/// `DingTalk` **不给**提及跨度，所以**只有**经平台 API 校验过的名字才允许用来剥。拿不到名字时
/// 正确的行为是[**失败关闭**]：保留每一个可见提及，而不是靠空白猜跨度。
pub trait BotNameSource: Send + Sync {
    /// 同步取名字（实现自己管缓存 / 权限缓存；`None` = 拿不到）。
    ///
    /// 上游的 `Resolve` 是 async 且要打平台 API；本仓把它留给宿主注入的实现，本片只定形状
    /// （M7-9 的 `bot_identity.go` 面接进来时换掉 [`NoBotName`]）。
    fn bot_name(&self, app_key: &str, conversation_id: &str) -> Option<String>;
}

/// 永远拿不到名字的诚实默认值（= 上游"空 bot 名"的失败关闭语义）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBotName;

impl BotNameSource for NoBotName {
    fn bot_name(&self, _app_key: &str, _conversation_id: &str) -> Option<String> {
        None
    }
}

// =====================================================================
// 入站作业体（上游 `dingtalkChannel.runInbound`）
// =====================================================================

/// 队列作业体：**先**解析 bot 可读身份，**再**归一化，最后交给共享 engine handler。
///
/// 顺序是承重的（上游逐字）：DingTalk 的提及剥离需要"经校验的 bot 名"，而那个名字只对被 @
/// 的群消息有意义（直聊不需要剥离；没被 @ 的群消息根本不会走到这里）。
pub(crate) struct CallbackJobHandler {
    pub(crate) app_key: String,
    pub(crate) handler: Option<SharedInboundHandler>,
    pub(crate) bot_names: Arc<dyn BotNameSource>,
}

impl std::fmt::Debug for CallbackJobHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CallbackJobHandler")
            .field("app_key", &self.app_key)
            .field("handler", &self.handler.is_some())
            .field("bot_names", &"<dyn BotNameSource>")
            .finish()
    }
}

#[async_trait]
impl JobHandler for CallbackJobHandler {
    async fn handle(&self, job: InboundJob) {
        let data = &job.callback;
        let bot_name = self.resolve_bot_name(data);
        let Some(message) = inbound_from_callback_with_bot_name(
            Some(data),
            &job.app_id,
            bot_name.as_deref().unwrap_or(""),
        ) else {
            // 这条**永远进不了** engine（没有发送者 staff id：系统消息 / bot 自己发的）⇒
            // 也不会有 `channel_inbound_audit` 行。记一条 info 让报告可诊断，而不是无声消失
            // （上游逐字：malformed / over-quota 的媒体**已经**会以不可用占位符进 engine）。
            tracing::info!(
                app_key = self.app_key,
                msg_type = data.msgtype,
                msg_id = data.msg_id,
                has_sender = !data.sender_staff_id.is_empty(),
                "dingtalk: dropped unsupported inbound message"
            );
            return;
        };
        let Some(handler) = self.handler.clone() else {
            tracing::warn!(
                app_key = self.app_key,
                "dingtalk: inbound handler not configured; dropping normalized message"
            );
            return;
        };
        if let Err(error) = handler.handle(message).await {
            tracing::warn!(
                app_key = self.app_key,
                code = error.code(),
                "dingtalk: inbound handler error"
            );
            // ⚠️ **登记缺口**（`docs/32` §19）：上游在这里还会发一条脱离任务送达的
            // `/issue` 派发失败告知（`notifyIssueDispatchError`）。那条路径要用 `sender` /
            // `targetFromMessage` / `isAddressedIssueCommand` —— **全是 M7-8 的类型**
            // （`outbound_send.go` / `reply_source.go`）⇒ 由 M7-8 的回复器接管。
        }
    }
}

impl CallbackJobHandler {
    /// 只在"群 + 被 @ + 有名字源"时才去解析名字（上游逐字）。
    fn resolve_bot_name(&self, data: &BotCallbackData) -> Option<String> {
        if data.conversation_type != CONV_TYPE_GROUP || !data.is_in_at_list {
            return None;
        }
        self.bot_names
            .bot_name(&self.app_key, &data.conversation_id)
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
    }
}

/// 用例用：按生产形态造一个作业体（归一化 + 交给 engine handler）。
#[cfg(test)]
pub(crate) fn callback_job_handler(
    app_key: &str,
    handler: Option<SharedInboundHandler>,
    bot_names: Arc<dyn BotNameSource>,
) -> Arc<dyn JobHandler> {
    Arc::new(CallbackJobHandler {
        app_key: app_key.to_string(),
        handler,
        bot_names,
    })
}

/// 连接的回调汇：入队到 per-conversation 串行队列（**同步、永不阻塞**）。
pub(crate) struct DispatchSink {
    pub(crate) app_key: String,
    pub(crate) dispatcher: Arc<Dispatcher>,
}

impl std::fmt::Debug for DispatchSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DispatchSink")
            .field("app_key", &self.app_key)
            .field("dispatcher", &self.dispatcher)
            .finish()
    }
}

#[async_trait]
impl CallbackSink for DispatchSink {
    async fn on_callback(&self, callback: BotCallbackData) -> ChannelResult<()> {
        if callback.sender_staff_id.is_empty() {
            // 与作业体里的登记一致：没有发送者的消息不进 engine，也不进队列。
            tracing::info!(
                app_key = self.app_key,
                msg_type = callback.msgtype,
                msg_id = callback.msg_id,
                "dingtalk: dropped unsupported inbound message"
            );
            return Ok(());
        }
        let conversation_id = callback.conversation_id.clone();
        let outcome = self.dispatcher.enqueue(
            &conversation_id,
            InboundJob::new(self.app_key.clone(), callback),
        );
        if !outcome.queued() {
            // `enqueue` 自己已经记了 warn（队列满 / 已收口）；这里只补一条 outcome 便于诊断。
            tracing::debug!(
                app_key = self.app_key,
                conversation_id,
                outcome = ?outcome,
                "dingtalk: inbound job not queued"
            );
        }
        // 上游逐字：`onMessage` 恒返回 nil —— DingTalk 从不重投机器人消息，engine 的
        // `(installation, msgId)` 去重兜住任何重复投递。
        Ok(())
    }
}
