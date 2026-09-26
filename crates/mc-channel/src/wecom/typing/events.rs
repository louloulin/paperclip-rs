//! **三次 run 结束各自怎么收尾**：绑定一次 run 到等它的气泡、一次失败（气泡里说话，或者作为一条
//! 普通消息），以及一次取消（只封气泡）。
//!
//! 本文件是 `typing.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分加上门 ⑩ 的
//! 800 行硬限（逐条清单见 `docs/32` §38 的 D9）。

use std::time::{Duration, Instant};

use mc_core::id::Id;

use super::ports::{failure_text, parse_task_id, OriginVerdict, TaskEvent, TaskRouting};
use super::TypingIndicator;
use crate::wecom::outbound::DeliveryLookup;
use crate::wecom::outbound::{empty_address, TaskAddressOutcome};
use crate::wecom::relay::SEAL_REASON_CANCELLED;
use crate::wecom::stream_store::{by_task, RoundAddress, RoundTurn};
use crate::wecom::ws_frame::aibot_chat_type_from_channel;
use mc_core::channel::message::ChatType;

/// 这台订阅者可以花在**别人的** goroutine 上的全部数据库预算（上游 `taskLookupTimeout`，
/// **800ms**）：会话回查、绑定行、以及 origin 门**合起来**。
///
/// 上游逐字：总线是**同步**的 —— 一个 `task:failed` 订阅者跑在**发布那个事件**的 goroutine 上，
/// 而一次清扫 tick 不是我们能拿来等的（池子很忙的时候答案要多久就多久）。收尾那条路有它自己、
/// 更长的一份预算（[`crate::wecom::outbound::FALLBACK_SEND_TIMEOUT`] 与 10s 的收尾窗口）。
///
/// # 本仓的形态差异（登记 `docs/32` §38 的 D7）
///
/// 本仓没有进程内总线 ⇒ 这三个入口由宿主**显式**调用（`chat:done` 那两条同款）。但这条预算仍然
/// 照抄并且**有用**：宿主可能从一次 HTTP 请求或一个定时器里调进来，而 800ms 是"我们愿意替调用方
/// 等数据库多久"的那个数。它落成一个**显式的截止时刻**（而不是一个 `tokio::time::timeout`）：
/// 下面两次读都要把它带下去，而 [`TaskQueries`](super::ports::TaskQueries) 的端口**没有**接受
/// 截止时刻的形参 ⇒ 本片用 `Instant::now() + TASK_LOOKUP_TIMEOUT` 只做**诊断与文档**的界，
/// 真正的界在宿主的调用点。
pub const TASK_LOOKUP_TIMEOUT: Duration = Duration::from_millis(800);

// =====================================================================
// task:queued —— 把一次 run 绑到等它的气泡
// =====================================================================

impl TypingIndicator {
    /// 上游 `handleTaskQueued`：把一个 run 绑到那个正等着一个运行的气泡。
    ///
    /// 它是"引擎把 flush 造出来的 task id 告诉它"的**替代品**：上游的 `Router` 对自己造出来的 run
    /// 什么都不说，所以 run 自己说出来 —— 在每一条入队路径**已经**在发布的那个事件上。
    ///
    /// 上游逐字：**这里的一切都是一次内存 map 操作，这是刻意的**。两个发布方（去抖 flush 的
    /// `FinalizeChatTaskEnqueue`、以及为等媒体的那一轮延迟的清扫器）都把事件发布在**发布者自己的**
    /// goroutine 上，所以这里的一次读库会被记在入队那个人的账上 —— daemon 的 HTTP 处理器、一次清扫
    /// tick —— 而一个慢池子会拖住**入队本身**。它需要的那两件事都在事件上。
    ///
    /// 两种 run 会到达这个订阅者，而**只有一种**拥有气泡：
    ///
    /// - 一次**会话里的** run 有 `chat_session_id` 而 `issue_id` 是 **NULL**（`CreateChatTask` 就是
    ///   这么写的）⇒ payload 上空的 `issue_id` 说的就是"这是会话里的一个轮次"；
    /// - 其余一切 —— `/issue`、autopilot、web UI 的一次重跑 —— 都带一个 issue id。那些 run 在**完全
    ///   别的地方**回答，而其中一个若拿走了一个 `WeCom` 气泡，就会用一个房间里没人问过的答案封掉一个
    ///   陌生人的问题，并且让真正的那一轮无处可落。
    pub fn handle_task_queued(&self, event: &TaskEvent) {
        let Some(streams) = self.streams.as_ref() else {
            return;
        };
        if !event.is_chat_turn() {
            return;
        }
        let Some(session_id) = event.session_id() else {
            return; // 一次 issue / autopilot 的 run：没有会话、也没有气泡
        };
        let task_id = event.task_id();
        if task_id.is_empty() {
            return;
        }
        streams.bind_next(session_id, task_id);
    }

    // =================================================================
    // task:failed
    // =================================================================

    /// 上游 `handleTaskFailed`：一次 run 死了 —— 还有气泡就在气泡里说，没有就作为一条普通消息说。
    ///
    /// `task:failed` 的两个发布方都会在 task 行有时盖上 `chat_session_id`（`service.taskEvent`
    /// 与清扫器自己的信封），所以会话通常**就在事件上**；而 `sessionFor` 那次从 task 行读是给
    /// 一个**今天两个发布方都不产出**的 payload 形态留的兜底。它留着，是因为一个一直转圈没人收的气泡
    /// 是一次**没人报告**的失败。
    ///
    /// 气泡不是全部：一次丢掉气泡的轮次（运行中途重启、服务端拒掉开场帧、一条跑完自己窗口的流）
    /// 仍然有一个提问的人，而这条告知是 `WeCom` 唯一产出的"那次运行没跑通"。
    ///
    /// 一次失败的**重发**（清扫器重新发布它、第二个发布方）是第二条告知。这里不记"我已经说过了"：
    /// 气泡是缓存，而缓存不记账。
    pub async fn handle_task_failed(&self, event: &TaskEvent) {
        let Some(streams) = self.streams.as_ref() else {
            return;
        };
        // 一次**平台已经在重试**的尝试不是一个结局（上游逐字：`FailTask` 仍然为它发布
        // `task:failed` —— web 卡片得清掉 —— 并盖上 `retry_pending` 让消费者安静；`taskFailedFields`
        // 连错误文本都扣住了，dingtalk 的出站也已经照办）。在这里封气泡会告诉用户"这次没跑通"，
        // 而那次尝试的替代品**已经**在队列里，于是重试的答案会落在一个**已经宣告失败**的气泡下面。
        //
        // ⇒ 气泡留着，而那一轮交出死掉那次尝试的 id、回去等一个 run（`retry_unbind`）。这正是让
        // clone 自己的 `task:queued` 落**在这里**、而不是下一个问题的气泡上的东西 —— clone 是一条
        // **新** task 行、新 id，而事件上没有别的东西把它与一次全新的轮次区分开。
        if event.retry_pending() {
            if let Some(session_id) = event.session_id() {
                streams.retry_unbind(session_id, event.task_id());
            }
            return;
        }
        if self.deliveries.is_none() && !streams.holding() {
            // 什么都没有归档，也没有找到某个聊的办法：没有理由为**别人的** run 去读一行。
            return;
        }
        let Some(session_id) = self.session_for(event).await else {
            return; // 一次 issue / autopilot 的 run：没有会话、也没有气泡
        };
        let task_id = event.task_id();
        // 无论下面那几道门怎么判，这次 run 都已经结束了。**在任何一处提前返回之前**把它从"等一个
        // 气泡的 run"队列里丢掉，否则下一个问题的气泡会把自己绑到一次**已经结束**的 run 上、转圈而
        // 再也没有东西能封它。
        streams.forget(session_id, task_id);

        // 这个问题是在哪儿问的？**在取气泡之前**问，因为一道放在取**之后**的门已经用一次 web run 的
        // 结局封掉了一个 `WeCom` 轮次的气泡了。回答路径把自己的门也排成这样（`outbound.rs` 的
        // `processEvent` 里那道门）。
        //
        // 🔴 上游逐字：**开着的轮次不是来源证明**，而它过去被当成一个。按引擎交下来的批次身份，一个
        // 被绑上的轮次**由构造**就是 `WeCom` 的；而按 `task:queued` 绑定的现在不是了：在这个同一个
        // 会话上**在 Multica 里**敲的一个问题会发布一个带着**同样** `chat_session_id`、**同样** NULL
        // `issue_id` 的事件（`CreateChatTask` 给每一个会话任务都写 NULL）⇒ 浏览器那次 run 可以拿着这
        // 个房间的气泡。为它跳过这道门会把一次 web run 的错误文本放到聊天里每个人眼前。
        //
        // 所以门对**每一次**结束都跑。顺序是让它便宜的原因：`task:failed` 对部署里**每一个** run 都会
        // 发 —— slack 的、lark 的、dingtalk 的、web UI 的 —— 而投递行**先**读，于是一次没有 `WeCom`
        // 路线的 run 永远到不了第二次读。
        let mut bound: Option<RoundAddress> = None;
        if task_id.is_empty() {
            // 没有 id 就命名不了任何行、也命名不了任何批次。`originOf` 会拒它并说出来 —— 那是本进程
            // 吞掉的那条告知可见的那一半。
            let _ = self.origin_verdict(Some(session_id), task_id).await;
            return;
        }
        match self.address_for_task(task_id).await {
            (_, TaskRouting::Silent) => return,
            (_, TaskRouting::NoRow) => match self.origin_verdict(Some(session_id), task_id).await {
                OriginVerdict::NotOurs => {
                    self.release_refused_round(session_id, task_id);
                    return;
                }
                OriginVerdict::Unknown => return, // `origin_of` 已经记过是哪一次读失败
                OriginVerdict::Ours => {}
            },
            (address, TaskRouting::Ours) => bound = Some(address),
        }

        let turn = self.take_round(session_id, task_id).await;
        if let Some(turn) = turn.filter(|turn| turn.has_bubble) {
            let text = failure_text(event, turn.handle.locale);
            self.write_closing(session_id, &turn.handle, &text, "task failed")
                .await;
            return;
        }
        // 没有可以写进去的气泡：那一轮从没被画出来，或者它的流已经跑完窗口了。话仍然要去问过的那个聊。
        let address = if let Some(address) = bound {
            address
        } else {
            let (address, routing) = self.address_for_task(task_id).await;
            if routing != TaskRouting::Ours {
                return;
            }
            address
        };
        // 第三个实参是**这个目的地**的身份：单聊里那个 chat id **就是**那个人的 userid（上游逐字），
        // 所以 1:1 的档案回查恰好拿得到要问的那个人。
        let locale = self.locale_for(
            address.installation_id.unwrap_or_else(Id::nil),
            address.chat_type,
            &address.chat_id,
        );
        let text = failure_text(event, locale);
        let _ = self
            .say_as_plain_message(
                Some(Instant::now() + crate::wecom::outbound::FALLBACK_SEND_TIMEOUT),
                session_id,
                &address,
                task_id,
                &text,
            )
            .await;
    }

    // =================================================================
    // task:cancelled
    // =================================================================

    /// 上游 `handleTaskCancelled`：封住一次**用户自己停掉**的运行的气泡。
    ///
    /// 取消是一个终态，它**不**发布 `chat:done`、也**不**发布 `task:failed` ⇒ 没有这一条，那个气泡会
    /// 一直转到服务端的窗口在它身上用完。一个开着好几轮的会话每一次被取消的 run 拿到一帧收尾帧，
    /// 各在**它自己**的气泡上，因为那一轮是按 `bind_next` 绑上去的 task id 匹配的。
    ///
    /// 与失败不同，它在没有气泡在册时**不**去找一个地址。`StreamFailed` 是 `WeCom` 唯一产出的
    /// "那次运行没跑通"，所以失败值得去追一个地址；而一次取消是**用户**做的，去追它会把一次
    /// "取消全部任务"的点击变成那个 agent 服务的**每一个**聊里的一条消息 —— 包括 `WeCom` 从来
    /// 没画过气泡的那些会话。
    pub async fn handle_task_cancelled(&self, event: &TaskEvent) {
        let Some(streams) = self.streams.as_ref() else {
            return;
        };
        // **在册的任何东西**，不只是画过的。一次绑到某个 run 的轮次、而它的开场帧还在路上时还没有
        // 气泡；在这里返回会把它留在开着的名单上，而那帧随后落地就会画出一个**再没有收尾**的转圈 ——
        // 取消是这次 run 产出的**最后**一个事件。把那**一轮**退休掉则让那次迟到的画变成 no-op。
        let Some(session_id) = self.session_for(event).await else {
            return;
        };
        // 与失败路径**同一道门**、**同一理由**：一个按 `task:queued` 绑上去的轮次可能属于一个在这个
        // 会话上**在 Multica 里**敲的问题，而"这次处理已取消"封掉那个房间的气泡会结束一个房间还在等
        // 的问题 —— 为一次**它里面没人做**的取消。**在取之前**问，于是一次拒绝把那一轮留在原地而不是
        // 已经把它移走了。
        let task_id = event.task_id();
        if !streams.holding() {
            // 🔴 上游逐字：**这里根本没有轮次**并不能说明轮次不存在。气泡在**画它的那个副本**上，而
            // 有 N 个副本时一次取消落**在离租约之外**的概率约 `(N-1)/N`。在这里返回会让那个气泡在
            // 协议剩下的窗口里一直宣称工作在进行 —— 一个**说着假话**的转圈，而 main 那边只是安静。
            //
            // 上面那道门不为这件事而被咨询：一帧封印帧在**没有**轮次时什么都不做，而一个轮次之所以
            // 存在，只因为这个 adapter 为一个**在房间里问的**问题画了它。
            self.relay_seal(session_id, task_id, SEAL_REASON_CANCELLED);
            return;
        }
        match self.origin_verdict(Some(session_id), task_id).await {
            OriginVerdict::NotOurs => {
                self.release_refused_round(session_id, task_id);
                return;
            }
            OriginVerdict::Unknown => return,
            OriginVerdict::Ours => {}
        }

        let turn = self.take_round(session_id, task_id).await;
        let Some(turn) = turn.filter(|turn| turn.has_bubble) else {
            // 这里有轮次，只是不是这一个 —— 气泡在兄弟副本上，于是 `holding()` 那一支的同一套推理
            // 成立。
            self.relay_seal(session_id, task_id, SEAL_REASON_CANCELLED);
            return;
        };
        if self.senders.is_none() {
            return;
        }
        let text = crate::wecom::strings::copy_for(turn.handle.locale)
            .stream_cancelled
            .to_string();
        self.write_closing(session_id, &turn.handle, &text, "task cancelled")
            .await;
    }

    // =================================================================
    // 共享的两件小事
    // =================================================================

    /// 上游 `sessionFor`：找出一条 task 生命周期事件背后的那个 chat session。
    ///
    /// 它通常**就在事件上**（两个发布方都会在 task 行有时盖上它）。从 task 行读是兜底，见
    /// [`TypingIndicator::handle_task_failed`] 的文档。
    pub(crate) async fn session_for(&self, event: &TaskEvent) -> Option<Id> {
        if let Some(session_id) = event.session_id() {
            return Some(session_id);
        }
        let tasks = self.tasks.as_ref()?;
        let task_id = event.task_id();
        let id = parse_task_id(task_id)?;
        match tasks.get_agent_task(id).await {
            Ok(Some(task)) => task.chat_session_id,
            Ok(None) | Err(_) => None,
        }
    }

    /// 上游 `take`：把这一轮的气泡取出来（`None` = 本进程没有在册的轮次 / 没有匹配上的）。
    pub(crate) async fn take_round(&self, session_id: Id, task_id: &str) -> Option<RoundTurn> {
        self.streams.as_ref()?;
        let (turn, _) = self.rounds().take(session_id, &by_task(task_id)).await;
        turn
    }

    /// 上游 `addressForTask`：从一次 run 的投递行上读出它在哪个聊被问的 —— 与回答在 `outbound.rs`
    /// 里做的是**同一次**读。
    ///
    /// 一次**没有** `WeCom` 投递行的 run 不是我们能替它说话的：这个订阅者看到的是一次共享总线上的
    /// **每一个**失败的 run，包括 slack 的与 web UI 的。这让它同时是**归属测试**与地址来源，而这正是
    /// [`TypingIndicator::handle_task_failed`] 在花任何东西读 task 行**之前**先问它的原因。
    ///
    /// 三格而不是两格（上游逐字）：把前两格压成一个正是让 origin 门够不着的原因 —— "没有行" 被读成
    /// "不是我们的" 然后返回，而一个在 Multica 里敲的 run 按设计**没有**行。
    pub(crate) async fn address_for_task(&self, task_id: &str) -> (RoundAddress, TaskRouting) {
        let Some(deliveries) = self.deliveries.as_ref() else {
            return (empty_address(), TaskRouting::NoRow);
        };
        let Some(id) = parse_task_id(task_id) else {
            return (empty_address(), TaskRouting::NoRow);
        };
        match task_address(deliveries.as_ref(), id).await {
            Err(error) => {
                tracing::warn!(
                    task_id,
                    error = %error,
                    "wecom typing: cannot find the chat a failed run belongs to"
                );
                (empty_address(), TaskRouting::Silent)
            }
            Ok(outcome) if !outcome.ours => (empty_address(), TaskRouting::NoRow),
            Ok(outcome) if outcome.skip.is_some() || !outcome.addr.is_known() => {
                (empty_address(), TaskRouting::Silent)
            }
            Ok(outcome) => (outcome.addr, TaskRouting::Ours),
        }
    }
}

/// 上游 `taskAddress`：把一个 task id 翻成 `WeCom` 聊的地址（两次读）。
///
/// ⚠️ **本片落的是上游那个**自由函数**的形状**，而 M7-17 把同一套规则落成了
/// `Outbound::task_address`（它要宽的 `OutboundQueries`，而本片只拿得到窄的 [`DeliveryLookup`]）。
/// 两条规则**必须**给出同一个答案 —— `typing/tests.rs` 的 `the_addressing_rule_agrees_with_outbound`
/// 就是钉这件事的等价比对，而把两处收敛成一处的票登记在 `docs/32` §38 的 D10 与交接 H1（拿宽查询的
/// 那一个不是本片能改的写集）。
///
/// # Errors
///
/// 读库失败（调用方按 `Silent` 处理：一次够不着的数据库不是"这次 run 属于我们"的证据）。
pub(crate) async fn task_address(
    deliveries: &dyn DeliveryLookup,
    task_id: Id,
) -> Result<TaskAddressOutcome, String> {
    let Some(delivery) = deliveries.task_delivery(task_id).await? else {
        // **一行都没有，与一行命名别的平台，不是同一个答案**（上游逐字）。
        return Ok(TaskAddressOutcome {
            addr: empty_address(),
            skip: None,
            ours: false,
        });
    };
    if delivery.channel_type != crate::wecom::types::CHANNEL_TYPE {
        return Ok(TaskAddressOutcome {
            addr: empty_address(),
            skip: None,
            ours: true,
        });
    }
    let binding = delivery.binding();
    let Some(installation) = deliveries
        .installation_record(binding.installation_id)
        .await?
    else {
        return Err("load installation: no such installation".to_string());
    };
    if !installation.is_active() {
        // 触发与回复之间被撤销。
        return Ok(TaskAddressOutcome {
            addr: empty_address(),
            skip: Some(crate::wecom::outcome::SkipReason::InstallationInactive),
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
