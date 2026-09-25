//! **在握着 socket 的那个副本上执行一条被路由的投递**（上游 `relay_outbound.go` 的
//! `deliverRelayed` / `sealRelayedRound` / `ownsSocket` / `provablyNotSent` 那一半）。
//!
//! 本文件是 `relay.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分
//! （上游 1,578 行 ⇒ 按「重投递链 / 优先级队列」拆），逐条清单见 `docs/32` §34 的 D9。
//!
//! # 它跑在调度 worker 上，**从不**跑在分片读循环上（上游逐字）
//!
//! 帧驮的是**标识**而不是载荷，凡是这里读得到的东西都按 id 传：附件行由**这个**副本去取 ——
//! 它是那个能发出它们的副本 —— 而永不经过中继。

use std::time::Instant;

use async_trait::async_trait;

use crate::wecom::stream_store::by_task;
use crate::wecom::strings::{copy_for, Locale};
use crate::wecom::ws_frame::has_visible_char;
use crate::wecom::ws_sender::SenderError;

use super::super::outbound::{
    classify_seal, parse_uuid, DeliveryBudget, Outbound, SealVerdict, EVENT_BUDGET,
};
use super::super::outcome::unconfirmed_seal_reason;
use super::{RelayFrame, RelayHandler, RelayKind, RelayOutcome, RelayRecord, RelayResult};

/// 上游 `sealReasonCancelled`：用户自己停了这次运行。
pub const SEAL_REASON_CANCELLED: &str = "cancelled";

/// 上游 `sealReasonNoReply`：这一轮没什么可说的。
pub const SEAL_REASON_NO_REPLY: &str = "no_reply";

/// 上游 `wordlessSealCopy`：一个**没有自己的话**的结束用哪句话把气泡封上。
///
/// 一处定义，所以一条被中继的轮次与一条本地的轮次不可能对同一个结局说出不同的句子。
#[must_use]
pub fn wordless_seal_copy(locale: Locale, carries_files: bool) -> &'static str {
    let copy = copy_for(locale);
    if carries_files {
        copy.stream_no_reply_with_files
    } else {
        copy.stream_no_reply
    }
}

/// 上游 `provablyNotSent`：这条发送错误**确定**发生在任何字节有机会出去**之前**吗？
///
/// `ws_sender` 自己划那条界（`SenderError::is_not_attempted` 就是这张表）：写自身引发的失败是
/// `WriteAttempted`，永远没回来的判决是 `AckTimeout`，调用方不再等的判决是 `AckAbandoned`，
/// 而一个**说出来了的**拒绝是 `Api` —— 这四种都意味着对端可能已经（对一个拒绝而言是**确实**）
/// 看到了这一帧。
///
/// 一条裸的上下文错误读法相同，而那是一个**选择**而不是没能力。`request()` 现在自己标记写之后
/// 那种情况，所以剩下的只是一次在任何东西被写之前引发的取消。在它上面释放 claim 会是正确的，
/// 而这里**刻意不做**：这是帧被 offer 给另一个副本之前的最后一道闸，两个错误的代价不同 ——
/// 一次没被重试的投递 对 用户聊里的第二份回答 —— 而放宽"什么会被重投递"是对中继重试行为的改动，
/// 不是对错误读法的改动。
#[must_use]
pub fn provably_not_sent(error: Option<&SenderError>) -> bool {
    match error {
        None => false,
        // 一条超过上限的回答会分成好几帧发出去，而第二帧上的失败对第一帧什么都没说 ——
        // 用户已经在读它了。重试这样的发送会把已经落地的部分重复一遍。
        Some(error) => error.is_not_attempted(),
    }
}

#[async_trait]
impl RelayHandler for Outbound {
    /// 上游 `deliverRelayed`：执行**另一个副本**发布的一次投递。
    async fn deliver_relayed(&self, frame: &RelayFrame) -> RelayResult {
        // **封印帧在这一切之前就被回答了**，而且它按"谁握着**轮次**"过滤，而不是按"谁握着 socket"。
        // 这就是它不需要 installation id 的原因：一次取消**刻意**不追一个地址，而一个气泡只在
        // 画它的那个副本上可写 ⇒ "我有这一轮吗"问的是同一个问题，却不必读一次库。
        //
        // 它不写任何消息，所以下面那套回复机制一样都不适用：计数器不动、没有回退推送，
        // 而一个不在这里的轮次意味着这一帧**已经靠什么都不做完成了它的全部工作**。
        if frame.kind == RelayKind::Seal {
            self.seal_relayed_round(frame).await;
            return RelayResult::new(RelayOutcome::Done);
        }
        let Some(installation_id) = parse_uuid(&frame.installation_id) else {
            // 无从寻址；重试不能让它变得可寻址。
            return RelayResult::new(RelayOutcome::Done);
        };
        let Some(senders) = self.senders() else {
            return RelayResult::new(RelayOutcome::NotOurs);
        };
        let Some(sender) = senders.get(installation_id) else {
            // 握着租约的副本会接过它。
            return RelayResult::new(RelayOutcome::NotOurs);
        };

        // `record` 是这个持有者对一个完事帧的**一个**记录，由调度器在 claim 被结算之后施加。
        // 它**只**为 agent 回复动回复计数器 —— 那是它们文档里写着的单位。一条被路由到这里的
        // 收件箱推送**不许**动它们：同一条推送否则会算成一条送达的回复、一条丢弃的回复、
        // 或者什么都不算，取决于哪个副本恰好握着 socket。
        // `has_visible_char` 而不是 `!= ""`，而且是**本地路径用的同一个**谓词。一条驮着文件的
        // `"\n"` 完成对两边都不是话：本地路径什么都不发、让文件扛这条回复的结局，而一条被路由到
        // 这里的帧必须到达同样的两个结论，否则"哪个副本握着 socket"就决定了用户会不会看到一条
        // 空消息、以及这条回复计的是文字还是文件。
        //
        // 气泡优先：**这条回复属于的那个气泡可能就在这个副本上**（接一条被中继的回复的那个副本
        // 按定义就是握着 socket 的那个，而气泡只在画它的地方可写）。这一半抽成
        // [`Outbound::seal_relayed_bubble`]，因为它自己就有三十行证据链。
        let mut answer = self.seal_relayed_bubble(frame).await;
        let mut record = answer.record.take();
        let spoke = answer.spoke;
        let text = answer.text;
        let budget = answer.budget;

        // `text` 而不是 `frame.content`：一个无话结束的封印被拒时，本来要去封气泡的那句文案
        // 就是那个聊拿到的东西 —— 与本地路径做的替换一样。
        if has_visible_char(&text) && !spoke {
            let error = sender
                .send_text(&frame.chat_id, frame.chat_type, &text, budget.as_deadline())
                .await
                .err();
            if let Some(error) = error {
                // **这一帧完事没有，在任何计数器移动之前就定了。** 一条还欠一次 offer 的帧仍在飞，
                // 而在这里计数会**每次尝试都数一次**：调度器会把一条可证明没发出的帧重投递整条链
                // （生产默认值上是十一节），而发布方的 `watch_outcomes` 之后还会再结算一次 ⇒
                // 一条回复会在 `outbound_dropped` 上落十二三次 —— 而如果后来某一次 offer 成功了，
                // 还会同时落在 `outbound_delivered` 上。那正是"一条回复被同时数成送达与丢弃"的缺陷。
                //
                // 所以下面的计数器**只**为一个不会再被 offer 的帧记结局；一条仍在飞的帧的结局由那个
                // 唯一能在事后结算它的所有者欠着 —— 发布方的 `watch_outcomes`，它给一条没人接的投递
                // 记**恰好一次**。
                //
                // 只有**被证明发生在写之前**的失败才释放 claim。过了那一点的一切 —— 一次被尝试过的写、
                // 一个永远没回来的判决、一个在等待中过期的预算 —— 都可能已经到达对端，在那里释放
                // claim 会把一次重试变成用户聊里的第二份回答。
                if provably_not_sent(Some(&error)) {
                    tracing::debug!(
                        error = %error,
                        kind = frame.kind.as_str(),
                        installation_id = frame.installation_id.as_str(),
                        task_id = frame.task_id.as_str(),
                        "wecom relay: nothing reached the wire, the frame is owed another offer"
                    );
                    return RelayResult::new(RelayOutcome::ProvablyNotSent);
                }
                let owed = if frame.kind == RelayKind::Reply {
                    // 与直连路径**同一个**映射。尤其"部分发送"必须在两条路径上一致，
                    // 否则同一条回复会因为哪个副本握着 socket 而算成送达或丢弃 —— 见 `record_send`。
                    RelayRecord::Send {
                        session_id: frame.session_id.clone(),
                        event_type: frame.kind.as_str().to_string(),
                        error: Some(error),
                    }
                } else {
                    RelayRecord::InboxFailed {
                        installation_id: frame.installation_id.clone(),
                    }
                };
                return RelayResult::recorded(RelayOutcome::Done, owed);
            }
            if frame.kind == RelayKind::Reply {
                record = Some(RelayRecord::Delivered);
            }
        }
        if frame.kind == RelayKind::Inbox {
            return RelayResult::new(RelayOutcome::Done);
        }
        if frame.carries_files {
            // 附件行由本副本去取（`deliver_attachments` 的端口在 M7-18 落地），
            // 而"文件**就是**这条回复"只在原来那份正文没有可见字符时才成立。
            if let Some(target) = attachment_target(frame) {
                self.deliver_attachments_by_id(
                    &frame.message_id,
                    &frame.workspace_id,
                    &target,
                    !has_visible_char(&frame.content),
                );
            }
        }
        match record {
            Some(record) => RelayResult::recorded(RelayOutcome::Done, record),
            None => RelayResult::new(RelayOutcome::Done),
        }
    }

    /// 上游 `ownsSocket`：claim 之前的归属闸。故意便宜：一次查表。
    fn owns_socket(&self, installation_id: &str) -> bool {
        let Some(senders) = self.senders() else {
            return false;
        };
        match parse_uuid(installation_id) {
            Some(id) => senders.get(id).is_some(),
            None => false,
        }
    }

    /// 施加一个完事帧欠下的记录（上游 `relayResult.record` 那个闭包）。
    fn record(&self, record: RelayRecord) {
        match record {
            RelayRecord::Delivered => {
                self.delivered();
            }
            RelayRecord::Send {
                session_id,
                event_type,
                error,
            } => {
                // 返回值是给判决看的第二个读者；这里的**唯一**职责是让那一个映射跑一次。
                let _ = self.record_send(&session_id, &event_type, error.as_ref());
            }
            RelayRecord::Unconfirmed {
                session_id,
                event_type,
                reason,
                error,
            } => {
                let wrapped = error.map(crate::wecom::outbound::OutboundError::Send);
                self.unconfirmed_for(&session_id, &event_type, &reason, wrapped.as_ref());
            }
            RelayRecord::InboxFailed { installation_id } => {
                tracing::warn!(
                    installation_id = installation_id.as_str(),
                    "wecom relay: inbox push failed on the lease holder"
                );
            }
        }
    }
}

/// 一次被中继回答的**气泡那一半**的结论（见 [`Outbound::seal_relayed_bubble`]）。
struct RelayedBubble {
    /// 要发出去的话（气泡封上时它就是封进去的那句；被拒时退回普通消息用同一句）。
    text: String,
    /// 这一轮已经记过的那笔账（气泡封上时是 `Delivered` / `Unconfirmed`）。
    record: Option<RelayRecord>,
    /// 话**从本进程**到了用户那儿（气泡封上了）。
    spoke: bool,
    /// 退回普通消息时要用的预算（收尾花不掉的那一份）。
    budget: DeliveryBudget,
}

impl Outbound {
    /// 上游 `deliverRelayed` 的**气泡那一半**：这一轮的气泡在这里就封掉，而不是在它下面推第二条
    /// 消息。
    ///
    /// 三条别改的判断：
    ///
    /// - **只对回复**：一条收件箱推送不是对某一轮的回答，绝不许替它收尾；
    /// - **不以"有没有话"为闸**：一条只有文件的回答**仍然结束那一轮**，把 take 闸在
    ///   `has_visible_char` 上会让那一轮一直开着（文件到了，而它上面的转圈还在转）；
    /// - 话**是什么**要在拿到那一轮**之后**决定，因为文案必须用那一轮自己的 locale，
    ///   而那个 locale 在它的气泡被画出来时就捕获了。
    async fn seal_relayed_bubble(&self, frame: &RelayFrame) -> RelayedBubble {
        let mut answer = RelayedBubble {
            text: frame.content.clone(),
            record: None,
            spoke: false,
            budget: DeliveryBudget::lasting(EVENT_BUDGET),
        };
        if frame.kind != RelayKind::Reply || frame.task_id.is_empty() {
            return answer;
        }
        let Some(session_id) = parse_uuid(&frame.session_id) else {
            return answer;
        };
        let (turn, _) = self
            .rounds()
            .take(session_id, &by_task(frame.task_id.clone()))
            .await;
        let Some(turn) = turn.filter(|turn| turn.has_bubble) else {
            return answer;
        };
        if !has_visible_char(&answer.text) {
            answer.text = wordless_seal_copy(turn.handle.locale, frame.carries_files).to_string();
        }
        let seal = self.finish_stream(&turn.handle, &answer.text).await;
        match classify_seal(seal.as_ref()) {
            SealVerdict::OnScreen => {
                answer.record = Some(RelayRecord::Delivered);
                answer.spoke = true;
            }
            SealVerdict::Unknown => {
                let reason = seal
                    .as_ref()
                    .map_or("seal_unacked", unconfirmed_seal_reason)
                    .to_string();
                answer.record = Some(RelayRecord::Unconfirmed {
                    session_id: frame.session_id.clone(),
                    event_type: frame.kind.as_str().to_string(),
                    reason,
                    error: seal,
                });
                answer.spoke = true;
            }
            SealVerdict::NotOnScreen => {
                // 话**不在**气泡里，这已经证明了。它们仍然必须到达那个聊，
                // 而且用一份**收尾不可能已经花掉**的预算。
                answer.budget = answer.budget.fallback(Instant::now());
            }
        }
        answer
    }
}

impl Outbound {
    /// 上游 `sealRelayedRound`：替一个够不到它的副本收掉一轮，而**这一轮不在这里时什么都不做**
    /// —— 那正是它是自己一种帧、而不是一条正文为空的回答的原因。
    pub async fn seal_relayed_round(&self, frame: &RelayFrame) {
        if frame.task_id.is_empty() {
            return;
        }
        let Some(session_id) = parse_uuid(&frame.session_id) else {
            return;
        };
        let (turn, _) = self
            .rounds()
            .take(session_id, &by_task(frame.task_id.clone()))
            .await;
        let Some(turn) = turn.filter(|turn| turn.has_bubble) else {
            // 这里没有气泡。没什么可封、也没什么可说：这个结束在帧存在之前就是无声的，
            // 而它继续无声。
            return;
        };
        let copy = copy_for(turn.handle.locale);
        let text = if frame.seal_reason == SEAL_REASON_CANCELLED {
            copy.stream_cancelled
        } else {
            wordless_seal_copy(turn.handle.locale, frame.carries_files)
        };
        if let Some(error) = self.finish_stream(&turn.handle, text).await {
            // 一次封不上的收尾把气泡留在原处。把那些话作为一条消息说出来是**回答**的本地路径
            // 的动作，而提问者正在等那条回答；这一条没人在等，而在这里推一条正是这种帧存在要避免的
            // 那条普通消息。
            tracing::warn!(
                task_id = frame.task_id.as_str(),
                reason = frame.seal_reason.as_str(),
                error = %error,
                "wecom relay: could not seal a routed round's ending"
            );
        }
    }
}

/// 上游在 `deliverRelayed` 里用帧上的四个字段直接建 `attachmentTarget`（一次**纯**解析，
/// 不打库：帧驮着的 installation / chat / `chat_type` / session 就是地址本身）。
#[must_use]
fn attachment_target(frame: &RelayFrame) -> Option<super::super::outbound::AttachmentTarget> {
    Some(super::super::outbound::AttachmentTarget {
        installation_id: parse_uuid(&frame.installation_id)?,
        chat_id: frame.chat_id.clone(),
        chat_type: frame.chat_type,
        session_id: frame.session_id.clone(),
    })
}
