//! **一条回复到底有没有到用户那儿** —— 按运维能数的单位记账（上游
//! `internal/integrations/wecom/outbound_outcome.go`，**439 行**）。
//!
//! - **写者**：M7-17（`LUM-1782` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：出站路径上每一个"转完了却没把话说给用户"的分支，过去都
//!   是一条裸 `return nil` 或一句孤零零的 WARN。那是 GH #7215 / #6890 的形态：答案在 Multica
//!   的 transcript 里，`WeCom` 的聊天里一片安静，而服务端的证据要么是**一句不带原因**的日志，
//!   要么什么都没有。几个**不同**的成因产出**同一个**无法区分的症状 ⇒ 无论是我们还是一个部署
//!   的运维都说不清是哪一个响了，修法只能是猜。
//!
//! # 于是每个分支都报出自己的名字
//!
//! **计数器是持久的那一半**（永远 +1），**日志级别是判断的那一半**：一个人该为此行动的
//! 原因打 WARN，健康部署里寻常的原因打 DEBUG 并且只被当成速率来读。
//!
//! 上游这里有一条**已经作废的**中间态：`dropReason` 曾经分成"该行动 / 不该行动"两类，
//! 后来两个寻常结局搬去了 `skipReason` ⇒ `actionable()` 现在恒为真、`dropped` 里的
//! DEBUG 分支成了**不可达**代码。本仓**照抄**这个恒真（含它的注释留下的理由），
//! 见 `docs/32` §34 的 D3 —— 收敛（或删掉那个死分支）不在本片写集里。
//!
//! # 原因集是**封闭**的，故意
//!
//! 它是 metric label，而开放的 label 就是 `forbiddenMetricLabels` 存在的理由（无界基数）。
//! 所以本文件给每个枚举配一个字面量表 + `ALL`：加一个原因必须同时改这三处，
//! 漏了会被 [`DropReason::as_str`] 的穷尽匹配拦住编译。
//!
//! # 四个单位，别混
//!
//! | 单位 | 计数 | 为什么不能并 |
//! | --- | --- | --- |
//! | **回复**（到达） | `record_outbound_delivered` | 没有它就是分母缺失："今天没丢"与"今天没流量"分不开 |
//! | **回复**（该到没到） | `record_outbound_dropped` | 每个原因都让某个人在 `WeCom` 里等一个不会来的答案 |
//! | **回复**（根本不欠） | `record_outbound_skipped` | 把 web UI 提问的答案算成失败的 `WeCom` 投递，会让寻常的 web 使用看起来像一次故障 |
//! | **文件**（一条一个） | `record_attachment_*` | "文字到了、文件没到"是一条做到了的回复 + 一个失败了的附件；合成一个数只能往一个方向撒谎 |
//!
//! 外加一对"结局未知"（`record_outbound_unconfirmed`）：帧**上了线**而消息可能已经在用户眼前。
//! 它不能并进 `dropped` —— 那会用大概率成功的发送去抬高一个"确定失败率"，而按丢弃率取告警的
//! 运维会为**发生了**的投递被叫起来。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 本文件只记**原因标签**与**会话/task 标识**：没有正文、没有凭据、没有任何平台密钥。
//! 上游把发送错误整条塞进日志（`"error", err`），本仓照做 —— 但 `SenderError`
//! （`ws_sender/error.rs`）的错误值里**没有**凭据（`Api` 只带 `errcode`/`cmd`），
//! 这条事实由 M7-16 的用例钉住。

use crate::wecom::metrics::{or_nop_metrics, Metrics};
use crate::wecom::ws_sender::SenderError;

use super::outbound::{Outbound, OutboundError};

// =====================================================================
// 丢弃的原因（封闭集）
// =====================================================================

/// 一条回复**该到而没到**的原因（上游 `dropReason`）。
///
/// 封闭集：它是 metric label，见模块文档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DropReason {
    /// 上游 `dropNoConnection` —— **没有活的 WebSocket**扛这条回复。
    ///
    /// 两种情况到这里：没有中继时，本进程里没有（多副本部署下它与"租约握在别的副本手里"
    /// 无法区分）；有中继时，这条回复**确实**被路由到了每一个副本、而**没有一个**副本握着
    /// 连接 —— 那是 `SELF_HOSTING.md` 描述的那段残余窗口，由路由它的那个副本在
    /// `RelayOutbound::watch_outcomes` 里、从"没人领的 claim"上记**一次**。
    NoLiveConnection,
    /// 上游 `dropTaskMissing` —— 这次完成所属的 task 解析不出来：事件上没有 id，
    /// 或者那一行在它的结局在飞的路上被回收了。
    TaskMissing,
    /// 上游 `dropPlatformRefused` —— `WeCom` 用一个非零 `errcode` 回答了发送。
    /// **说出来了的**拒绝：帧超预算、机器人已经不在那个聊里、租户被限流。
    PlatformRefused,
    /// 上游 `dropTransport` —— 一个字节都没到平台，且原因是**本地**的。
    /// 写自己失败了、写之前的某次查表失败了、或者这次投递自己的预算在它拿到线上的一轮之前
    /// 就用完了。三者的共同点正是运维需要的事实：失败在 socket 的**我们这一侧**，
    /// 所以谁都没看见任何东西。
    Transport,
    /// 上游 `dropAttachmentNotAdmitted` —— 太多投递已经在跑或在排队，所以这次投递被**削减**了。
    ///
    /// 它出现在**两个**单位上，意思各不相同：在文件计数器上它是"一个不会被发出去的文件"；
    /// 在回复计数器上它**只**在"文件**就是**那条回复"时出现（一次空完成，因为有什么绑在了
    /// 它上面才走到了投递），而那里它的意思是用户**什么都没得到**。文字已经落地的回复在这道
    /// 门之前就结算了，永远到不了这里。
    AttachmentNotAdmitted,
}

impl DropReason {
    /// 全部取值（封闭集的**单一**来源；加一个原因只改这里与 [`Self::as_str`]）。
    pub const ALL: [Self; 5] = [
        Self::NoLiveConnection,
        Self::TaskMissing,
        Self::PlatformRefused,
        Self::Transport,
        Self::AttachmentNotAdmitted,
    ];

    /// wire 取值（上游常量的字面量，**逐字**；它是 metric label，不许改）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoLiveConnection => "no_live_connection",
            Self::TaskMissing => "task_missing",
            Self::PlatformRefused => "platform_refused",
            Self::Transport => "transport_error",
            Self::AttachmentNotAdmitted => "attachment_not_admitted",
        }
    }

    /// 解回枚举（诊断与用例用；**别**用默认值吞掉未知取值）。
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == raw)
    }

    /// 上游 `actionable` —— 这个原因该不该有人去看。
    ///
    /// 上游现在是**恒真**：两个寻常结局搬去 [`SkipReason`] 之后，剩下每一个都该看。
    /// 本仓照抄恒真 + 保留调用点（`dropped` 的 DEBUG 分支），见模块文档与 `docs/32` §34 D3。
    #[must_use]
    pub fn actionable(self) -> bool {
        true
    }
}

// =====================================================================
// 根本不会送的原因（封闭集，另一个计数器）
// =====================================================================

/// 一条"这个 adapter **本来就不会**送"的完成（上游 `skipReason`）。
///
/// 与 [`DropReason`] 分开、配另一个计数器，因为"我们选择不发这个"与"我们欠这个而且失败了"
/// 回答的是不同的问题，而**只有一个**是事故。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// 上游 `skipOriginNotChannel` —— 这一轮是在 Multica 的 web UI 上问的，而那个会话源自
    /// `WeCom` ⇒ 它的答案只属于 Multica。健康部署里寻常，且是忙碌工作区上这个计数器**最大**
    /// 的来源。
    OriginNotChannel,
    /// 上游 `skipInstallationInactive` —— 安装在一次触发与它的回复之间被撤销了。
    /// **不是**投递失败：已经没有可以投递的安装了，而机器人也从用户那一侧消失了。
    InstallationInactive,
    /// 上游 `skipNothingToSay` —— 一次空完成、且不带文件。这里从来就没有过一条消息。
    NothingToSay,
}

impl SkipReason {
    /// 全部取值（封闭集的单一来源）。
    pub const ALL: [Self; 3] = [
        Self::OriginNotChannel,
        Self::InstallationInactive,
        Self::NothingToSay,
    ];

    /// wire 取值（上游常量的字面量，**逐字**）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OriginNotChannel => "origin_not_channel",
            Self::InstallationInactive => "installation_inactive",
            Self::NothingToSay => "nothing_to_say",
        }
    }

    /// 解回枚举。
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == raw)
    }
}

// =====================================================================
// 分类
// =====================================================================

/// 上游 `unconfirmedReason`：一次发送失败的**结局未知**吗（消息可能已经在用户眼前），
/// 还是确定的？
///
/// 返回 `Some(label)` = 未知（`label` 是运维读的那一个字面量）；`None` = **确定**。
///
/// 调用方**必须**先问它再问 [`classify_drop`]：把"未知"归档成"丢了"，会让运维去重发一条
/// 用户可能已经拿到的消息。
///
/// # 上游那张表在 Rust 里逐格摊开
///
/// | 上游 | 本变体 | 结果 |
/// | --- | --- | --- |
/// | `errAckTimeout` | [`SenderError::AckTimeout`] | `ack_timeout` |
/// | `errWriteAttempted` | [`SenderError::WriteAttempted`] | `write_attempted` |
/// | `errNotAttempted`（**先于**下面的 ctx 分支） | [`SenderError::NotAttempted`] / [`SenderError::ChatBusy`] | `None`（确定） |
/// | `context.Canceled` / `DeadlineExceeded` | [`SenderError::AckAbandoned`] | `interrupted` |
/// | 其余 | 其余变体 | `None`（确定） |
///
/// 🔴 **上游自己在这两格上不闭合**（照抄，登记 `docs/32` §34 的 R2）：`errStreamAckTimeout`
/// 在 `provablyNotSent` 里被判成"可能已上 socket"（`false`），却**不在** `unconfirmedReason`
/// 的表里 ⇒ 落进 default 被读成**确定**、进而被 [`classify_drop`] 记成 `transport_error`。
/// 本仓不改上游行为（一个片顺手改分类会让别的片的读数漂），只把它标出来。
#[must_use]
pub fn unconfirmed_send_reason(error: &SenderError) -> Option<&'static str> {
    match error {
        SenderError::AckTimeout => Some("ack_timeout"),
        SenderError::WriteAttempted { .. } => Some("write_attempted"),
        // 上游逐字：**先于**下面那条 ctx 分支。每一个 not-attempted 的失败都包着结束它的
        // `ctx.Err()`，所以它们在 Go 里**同时**匹配 ctx 分支；这里说的是"等在一个帧存在之前
        // 就结束了"（这个聊的轮次没来，或者进入 request 时预算就已经到点）⇒ 它们是这条路径上
        // **确定**（而不是未知）的 ctx 失败。"interrupted" 会让运维不去重发一条谁也没发出的消息。
        SenderError::AckAbandoned { .. } => Some("interrupted"),
        // `NotAttempted` / `ChatBusy` 与其余一切一样都是**确定**的（见上面那条上游注释）。
        _ => None,
    }
}

/// [`unconfirmed_send_reason`] 的 outbound 层版本：把包装过的错误剥回发送错误。
///
/// [`OutboundError::Send`] 之外的一切都是**确定的**（它们发生在任何字节出线之前）。
#[must_use]
pub fn unconfirmed_reason(error: &OutboundError) -> Option<&'static str> {
    match error {
        OutboundError::Send(send) => unconfirmed_send_reason(send),
        _ => None,
    }
}

/// 上游 `unconfirmedSealReason`：一次**收尾帧**的结局未知时，运维读的那一个 label。
///
/// 它是 [`unconfirmed_reason`] 的镜像，外加收尾自己那一格：帧写了、也重试了，而判决
/// **永远没回来**（`seal_unacked`）。
#[must_use]
pub fn unconfirmed_seal_reason(error: &SenderError) -> &'static str {
    unconfirmed_send_reason(error).unwrap_or("seal_unacked")
}

/// 上游 `classifyDrop`：把一个**确定**的发送失败翻成运维读的原因。
///
/// "确定"是调用方的义务：先问 [`unconfirmed_reason`]。
#[must_use]
pub fn classify_drop(error: &OutboundError) -> DropReason {
    match error {
        OutboundError::NoLiveConnection => DropReason::NoLiveConnection,
        OutboundError::Send(SenderError::Api { .. }) => DropReason::PlatformRefused,
        _ => DropReason::Transport,
    }
}

/// 上游 `worseDropReason`：一条回复的**多个**文件因不同原因失败时的聚合规则。
///
/// 优先级：说出来了的拒绝 > 本地传输失败 > 一段永远没回来的判决 —— 按"每个事实对
/// 内容为什么没到有多具体"排。稳定且写下来，是为了让"多文件回复的原因"是一条**规则**，
/// 而不是循环顺序的偶然。
#[must_use]
pub fn worse_drop_reason(a: DropReason, b: DropReason) -> DropReason {
    fn rank(reason: DropReason) -> u8 {
        match reason {
            DropReason::PlatformRefused => 3,
            DropReason::Transport => 2,
            _ => 0,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// [`worse_drop_reason`] 在**另一个单位**（结局未知）上的孪生。
///
/// 按每条真相能确立多少排：`ack_timeout` 最具体（帧上了线，只缺判决）；`write_attempted`
/// 次之（本地报了失败，而对端可能仍握着字节）；`interrupted` 说得最少（等待结束了，
/// 而它结束在哪儿从这里不可知）。
#[must_use]
pub fn worse_unconfirmed_reason(a: &str, b: &str) -> &'static str {
    fn rank(reason: &str) -> u8 {
        match reason {
            "ack_timeout" => 3,
            "write_attempted" => 2,
            "interrupted" => 1,
            _ => 0,
        }
    }
    let (a, b): (&'static str, &'static str) = (intern(a), intern(b));
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// 把三个已知 label 变成 `'static` 字面量（未知取值归 `interrupted` 之外的空串）。
///
/// 存在的理由：聚合结果要被当成**封闭 label** 用（进 metric），而不能是调用方传进来的
/// `String`。未知取值（调用方自己造的）**不**透传 —— 它会让 label 无界。
fn intern(reason: &str) -> &'static str {
    match reason {
        "ack_timeout" => "ack_timeout",
        "write_attempted" => "write_attempted",
        // 未知取值（调用方自己造的）**不**透传 —— 它会让 label 无界。
        _ => "interrupted",
    }
}

// =====================================================================
// 记账（`impl Outbound`）
// =====================================================================

impl Outbound {
    /// 上游 `mx`：拿到汇，或者一个 no-op。
    #[must_use]
    pub fn mx(&self) -> &'static dyn Metrics {
        or_nop_metrics(self.metrics())
    }

    /// 上游 `dropped`：记一条**该到而没到**的回复。
    ///
    /// 每个分支都自带一条日志，级别说清"有没有人该行动"（见模块文档与 D3）。
    ///
    /// **刻意不是错误返回值**：其中几个分支是在"本来就不是这个 adapter 该回答的"事件上到达的，
    /// 把它们变成错误会改变 [`Outbound::handle_chat_done`] 的调用方（以及一堆既有用例）
    /// 对"这里没什么可做"的理解。
    pub fn dropped(
        &self,
        session_id: &str,
        event_type: &str,
        reason: DropReason,
        error: Option<&OutboundError>,
    ) {
        self.dropped_for(session_id, event_type, reason, error);
    }

    /// [`Outbound::dropped`] 给"手上只有会话 id、事件已经没了"的调用方（附件路径，
    /// 它在事件消失很久之后还在跑）用。
    pub fn dropped_for(
        &self,
        session_id: &str,
        event_type: &str,
        reason: DropReason,
        error: Option<&OutboundError>,
    ) {
        self.mx().record_outbound_dropped(reason.as_str());
        if reason.actionable() {
            tracing::warn!(
                reason = reason.as_str(),
                chat_session_id = session_id,
                event = event_type,
                error = error.map(ToString::to_string).as_deref(),
                "wecom outbound: reply not delivered"
            );
            return;
        }
        // 不可达（`actionable()` 恒真）；留着是上游的形态，见 D3。
        tracing::debug!(
            reason = reason.as_str(),
            chat_session_id = session_id,
            event = event_type,
            "wecom outbound: reply not delivered"
        );
    }

    /// 上游 `unconfirmed`：记一条结局**未知**的回复。WARN —— 正在决定要不要重发的人需要知道
    /// 这**不是**一次失败。
    pub fn unconfirmed(
        &self,
        session_id: &str,
        event_type: &str,
        reason: &str,
        error: Option<&OutboundError>,
    ) {
        self.unconfirmed_for(session_id, event_type, reason, error);
    }

    /// [`Outbound::unconfirmed`] 的 `For` 版本。
    pub fn unconfirmed_for(
        &self,
        session_id: &str,
        event_type: &str,
        reason: &str,
        error: Option<&OutboundError>,
    ) {
        let reason = intern(reason);
        self.mx().record_outbound_unconfirmed(reason);
        tracing::warn!(
            reason,
            chat_session_id = session_id,
            event = event_type,
            error = error.map(ToString::to_string).as_deref(),
            "wecom outbound: reply delivery unconfirmed"
        );
    }

    /// 上游 `attachmentUnconfirmed`：记一个结局未知的**文件**。
    pub fn attachment_unconfirmed(&self, reason: &str, error: Option<&OutboundError>) {
        let reason = intern(reason);
        self.mx().record_attachment_unconfirmed(reason);
        tracing::warn!(
            reason,
            error = error.map(ToString::to_string).as_deref(),
            "wecom outbound: attachment delivery unconfirmed"
        );
    }

    /// 上游 `skipped`：记一条这个 adapter **本来就不会**送的完成。永远 DEBUG。
    pub fn skipped(&self, session_id: &str, reason: SkipReason) {
        self.skipped_for(session_id, reason);
    }

    /// [`Outbound::skipped`] 的 `For` 版本。
    pub fn skipped_for(&self, session_id: &str, reason: SkipReason) {
        self.mx().record_outbound_skipped(reason.as_str());
        tracing::debug!(
            reason = reason.as_str(),
            chat_session_id = session_id,
            "wecom outbound: reply not owed to WeCom"
        );
    }

    /// 上游 `attachmentDelivered`：记一个到达的用户可见**文件**（一条一个）。
    ///
    /// 上游逐字：没有"回复"与"文件"两个单位，一个"只到了文字、文件没到"的回复就会在两个
    /// 计数器上同时说谎。
    pub fn attachment_delivered(&self) {
        self.mx().record_attachment_delivered();
    }

    /// 上游 `attachmentDropped`：记一个没到达的文件。
    pub fn attachment_dropped(&self, reason: DropReason, error: Option<&OutboundError>) {
        self.mx().record_attachment_dropped(reason.as_str());
        tracing::warn!(
            reason = reason.as_str(),
            error = error.map(ToString::to_string).as_deref(),
            "wecom outbound: attachment not delivered"
        );
    }

    /// 上游 `attachmentShed`：一次**准入**判决 —— 一次投递尝试在查表之前就被拒了。
    /// 不是文件计数：那一刻没人知道这一轮带零个文件还是五个。
    pub fn attachment_shed(&self) {
        self.mx().record_attachment_delivery_shed();
    }

    /// 上游 `delivered`：记一条**到达**用户那里的回复。没有它，丢弃计数器就没有分母，
    /// 而"今天没丢"与"今天没流量"分不开 —— 那正是 #7215 被报上来的那种安静。
    pub fn delivered(&self) {
        self.mx().record_outbound_delivered();
    }

    /// 上游 `recordSend`：给一次**完成**的文本发送归档。
    ///
    /// 它是那条映射的**唯一**定义，而且必须保持唯一：把 agent 的话送到 `WeCom` 用户眼前的
    /// 两条路径（产出这次完成的那个副本上的 `process_event`，以及握着 socket 的那个副本上的
    /// `deliver_relayed`）**过去**把一个**部分**发送分类得不一样 —— 同一个用户可见事件，
    /// 第一段进了聊、第二段被拒，在单副本部署上算 `outbound_delivered`、一旦回复走了中继就算
    /// `outbound_dropped` ⇒ **一个部分投递会不会惊动任何人，取决于哪个副本恰好握着租约**。
    ///
    /// **部分发送算送达**，并 WARN 出没落地的部分。另一种读法（记一次丢弃）会让运维去重发一条
    /// 用户正在读的答案，而重发会把第一段又打一遍。两个计数器都不适合"大部分到了"；
    /// 这一个是不招来有害动作的那个。
    ///
    /// 调用方**不得**再把这个错误返回给另一层去重新分类：一次发送动一个计数器
    /// （上游 `errOutcomeRecorded`）。
    pub fn record_send(
        &self,
        session_id: &str,
        event_type: &str,
        error: Option<&SenderError>,
    ) -> crate::wecom::outbound::Recorded {
        use crate::wecom::outbound::Recorded;
        match error {
            None => {
                self.delivered();
                Recorded::Delivered
            }
            Some(SenderError::PartiallySent { cause }) => {
                tracing::warn!(
                    chat_session_id = session_id,
                    event = event_type,
                    cause = cause.as_str(),
                    "wecom outbound: only part of a long answer reached the chat"
                );
                self.delivered();
                Recorded::Delivered
            }
            Some(error) => {
                let error = OutboundError::Send(error.clone());
                if let Some(reason) = unconfirmed_reason(&error) {
                    self.unconfirmed_for(session_id, event_type, reason, Some(&error));
                    return Recorded::Unconfirmed(reason);
                }
                let reason = classify_drop(&error);
                self.dropped_for(session_id, event_type, reason, Some(&error));
                Recorded::Dropped(reason)
            }
        }
    }
}

#[cfg(test)]
mod tests;
