//! **一个气泡怎么被收尾**：`writeClosing` 的三格读法、降级成普通消息的那条路，以及把一次结束
//! 交给握着 socket 的副本的那两条路由。
//!
//! 本文件是 `typing.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分加上门 ⑩ 的
//! 800 行硬限（逐条清单见 `docs/32` §38 的 D9）。

use std::time::Instant;

use async_trait::async_trait;
use mc_core::id::Id;

use super::TypingIndicator;
use crate::wecom::outbound::{DeliveryBudget, FALLBACK_SEND_TIMEOUT};
use crate::wecom::outcome::unconfirmed_seal_reason;
use crate::wecom::relay::RelayFrame;
use crate::wecom::seal::{classify_seal, SealVerdict};
use crate::wecom::senders::{no_live_connection, LiveSenders};
use crate::wecom::stream_store::{
    RoundAddress, RoundTaker, StreamHandle, StreamSender, STREAM_CLOSE_TIMEOUT,
};
use crate::wecom::ws_sender::{Deadline, SenderError};

/// 一次收尾要的那一半外部世界（上游 `typing_indicator.go` 用的 `sendersRegistry` 的四个方法）。
///
/// 它是 [`StreamSender`]（M7-16 的端口）**加上**打字指示独有的三件：写一条普通消息、记一次开场，
/// 以及"本副本此刻有没有这把 socket"。生产落点就是 [`LiveSenders`]（本片的 `senders.rs`）。
#[async_trait]
pub trait RoundSenders: StreamSender {
    /// 一条普通消息（上游 `sendersRegistry.sendTextCtx`）。
    ///
    /// # Errors
    ///
    /// 发送侧的任意失败。
    async fn send_text(
        &self,
        installation_id: Id,
        chat_id: &str,
        chat_type: i32,
        content: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError>;

    /// 记一个气泡正在用户屏幕上、并且有人欠它一个收尾（上游 `recordOpened`）。
    fn record_opened(&self);

    /// 本副本**此刻**握着这个安装的活连接吗（上游 `senders.get(id) == nil` 的反面）。
    ///
    /// 它决定一条告知要不要走中继：本副本没有 socket **不**等于别处也没有。
    fn has_socket(&self, installation_id: Id) -> bool;
}

#[async_trait]
impl RoundSenders for LiveSenders {
    async fn send_text(
        &self,
        installation_id: Id,
        chat_id: &str,
        chat_type: i32,
        content: &str,
        deadline: Deadline,
    ) -> Result<(), SenderError> {
        LiveSenders::send_text(self, installation_id, chat_id, chat_type, content, deadline).await
    }

    fn record_opened(&self) {
        LiveSenders::record_opened(self);
    }

    fn has_socket(&self, installation_id: Id) -> bool {
        self.holds(installation_id)
    }
}

impl TypingIndicator {
    /// 一次收尾要的那个匹配器（上游 `rounds()`）：把事件上的 task id 变成它所属的那一轮。
    ///
    /// 血缘查询（上游 `taskLookup` 的 `ChatInputTaskID` 那一次读）来自 [`super::ports::TaskLookupRoots`]
    /// —— 它由宿主在装配时挂上（[`super::TypingIndicator::with_roots`]）。
    pub(crate) fn rounds(&self) -> RoundTaker {
        match self.streams.as_ref() {
            Some(streams) => RoundTaker::new(
                std::sync::Arc::clone(streams),
                self.roots.as_ref().map(std::sync::Arc::clone),
            ),
            None => RoundTaker::disabled(),
        }
    }

    /// 上游 `writeClosing`：用一个气泡封住一次结束，封不住就把同样的话当**一条新消息**发出去。
    ///
    /// 由存储决定**哪一个**气泡（它自己那把锁里已经把那一轮取走了 —— 那正是两个收尾器抢同一个 run
    /// 时只产出一帧收尾帧的原因）。
    ///
    /// 一个发不出去的收尾帧退回一条普通消息，与回答在 `outbound.rs` 里做的一样。这里的**话**比那里
    /// 更要紧：`StreamFailed` 是 `WeCom` 唯一产出的"那次运行没跑通"，所以一帧被判了永久拒绝的收尾帧
    /// 会让用户留着一个转圈，而那句解释**永远**不会到。寻址来自句柄（ingest 时捕获的），因为到现在
    /// 绑定行可能已经指向另一个聊了。
    pub(crate) async fn write_closing(
        &self,
        session_id: Id,
        handle: &StreamHandle,
        text: &str,
        why: &str,
    ) {
        let (Some(senders), Some(streams)) = (self.senders.as_ref(), self.streams.as_ref()) else {
            return;
        };
        let outcome = streams
            .seal(senders.as_ref() as &dyn StreamSender, handle, text)
            .await;
        let error = outcome.as_ref().err().cloned();
        match classify_seal(error.as_ref()) {
            SealVerdict::OnScreen => return,
            SealVerdict::Unknown => {
                // 与回答路径**同一**读法、**同一**理由：这些话可能已经在气泡里了，而 `WeCom` 没有
                // 撤回。一条用户可能看到两遍的告知，比一条他们可以再问一次的告知更糟。
                tracing::warn!(
                    chat_session_id = %session_id,
                    reason = why,
                    unconfirmed_reason = error
                        .as_ref()
                        .map_or("-", unconfirmed_seal_reason),
                    error = %error.as_ref().map_or_else(String::new, ToString::to_string),
                    "wecom typing: closing frame's outcome is unknown, not saying it again"
                );
                return;
            }
            SealVerdict::NotOnScreen => {
                tracing::warn!(
                    chat_session_id = %session_id,
                    reason = why,
                    unusable = error.as_ref().is_some_and(SenderError::stream_unusable),
                    error = %error.as_ref().map_or_else(String::new, ToString::to_string),
                    "wecom typing: closing frame refused, saying it as a new message"
                );
            }
        }
        // 降级那一步拿到**它自己**的一份预算（上游 `fallbackBudget`）：收尾会把这份预算花掉大半，
        // 而"那条普通消息"是这条路径存在的全部意义。
        let budget = DeliveryBudget::lasting(STREAM_CLOSE_TIMEOUT).fallback(Instant::now());
        let Some(installation_id) = handle.installation_id else {
            tracing::warn!(
                chat_session_id = %session_id,
                reason = why,
                "wecom typing: nowhere to say it — the handle names no installation"
            );
            return;
        };
        if let Err(error) = senders
            .send_text(
                installation_id,
                &handle.chat_id,
                handle.chat_type,
                text,
                budget.as_deadline(),
            )
            .await
        {
            tracing::warn!(
                chat_session_id = %session_id,
                reason = why,
                error = %error,
                "wecom typing: the fallback message was unsendable too"
            );
        }
    }

    /// 上游 `sayAsPlainMessage`：话说给**当初问的那个聊**，本副本没有 socket 时请求握着它的那个副本。
    ///
    /// 本副本没有 socket 不意味着别处也没有：握着租约的那个副本能说，而帧带着 task id，于是它封的是
    /// **对的那个**气泡，而不是在一个气泡下面再推一条消息。
    ///
    /// # Errors
    ///
    /// 没有活连接时 [`SenderError::NotAttempted`]；否则与发送侧同。
    pub(crate) async fn say_as_plain_message(
        &self,
        ctx: Deadline,
        session_id: Id,
        address: &RoundAddress,
        task_id: &str,
        text: &str,
    ) -> Result<(), SenderError> {
        let Some(senders) = self.senders.as_ref() else {
            return Err(no_live_connection());
        };
        let Some(installation_id) = address.installation_id else {
            return Err(no_live_connection());
        };
        if !senders.has_socket(installation_id) {
            if let Some(relay) = self.relay.as_ref() {
                let frame = RelayFrame::reply(
                    installation_id.to_string(),
                    address.chat_id.clone(),
                    address.chat_type,
                    text,
                    task_id,
                    "",
                    "",
                    &session_id.to_string(),
                );
                if relay.publish(&frame, task_id) {
                    tracing::debug!(
                        chat_session_id = %session_id,
                        installation_id = %installation_id,
                        "wecom typing: routed a run's ending to the replica holding the socket"
                    );
                    return Ok(());
                }
            }
        }
        let outcome = senders
            .send_text(
                installation_id,
                &address.chat_id,
                address.chat_type,
                text,
                ctx,
            )
            .await;
        if let Err(error) = outcome.as_ref() {
            tracing::warn!(
                chat_session_id = %session_id,
                installation_id = %installation_id,
                error = %error,
                "wecom typing: could not deliver a run's ending"
            );
        }
        outcome
    }

    /// 上游 `relaySeal`：请握着这一轮的那个副本把它收掉 —— **不说**它在哪儿。
    ///
    /// 没有地址查询：一帧封印帧按**轮次归属**路由（见 `relay/relayed.rs` 的 `deliver_relayed`），
    /// 而那正是取消路径能保持"拒绝去追一个地址"的原因。
    pub(crate) fn relay_seal(&self, session_id: Id, task_id: &str, reason: &str) -> bool {
        let Some(relay) = self.relay.as_ref() else {
            return false;
        };
        if task_id.is_empty() {
            return false;
        }
        let frame = RelayFrame::seal(reason, task_id, &session_id.to_string(), false);
        relay.publish(&frame, task_id)
    }

    /// 一次 run 的**来源**判定（上游 `originOf`）：它需要的就是本管理器的那个可选 task 端口。
    ///
    /// 三格各要不同的动作，见 [`super::ports::OriginVerdict`] 的文档。
    pub(crate) async fn origin_verdict(
        &self,
        session_id: Option<Id>,
        task_id: &str,
    ) -> super::ports::OriginVerdict {
        super::ports::origin_of(self.tasks.as_deref(), session_id, task_id).await
    }

    /// 上游 `releaseRefusedRound`：绑上去的 run 结果**不是**本 adapter 的，于是把一个气泡还回去。
    ///
    /// 没有它，那一轮会留在一个永远不会收尾的 run 上：提问者看着那个气泡转到平台自己结束它，而他们
    /// 自己的回答找不到轮次、降级成一条普通消息。
    ///
    /// **还回去**而不是封起来，因为当初为它打开的那个问题**还没有**被回答 —— 那一轮回去等一个 run，
    /// 而这正是 `retry_unbind` 留下的东西：这个会话的下一个 `task:queued` 取走它。
    pub(crate) fn release_refused_round(&self, session_id: Id, task_id: &str) {
        if task_id.is_empty() {
            return;
        }
        let Some(streams) = self.streams.as_ref() else {
            return;
        };
        streams.release_round(session_id, task_id);
    }
}

/// 兜底消息的预算长度（上游 `fallbackSendTimeout`：**一条** `aibot_send_msg`，不是会被重试的那种）。
pub const FALLBACK_SEND_BUDGET: std::time::Duration = FALLBACK_SEND_TIMEOUT;
