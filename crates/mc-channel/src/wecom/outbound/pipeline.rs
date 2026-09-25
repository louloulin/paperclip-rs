//! `outbound` 的**判决链**：上游 `processEvent` / `deliverAnswer` / `sendAsMessage` /
//! `taskAddress` / `routeFrame` / `relaySeal` / `finishStream` 那一半。
//!
//! 本文件是 `outbound.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

use std::sync::Arc;
use std::time::Instant;

use mc_core::channel::message::ChatType;
use mc_core::id::Id;

use crate::engine::commands::task_input_is_channel_ingested;
use crate::wecom::stream_store::{by_task, NothingToSay, RoundAddress, RoundTurn};
use crate::wecom::types::CHANNEL_TYPE;
use crate::wecom::ws_frame::{aibot_chat_type_from_channel, has_visible_char};
use crate::wecom::ws_sender::SenderError;

use super::events::{parse_uuid, uuid_string, ChatDone, DeliveryBudget};
use super::ports::{DeliveryLookup, OutboundQueries};
use super::{
    classify_seal, AnswerOutcome, Outbound, OutboundError, Processed, Recorded, SealVerdict,
};

impl Outbound {
    /// 上游 `processEvent`：一条 `chat:done` 的整条判决链。
    pub async fn process_event(
        &self,
        budget: DeliveryBudget,
        event: &ChatDone,
    ) -> Result<Processed, OutboundError> {
        let Some(session_id) = parse_uuid(&event.chat_session_id) else {
            // issue / autopilot 任务不带 chat_session。
            return Ok(Processed::ignored());
        };
        let Some(task_id) = event.parsed_task_id() else {
            self.dropped_for(
                &event.chat_session_id,
                &event.event_type,
                super::super::outcome::DropReason::TaskMissing,
                None,
            );
            return Ok(Processed::recorded(Recorded::Dropped(
                super::super::outcome::DropReason::TaskMissing,
            )));
        };
        let task = match self.q.get_agent_task(task_id).await {
            Ok(Some(task)) => task,
            // 它的完成还在飞的时候被取消并删掉了。
            Ok(None) => {
                self.dropped_for(
                    &event.chat_session_id,
                    &event.event_type,
                    super::super::outcome::DropReason::TaskMissing,
                    None,
                );
                return Ok(Processed::recorded(Recorded::Dropped(
                    super::super::outcome::DropReason::TaskMissing,
                )));
            }
            Err(message) => return Err(OutboundError::lookup("load agent task", message)),
        };
        let ingested = self
            .q
            .task_has_channel_ingested_messages(task_id)
            .await
            .map_err(|message| OutboundError::lookup("classify task input origin", message))?;
        if !task_input_is_channel_ingested(task.chat_input_task_id, ingested) {
            // **把气泡还回去**：一次在 Multica 里打的 run 可以占着这个聊的轮次（它绑在
            // `task:queued` 上，而一个 chat task 的事件里没有任何东西能区分两者）⇒ 在这里直接返回
            // 而不释放，会把那一轮留在一个**永远不会收尾**的 run 上：提问者看着气泡转到平台自己
            // 结束它，而他们自己的回答找不到轮次、降级成一条普通消息。失败与取消门的释放同款。
            if let Some(streams) = self.streams.as_ref() {
                streams.release_round(session_id, event.task_id());
            }
            self.skipped_for(
                &event.chat_session_id,
                super::super::outcome::SkipReason::OriginNotChannel,
            );
            return Err(OutboundError::Skipped {
                reason: super::super::outcome::SkipReason::OriginNotChannel,
            });
        }

        // agent 这一轮有没有产出文件：在封口之前就定下来，因为封口要做什么取决于它。
        // 它读的每一样都已经在手 ⇒ 一个没有对象存储的部署一分钱查询都不花。
        let carries_files = self.may_carry_attachments(event);

        // take 在存储的锁下把这一轮移走，这才让两个收尾器为同一个 run 赛跑时只产出一帧收尾。
        // 它有没有找到气泡就是 `deliver_answer` 全部需要知道的：气泡是**缓存**，
        // 一轮没有气泡就走普通路径。
        let (turn, _) = self
            .rounds()
            .take(session_id, &by_task(event.task_id()))
            .await;
        let processed = self
            .deliver_answer(budget, event, task_id, turn, event.content(), carries_files)
            .await?;
        if processed.answer.routed {
            // 握着 socket 的那个副本拥有这一轮剩下的部分，**含文件**
            // （`RelayFrame::carries_files` 就是它知道的途径）。从这里也发一遍会让每个附件
            // 在聊里出现两次。一条已发布的帧由中继欠一个结局 —— 没人领时它自己记那次丢弃。
            return Ok(processed);
        }
        // 然后才是 agent 与话一起产出的东西，作为**它自己的**消息 —— `WeCom` 的一条回复
        // 不能内联带文件。它去的地方就是回答刚去过的地方（气泡或普通消息），而那是这一轮
        // 确立的、唯一属于提问那个聊的地址。
        let carries_the_reply = !processed.answer.spoke;
        let addr = processed.answer.addr.clone();
        self.deliver_attachments(event, &addr, carries_the_reply);
        Ok(processed)
    }

    /// 上游 `deliverAnswer`：把 agent 的回答写到这一轮**还能被够到**的地方，
    /// 按用户更愿意收到的顺序。
    ///
    /// 气泡优先：这一轮在问题到达时开了一个，而这个功能的全部意义就是回答**就地替换**它。
    /// 其余一切都是以任务投递行命名的聊里的一条普通消息。
    pub async fn deliver_answer(
        &self,
        budget: DeliveryBudget,
        event: &ChatDone,
        task_id: Id,
        turn: Option<RoundTurn>,
        content: &str,
        carries_files: bool,
    ) -> Result<Processed, OutboundError> {
        let mut content = content.to_string();
        let mut budget = budget;
        if let Some(turn) = turn.filter(|turn| turn.has_bubble) {
            // 屏幕上的一个气泡必须用话结束。一次空完成是合法结局（agent 没有要补的），
            // 但一个转不完的圈不是 ⇒ 文案替它开口。于是当 agent 什么都没说却产出了文件时，
            // 沉默**根本**不是这一轮的结束：那些文件紧接着以它们自己的消息到达，
            // 所以一个写着"这轮没有需要回复的内容"的气泡会和屏幕上的下一件东西打架。
            let text = if has_visible_char(&content) {
                content.clone()
            } else {
                super::super::relay::wordless_seal_copy(turn.handle.locale, carries_files)
                    .to_string()
            };
            let seal = match (self.streams.as_ref(), self.stream_sender()) {
                (Some(streams), Some(sender)) => {
                    streams.seal(sender, &turn.handle, &text).await.err()
                }
                // 没有流面 ⇒ 收尾没得写：等同于"证明话不在屏幕上"（退回普通消息）。
                _ => Some(SenderError::NotAttempted),
            };
            match classify_seal(seal.as_ref()) {
                SealVerdict::OnScreen => {
                    self.delivered();
                    return Ok(Processed::recorded_at(
                        Recorded::Delivered,
                        AnswerOutcome {
                            addr: turn.handle.address(),
                            spoke: true,
                            routed: false,
                        },
                    ));
                }
                SealVerdict::Unknown => {
                    // `Unknown` 蕴含 `Some(error)`（`None` 只可能是 `OnScreen`）；
                    // 写成 map_or 而不是 expect，是为了让"这个分支永远有一个错误"这件事
                    // 由类型的形状说，而不是由一次 panic 说。
                    let reason = seal.as_ref().map_or(
                        "seal_unacked",
                        super::super::outcome::unconfirmed_seal_reason,
                    );
                    let wrapped = seal.clone().map(OutboundError::Send);
                    self.unconfirmed(
                        &event.chat_session_id,
                        &event.event_type,
                        reason,
                        wrapped.as_ref(),
                    );
                    return Ok(Processed::recorded_at(
                        Recorded::Unconfirmed(reason),
                        AnswerOutcome {
                            addr: turn.handle.address(),
                            spoke: true,
                            routed: false,
                        },
                    ));
                }
                SealVerdict::NotOnScreen => {
                    // 话没在屏幕上，这已经证明了 ⇒ 把它作为一条消息说出去，
                    // 而且用一份**收尾不可能已经花掉**的预算（`fallback`）：收尾的重试可以把
                    // 调用方的预算花掉大半，而"退回普通消息"是这条路径存在的全部意义 ——
                    // 它不能落在一份已经花光的预算上。
                    content = text;
                    budget = budget.fallback(Instant::now());
                }
            }
        }
        if !has_visible_char(&content) {
            // 没有气泡可封、也没什么可说。就是这里了：一次没有话也没有文件的完成从来不是一条
            // 消息，而没有气泡在等这样一条消息。
            if !carries_files {
                // **没什么可说也仍然是一个结束**，而且它仍然要到达那个气泡。走到这里意味着
                // 本副本上没找到轮次；离了租约，轮次在兄弟副本上，静默返回会让它在协议的窗口
                // 剩下的时间里一直转 —— 为了一轮已经结束的 run。
                self.relay_seal(
                    event,
                    task_id,
                    super::super::relay::SEAL_REASON_NO_REPLY,
                    false,
                );
                self.skipped_for(
                    &event.chat_session_id,
                    super::super::outcome::SkipReason::NothingToSay,
                );
                return Err(OutboundError::Skipped {
                    reason: super::super::outcome::SkipReason::NothingToSay,
                });
            }
            // agent 什么都没说但产出了文件，而那些仍然要到达那个聊 —— 由 `send_as_message`
            // 给它命名；没有话要驮时它一句都不发，并交回文件要去的地址。
        }
        self.send_as_message(budget, event, task_id, &content).await
    }

    /// 上游 `sendAsMessage`：把一条回答推到这一轮**被准入时**的那个聊，给一个再没有气泡可写的
    /// 轮次（运行中途重启、流过了窗口、服务端拒掉的帧）。它交回话去了哪儿，也就是后面那些文件
    /// 要去的地方。
    pub async fn send_as_message(
        &self,
        budget: DeliveryBudget,
        event: &ChatDone,
        task_id: Id,
        content: &str,
    ) -> Result<Processed, OutboundError> {
        let resolved = self.task_address(task_id).await?;
        if let Some(skip) = resolved.skip {
            self.skipped_for(&event.chat_session_id, skip);
            return Err(OutboundError::Skipped { reason: skip });
        }
        let addr = resolved.addr;
        if !addr.is_known() {
            // 这一轮没有记下的路由，或者那一行命名的是别的平台。没什么可发，也没人可以被记。
            return Err(OutboundError::NothingToSay(NothingToSay));
        }
        let Some(senders) = self.senders.as_ref() else {
            return Err(OutboundError::SenderRegistryMissing);
        };
        let Some(installation_id) = addr.installation_id else {
            // `is_known()` 已经保证了它是 `Some`；这条分支只是把那个不变式写成类型层面的
            // 事实（而不是一次 panic），同时给出一个诚实的错误。
            return Err(OutboundError::NoLiveConnection);
        };
        let Some(sender) = senders.get(installation_id) else {
            // 放弃之前：这条回复可能只是**在错的副本上**产出的。交给握着 socket 的那个。
            //
            // 账由投递它的那个副本记，这里不记 —— 所以一条被路由又投递成功的回复只出现一次。
            // 一条在**每个**副本都处于重连中时被路由的回复没人读、也没人记；那段窗口就是这个
            // 路径刻意不去解决的持久性问题。
            if self.route_frame(
                &super::super::relay::RelayFrame::reply(
                    uuid_string(installation_id),
                    addr.chat_id.clone(),
                    addr.chat_type,
                    content,
                    event.task_id(),
                    &event.message_id,
                    &event.workspace_id,
                    &event.chat_session_id,
                ),
                &super::super::relay::relay_event_id(&event.event_type, task_id),
            ) {
                tracing::debug!(
                    installation_id = %uuid_string(installation_id),
                    chat_session_id = event.chat_session_id.as_str(),
                    "wecom outbound: routed to the replica holding the socket"
                );
                return Ok(Processed::recorded_at(
                    // 中继的账由中继的 `watch_outcomes` 结（它是唯一能在事后结算它的所有者）。
                    Recorded::Nothing,
                    AnswerOutcome {
                        addr,
                        spoke: false,
                        routed: true,
                    },
                ));
            }
            // 本副本这个安装没有活的 WS。两个成因：(1) 监管器丢了租约或正在重连 —— 瞬时的，
            // 用户的下一条入站消息会到达重连后的循环；(2) 多副本部署下租约握在**另一个**副本手里，
            // 于是它永远不可能从这里投递（见文件头的单副本约束）。两种都**不该**缓冲 ——
            // 一条 socket 回来时回复已经陈旧了 —— 所以我们把它交给调用方的 WARN，而不是静默丢弃。
            return Err(OutboundError::NoLiveConnection);
        };
        // 话在先 —— 而且只在有话说的时候。一次空完成到这里只是因为有一个文件绑在这一轮上，
        // 而那个文件前面一条空的 markdown 消息是用户得划过去的噪音。
        if !has_visible_char(content) {
            return Ok(Processed::recorded_at(
                Recorded::Nothing,
                AnswerOutcome {
                    addr,
                    spoke: false,
                    routed: false,
                },
            ));
        }
        let error = sender
            .send_text(&addr.chat_id, addr.chat_type, content, budget.as_deadline())
            .await
            .err();
        // 记在这里而不是返回出去，是为了让这次发送与中继的那次走**同一个**映射
        // （`record_send`；上游 #8344）。
        let recorded = self.record_send(&event.chat_session_id, &event.event_type, error.as_ref());
        if let Some(error) = error {
            if !matches!(error, SenderError::PartiallySent { .. }) {
                // 回答一点都没落地。文件本身不是答案 ⇒ 这一轮到此结束。
                //
                // 是 `OutcomeRecorded` 而**不是** `error`：上面的 `record_send` 已经归档了这次发送，
                // 而 `handle_chat_done` 会给 `process_event` 返回的每一个错误记账。同一个拒绝会
                // 落两次计数器（实测过：`platform_refused = 2`）。
                return Err(OutboundError::OutcomeRecorded { recorded });
            }
        }
        // `PartiallySent` 仍然算说了话：回答的一部分在读者屏幕上，而它下面的文件不是那条回复。
        Ok(Processed::recorded_at(
            recorded,
            AnswerOutcome {
                addr,
                spoke: true,
                routed: false,
            },
        ))
    }

    /// 上游 `taskAddress`：一轮的话要去的那个聊，按它**自己的**投递行解出来。
    ///
    /// 三个结局，从两个结果一起读出来。一个 `is_known()` 的地址就是该写的地方。一个零地址、
    /// **没有** skip 原因是"这个 adapter 本来就不会回答"的一轮 —— 没有投递行，或者那一行是别的
    /// 平台的 —— 那是总线（每个渠道都在往它上面发）上那个安静而寻常的情况。一个零地址、
    /// **有**原因的一轮，则是一轮**本来是我们的**、直到安装被撤销：那个值得计数。
    ///
    /// 地址来自 `channel_task_delivery` 而不是会话当前的绑定：`/new` 与 `/clear` 会把一个会话
    /// 重指，而跨过其中之一的、已经产出的回答属于**当初问的那个聊**。气泡路径本来就是这么路由的
    /// （它的句柄驮着问题进来的那个地址）⇒ 这个 adapter 的两条路径都在**它被问的地方**回答。
    pub async fn task_address(&self, task_id: Id) -> Result<TaskAddressOutcome, OutboundError> {
        let lookup: Arc<dyn OutboundQueries> = Arc::clone(&self.q);
        let delivery = DeliveryLookup::task_delivery(&lookup, task_id)
            .await
            .map_err(|message| OutboundError::lookup("lookup task delivery", message))?;
        let Some(delivery) = delivery else {
            // **一行都没有，与一行命名别的平台，不是同一个答案**，第三个返回值就是让它们分开的
            // 东西。在 Multica 里打的 run 按设计**没有**行（`EnqueueChatTask` 不写外部投递快照）
            // ⇒ "没有行"是唯一一个让 origin 还开着的答案，也是唯一一个调用方该继续花读去查的。
            return Ok(TaskAddressOutcome {
                addr: empty_address(),
                skip: None,
                ours: false,
            });
        };
        if delivery.channel_type != CHANNEL_TYPE {
            return Ok(TaskAddressOutcome {
                addr: empty_address(),
                skip: None,
                ours: true,
            });
        }
        let binding = delivery.binding();
        let Some(installation) =
            DeliveryLookup::installation_record(&lookup, binding.installation_id)
                .await
                .map_err(|message| OutboundError::lookup("load installation", message))?
        else {
            return Err(OutboundError::lookup(
                "load installation",
                "no such installation",
            ));
        };
        if !installation.is_active() {
            // 触发与回复之间被撤销。
            return Ok(TaskAddressOutcome {
                addr: empty_address(),
                skip: Some(super::super::outcome::SkipReason::InstallationInactive),
                ours: true,
            });
        }
        Ok(TaskAddressOutcome {
            addr: RoundAddress {
                installation_id: Some(installation.id),
                chat_id: binding.channel_chat_id,
                chat_type: aibot_chat_type_from_channel(
                    ChatType::from_str_opt(&binding.chat_type).unwrap_or(ChatType::P2p),
                ),
            },
            skip: None,
            ours: true,
        })
    }

    /// 上游 `routeFrame`：把一帧交给中继，没有中继时报 `false`。
    ///
    /// **这里的 nil 检查是因为字段是接口。** 上游过去是 `*RelayOutbound`，它的 `publish` 以
    /// `if r == nil` 开头 ⇒ 穿过空指针的调用是安全的，而调用点依赖这一点却没有说出来。
    /// 一个 nil 接口没有可以跑那道守卫的接收者 ⇒ 把字段放宽成接口会**静默**移掉三个调用点
    /// 正踩着的安全。一处检查，下一个调用点就漏不掉。
    pub fn route_frame(&self, frame: &super::super::relay::RelayFrame, event_id: &str) -> bool {
        match self.relay.as_ref() {
            Some(relay) => relay.publish(frame, event_id),
            None => false,
        }
    }

    /// 上游 `relaySeal`：请握着这一轮的那个副本把它收掉。按**轮次归属**路由 ⇒ 不需要地址。
    pub fn relay_seal(&self, event: &ChatDone, task_id: Id, reason: &str, carries_files: bool) {
        let id = uuid_string(task_id);
        if id.is_empty() {
            return;
        }
        self.route_frame(
            &super::super::relay::RelayFrame::seal(
                reason,
                &id,
                &event.chat_session_id,
                carries_files,
            ),
            &id,
        );
    }

    /// 上游 `mayCarryAttachments`：这一轮值不值得那几次查表 —— 即使 agent 什么都没说。
    /// 它检查的每一样都已经在手，所以一个没有对象存储的部署（或者一个不命名任何消息的事件）
    /// 一分钱查询都不花。
    #[must_use]
    fn stream_sender(&self) -> Option<&dyn crate::wecom::stream_store::StreamSender> {
        self.senders.as_ref()?.stream_sender()
    }

    /// 上游 `finishStream`：把回答写进气泡并封口，走存储的 `seal` —— 收尾帧重试策略**唯一**
    /// 的所在地。这里的失败对回复不致命（它意味着调用方退回一条新消息），所以只记一条日志，
    /// 而且带上唯一能解释它的那个细节：这条流是**救不回来**了（过了窗口、坏的 `req_id`），
    /// 还是 socket 只是眨了一下眼。
    ///
    /// 两种结局都在 `sendersRegistry.recordEnding` 里计数（每一个气泡收尾器都走它 ——
    /// 这一个与打字指示的那一个）。之所以要计数：从外面看两者无法区分 —— 用户两种情况下都拿到
    /// 回答，而没有人会来报"我盯着的气泡变成了一条单独的消息"。一个**完全不工作**的气泡
    /// （`WeCom` 那一侧对流帧的改动、跑偏的 `req_id` 约定）只会表现为
    /// `stream_fell_back` 爬上来追平 `stream_finished`，别处看不出来。
    ///
    /// `senders` 与 `streams` 在这里非空：一轮**只有**通过它们才有气泡。
    pub(crate) async fn finish_stream(
        &self,
        handle: &crate::wecom::stream_store::StreamHandle,
        text: &str,
    ) -> Option<SenderError> {
        match (self.streams.as_ref(), self.stream_sender()) {
            (Some(streams), Some(sender)) => streams.seal(sender, handle, text).await.err(),
            _ => Some(SenderError::NotAttempted),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAddressOutcome {
    /// 话要去的地址（零值 = 没地方去）。
    pub addr: RoundAddress,
    /// 一个值得计数的跳过原因（安装已被撤销）。
    pub skip: Option<super::super::outcome::SkipReason>,
    /// 这一轮**是不是我们的**（有投递行）；`false` 时调用方不该再花读。
    pub ours: bool,
}

/// 零地址（上游 `roundAddress{}`）。
#[must_use]
pub fn empty_address() -> RoundAddress {
    RoundAddress {
        installation_id: None,
        chat_id: String::new(),
        chat_type: 0,
    }
}
