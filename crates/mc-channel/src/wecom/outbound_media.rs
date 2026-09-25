//! `outbound_media.go`（676 行）的本地落点：**把 agent 产出的文件送出去**。
//!
//! - **写者**：M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §35 的 D1）。
//!   本片同时补上 M7-17 的**交接 H3**：`AttachmentDelivery` 的实现（上游
//!   `sendAttachments` / `sendAttachment` / `tellUser` / `readObject`）。
//! - **上游定位**（文件头逐字）：agent 那一侧**已经**存在而且是平台无关的 —— 它跑
//!   `multica attachment upload <path>`，文件落进对象存储，而 `CompleteTask` 把那一行绑到它刚
//!   写完的那条助手消息上。缺的是**最后一跳**：那条绑定下游的一切都假定对面是一个浏览器里的聊天
//!   窗口，所以一个 `WeCom` 会话被告知它根本收不了文件。
//!
//! 三件事决定了这里的形状：
//!
//! 1. **回答先走，永远如此**。一次上传是好几十兆、好几十次往返、而且它可能失败；agent 写下的那句
//!    话**不能**被排在一次上传后面，也**不能**被它搭进去。所以这件事在回答出去之后跑，用**自己**
//!    的任务、自己的预算，而它最坏的结果是多一行"有个文件没送出去"。
//! 2. **文件是它自己的一条消息**。长连接没有 `msg_item`，所以没有任何东西能嵌进一条回复里 ——
//!    "带附件地回答"必然是两条消息。
//! 3. **`WeCom` 按 `msgtype` 校验字节**。一个被声明成图片的 `.pptx` 会被拒而不是被转换，而每个
//!    种类有自己的尺寸帽 ⇒ "这个文件该叫什么"是一个**判断**，不是一次查表。
//!
//! # 与上游的两点形态差异（登记 `docs/32` §35 的 D11）
//!
//! 1. **端口同步 + 桥成 async**：`AttachmentDelivery::deliver` 是 async（本片的写集勘误 D12 把它
//!    从同步改成 async，理由见那里），而对象存储那条读取在本仓是**同步**端口
//!    （与 `dingtalk::media::MediaStorage` 同款）⇒ `read_object` 直接同步读，不桥。
//! 2. **逐文件记账落在本文件**：`Outbound` 的那几个记账方法是 `pub` 的，但它们要一个
//!    `&Outbound`，而这段工作在**脱离任务**里跑、拿不到借用 ⇒ 本文件的
//!    [`AttachmentAccounting`] 用**同一份** `Metrics` 与**同样的字面量**重述那几条记账
//!    （label 与日志消息逐字取自 `outcome.rs`），交接 H4 给了收敛的路径。
//!
//! # 凭据面
//!
//! 对象的 URL **不进日志**：它是一个"谁持有就能拿到这个文件"的地址（上游逐字）。日志里只有
//! `attachment_id` / `content_type` / `size_bytes` 与判决。

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mc_core::id::Id;

use crate::wecom::media_upload::{
    send_media, upload_media, MediaMsgType, MediaSend, MediaUploadError, OutboundMedia,
    MAX_MEDIA_UPLOAD_BYTES,
};
use crate::wecom::metrics::{or_nop_metrics, Metrics};
use crate::wecom::outbound::{
    AttachmentDelivery, AttachmentGates, AttachmentTarget, OutboundError,
};
use crate::wecom::outcome::{classify_drop, unconfirmed_send_reason, DropReason, SkipReason};
use crate::wecom::ws_sender::WsSender;

use super::media_download::clean_media_filename;
use super::media_ingest::{base_content_type, media_extension};

// =====================================================================
// 用户读到的三句话（上游逐字，三个分别对应三种**真的**不同的实情）
// =====================================================================

/// 上游 `mediaSendFailedText`：**我们知道**它没到。确定的，因为对一次后来其实是投递成功的失败
/// 断言"确定失败"，正是一个用户开始无视这类通知的方式。
pub const MEDIA_SEND_FAILED_TEXT: &str = "⚠️ 有文件没能发出来，我这边保留着，需要的话我再试一次。";

/// 上游 `mediaSendUnknownText`：帧出去了、判决没回来，所以文件**可能已经在聊里**了。
///
/// 措辞要对**两种**结局都成立：它既不能对一个正看着那个文件的人说"没发出去"，也不能对一个
/// 没收到的人说"已发送"。它还要解释为什么不自动重发 —— 那是显而易见的下一个问题，而答案是
/// 一次重复无法撤回。
pub const MEDIA_SEND_UNKNOWN_TEXT: &str = "⚠️ 有文件我没收到企业微信的送达回执，可能已经发到了、也可能没有。我不会自动重发，免得发重了；你那边没看到的话说一声，我再发一次。";

/// 上游 `mediaLookupFailedText`：失败在我们这一侧、而且发生在问题被回答之前 —— 我们读不出这条
/// 回复上绑了什么，所以**不知道**有没有文件。在这里什么都不说，就是一个用户在等一件从未被尝试
/// 过的事。
pub const MEDIA_LOOKUP_FAILED_TEXT: &str =
    "⚠️ 我这边没查到这条回答带没带文件，所以要是有，这次没发出来。需要的话我再试一次。";

/// 一次回答的**全部**附件投递（读每个对象、上传、发送）的上限（上游 `attachmentBudget` 5min）。
///
/// 宽裕是有理由的：一个 20 MiB 的文件、每次两片地发四十来块，不快；而且没有任何东西在等它。
pub const ATTACHMENT_BUDGET: Duration = Duration::from_secs(300);

/// 每种上传材料 `WeCom` 施加的尺寸帽：图片 10MB、视频 10MB、语音 2MB。
///
/// 超过某个种类帽的字节**照常上路** —— 作为一个**文件**（四种里帽最宽的那个）—— 因为一张用户
/// 打得开的文件卡胜过一个被服务端拒掉的图片。
pub const MAX_OUTBOUND_IMAGE_BYTES: usize = 10 << 20;
pub const MAX_OUTBOUND_VOICE_BYTES: usize = 2 << 20;
pub const MAX_OUTBOUND_VIDEO_BYTES: usize = 10 << 20;

// =====================================================================
// 判决
// =====================================================================

/// 上游 `deliveryState`：一个文件试过之后我们**真的**知道什么。
///
/// 三个取值，因为两个取值的那个版本在两个方向上都错：一次判决从未回来的发送被报成确定失败，
/// 而那个文件很可能正躺在聊里；而那些根本没走到 socket 的本地失败谁也没被告知。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    /// `WeCom` 承认了这次发送。唯一不需要对用户说话的状态：文件就是他们看到的那个东西。
    Delivered,
    /// 什么都没到、也不可能到。要么文件从未变成一个 `media_id`（上传被拒、或者对象读不出来），
    /// 要么发送本身被拒了。原则上可以安全重试，也可以安全地描述成一次失败。
    DefinitelyFailed,
    /// 发送帧出去了、判决没回来。那条消息**可能**在聊里。
    ///
    /// 这个状态**绝不能**重试：同一条 `media_id` 发两次会让对方看到两次那个文件，而没有东西能
    /// 撤回它。
    Unknown,
}

impl DeliveryState {
    /// 日志里的名字，用的是代码推理时用的那套词，好让运维读到一行就能分辨一次未确认的发送与
    /// 一次被拒的发送，而不用知道哪个 `errcode` 是哪个意思。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::DefinitelyFailed => "definitely_failed",
            Self::Unknown => "unknown",
        }
    }
}

/// 上游 `sendOutcome`：读一次媒体推送的错误，看它说了什么关于那条消息的话。
///
/// 要紧的**唯一**那个区分：一次拒绝是 `WeCom` 在回答，而一个没回来的答案**不是一个答案**。
/// `AckTimeout` 意味着帧到了 socket 而判决从未回来，这留下"那条消息可能已投递"。
///
/// 传输失败也落在那一边，而且理由是同一个而不是一个更弱的：`WriteAttempted` 标记的是"已经进了
/// 写调用"之后才产生的错误；过了那一点帧可能已经在对面了。只有写之前失败（编帧错误、连接拒绝
/// 了一个截止时刻）才是**可证明没投递**。
#[must_use]
pub fn send_outcome(error: Option<&MediaUploadError>) -> DeliveryState {
    match error {
        None => DeliveryState::Delivered,
        // 上传阶段的一切失败都**没有**产出 `media_id` ⇒ 那条消息从未被寻址到那个聊，
        // 包括 finish 这一步自己的 ack 丢了的时候。文件确定不在。
        Some(MediaUploadError::Send(sender)) => {
            if unconfirmed_send_reason(sender).is_some() {
                DeliveryState::Unknown
            } else {
                DeliveryState::DefinitelyFailed
            }
        }
        Some(_) => DeliveryState::DefinitelyFailed,
    }
}

// =====================================================================
// 端口
// =====================================================================

/// 上游 `mediaObjectStore`：这条路径要的对象存储的那一片 —— 附件行驮着对象的 URL，而这两个方法
/// 把它变回字节。
///
/// **同步**端口（差异 1）：与 `dingtalk::media::MediaStorage` 同款，理由是同一个 —— adapter 的
/// 摄入路径在同步侧。
pub trait MediaObjectStore: Send + Sync {
    /// URL → 对象 key（**纯函数**；不是本部署存的对象 ⇒ 空串）。
    fn key_from_url(&self, raw_url: &str) -> String;

    /// 打开一个对象。
    ///
    /// # Errors
    ///
    /// 人可读描述（**不得**含凭据）。
    fn get_reader(&self, key: &str) -> Result<Box<dyn Read + Send>, String>;
}

/// `attachments` 表的一行的本面投影（上游 `db.Attachment` 的五列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    pub id: Id,
    /// 对象 URL（**不进日志**）。
    pub url: String,
    pub filename: String,
    pub content_type: String,
    /// 记下来的字节数。它是元数据，而对象才是真相 ⇒ [`read_object`] 会再查一次它**真的**读到了
    /// 多少。
    pub size_bytes: i64,
}

/// `attachments` 表的读取口（上游 `o.q.ListAttachmentsByChatMessage`）。
///
/// 本仓**没有** `mc-repos` 的 attachments 模块（那张表属 W8/M8）⇒ 端口在本文件声明，由宿主装配。
#[async_trait]
pub trait AttachmentQueries: Send + Sync {
    /// 一条聊天消息上绑着的附件。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn list_attachments_by_chat_message(
        &self,
        chat_message_id: Id,
        workspace_id: Id,
    ) -> Result<Vec<AttachmentRow>, String>;
}

/// 上游 `sendersRegistry.get` 在本面的用法：拿到那个安装**活着的**发送者。
///
/// 与 [`crate::wecom::outbound::SenderLookup`] 是**同一个**进程级注册表，只是本面只要这一个方法
/// 就够（M7-17 的端口已经在，这里不再声明一遍 —— 那两个端口都归 M7-20 的实现满足）。
pub trait MediaSenderLookup: Send + Sync {
    /// 活着的 socket，或者 `None`（正在重连 / 本副本没有它）。
    fn live_sender(&self, installation_id: Id) -> Option<Arc<WsSender>>;
}

// =====================================================================
// 记账（交接 H4）
// =====================================================================

/// 脱离任务里那几条记账（上游 `Outbound` 在附件路径上调的那几个方法）。
///
/// 见模块文档差异 2：这段工作在脱离任务里跑、拿不到 `&Outbound`，所以用**同一份** `Metrics` 与
/// **同样的字面量**把 `outcome.rs` 那几条重述一遍。收敛路径（交接 H4）：把 `Outbound` 放进
/// `Arc`、投递从 `Weak` 取，然后把这几条搬回去。
#[derive(Clone, Copy)]
pub struct AttachmentAccounting {
    metrics: Option<&'static dyn Metrics>,
}

impl std::fmt::Debug for AttachmentAccounting {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentAccounting")
            .field("metrics", &self.metrics.is_some())
            .finish()
    }
}

impl AttachmentAccounting {
    /// 装上一份汇（`None` = 一个 no-op）。
    #[must_use]
    pub fn new(metrics: Option<&'static dyn Metrics>) -> Self {
        Self { metrics }
    }

    fn mx(&self) -> &'static dyn Metrics {
        or_nop_metrics(self.metrics)
    }

    /// 上游 `attachmentDelivered`：记一个到达的用户可见**文件**（一条一个）。
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

    /// 上游 `attachmentUnconfirmed`：记一个结局未知的**文件**（不是失败：另有一个计数器）。
    pub fn attachment_unconfirmed(&self, reason: &'static str, error: Option<&OutboundError>) {
        self.mx().record_attachment_unconfirmed(reason);
        tracing::warn!(
            reason,
            error = error.map(ToString::to_string).as_deref(),
            "wecom outbound: attachment delivery unconfirmed"
        );
    }

    /// 上游 `delivered`：记一条到达用户那里的回复。
    pub fn delivered(&self) {
        self.mx().record_outbound_delivered();
    }

    /// 上游 `droppedFor`：记一条该到而没到的回复（附件路径手上只有会话 id）。
    pub fn dropped_for(
        &self,
        session_id: &str,
        event_type: &str,
        reason: DropReason,
        error: Option<&OutboundError>,
    ) {
        self.mx().record_outbound_dropped(reason.as_str());
        tracing::warn!(
            reason = reason.as_str(),
            chat_session_id = session_id,
            event = event_type,
            error = error.map(ToString::to_string).as_deref(),
            "wecom outbound: reply not delivered"
        );
    }

    /// 上游 `unconfirmedFor`：记一条结局未知的回复。
    pub fn unconfirmed_for(
        &self,
        session_id: &str,
        event_type: &str,
        reason: &'static str,
        error: Option<&OutboundError>,
    ) {
        self.mx().record_outbound_unconfirmed(reason);
        tracing::warn!(
            reason,
            chat_session_id = session_id,
            event = event_type,
            error = error.map(ToString::to_string).as_deref(),
            "wecom outbound: reply delivery unconfirmed"
        );
    }

    /// 上游 `skippedFor`：记一条这个 adapter **本来就不会**送的完成。
    pub fn skipped_for(&self, session_id: &str, reason: SkipReason) {
        self.mx().record_outbound_skipped(reason.as_str());
        tracing::debug!(
            reason = reason.as_str(),
            chat_session_id = session_id,
            "wecom outbound: reply not owed to WeCom"
        );
    }
}

// =====================================================================
// 投递
// =====================================================================

/// 上游 `sendAttachments` / `sendAttachment` / `tellUser` / `readObject` 的实现。
pub struct WecomAttachmentDelivery {
    objects: Arc<dyn MediaObjectStore>,
    queries: Arc<dyn AttachmentQueries>,
    senders: Arc<dyn MediaSenderLookup>,
    /// 与 `Outbound` 上的那一个**共用状态**（`AttachmentGates` 内部是 `Arc`）⇒ `admitted` 与
    /// `pending` 两个计数器真的是同一对（上游那两个计数器本来就是同一个结构体的字段）。
    gates: AttachmentGates,
    accounting: AttachmentAccounting,
}

impl std::fmt::Debug for WecomAttachmentDelivery {
    /// 手写：端口都是 trait 对象 ⇒ 只列存在性。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WecomAttachmentDelivery")
            .field("objects", &"<dyn MediaObjectStore>")
            .field("queries", &"<dyn AttachmentQueries>")
            .field("senders", &"<dyn MediaSenderLookup>")
            .finish_non_exhaustive()
    }
}

impl WecomAttachmentDelivery {
    /// 装配。`gates` 要传**同一个** `Outbound` 上那份（`clone` 就够 —— 状态是共享的）。
    #[must_use]
    pub fn new(
        objects: Arc<dyn MediaObjectStore>,
        queries: Arc<dyn AttachmentQueries>,
        senders: Arc<dyn MediaSenderLookup>,
        gates: AttachmentGates,
        metrics: Option<&'static dyn Metrics>,
    ) -> Self {
        Self {
            objects,
            queries,
            senders,
            gates,
            accounting: AttachmentAccounting::new(metrics),
        }
    }
}

#[async_trait]
impl AttachmentDelivery for WecomAttachmentDelivery {
    /// 上游 `sendAttachments`：投递绑在一条回答上的每一个文件。
    ///
    /// 文件彼此独立：一个失败不阻止其余的，而"哪些没有明确到达"这件事在最后**说一次**、
    /// 不是每个说一遍。
    ///
    /// 准入（admitted）已经由 [`crate::wecom::outbound::Outbound::deliver_attachments_by_id`]
    /// 认领；这里管的是**另一件事、而且刻意排在查表之后**：一轮没有文件绑在它上面的轮次不该占掉
    /// 一个 pending 名额 —— 而这件事对用户的意义在于，"因为没名额而被拒的一次投递"只能由一个
    /// **已经知道有一个文件在等**的东西诚实地报告出来。
    async fn deliver(
        &self,
        message_id: &str,
        workspace_id: &str,
        target: AttachmentTarget,
        carries_the_reply: bool,
    ) {
        let (Ok(message_id), Ok(workspace_id)) = (parse_id(message_id), parse_id(workspace_id))
        else {
            // 一次没有助手消息的轮次，没有任何东西绑在它上面。
            return;
        };
        let Ok(rows) = self
            .queries
            .list_attachments_by_chat_message(message_id, workspace_id)
            .await
        else {
            tracing::warn!(
                chat_message_id = %message_id,
                installation_id = %target.installation_id,
                "wecom outbound: attachment lookup failed"
            );
            self.tell_user(&target, MEDIA_LOOKUP_FAILED_TEXT).await;
            self.reply_failed(&target, carries_the_reply, DropReason::Transport, None);
            return;
        };
        if rows.is_empty() {
            // `may_carry_attachments` 说可能有；结果没有。
            if carries_the_reply {
                self.accounting
                    .skipped_for(&target.session_id, SkipReason::NothingToSay);
            }
            return;
        }
        // 过这里之后"有一个文件在等"是**已知的** ⇒ 这个函数每一条出口都必须以一次投递、或者
        // 一句对用户说的话结束。
        if !self.gates.claim_pending() {
            // 与准入不同，行数是已知的 ⇒ 记账可以精确：每个不会发出去的文件各记一次，而
            // （当文件**就是**那条回复时）回复自己的结局也记一次。
            for _ in &rows {
                self.accounting
                    .attachment_dropped(DropReason::AttachmentNotAdmitted, None);
            }
            self.reply_failed(
                &target,
                carries_the_reply,
                DropReason::AttachmentNotAdmitted,
                None,
            );
            tracing::warn!(
                installation_id = %target.installation_id,
                attachments = rows.len(),
                "wecom outbound: attachment delivery shed, too many already pending"
            );
            self.tell_user(&target, MEDIA_SEND_FAILED_TEXT).await;
            return;
        }
        self.send_attachments(&target, carries_the_reply, &rows)
            .await;
        self.gates.release_pending();
    }
}

impl WecomAttachmentDelivery {
    /// 上游 `sendAttachments` 的循环体。
    #[allow(clippy::too_many_lines)] // 上游 `sendAttachments` 就是一个长函数，逐条对齐优先于拆行
    async fn send_attachments(
        &self,
        target: &AttachmentTarget,
        carries_the_reply: bool,
        rows: &[AttachmentRow],
    ) {
        // 在这里解析而不是从调用方带进来：送出那句话的发送可能已经是几分钟前、在一条此后被换掉的
        // socket 上，而注册表总是持有活着的那一条。
        let Some(sender) = self.senders.live_sender(target.installation_id) else {
            // 没有用来告诉它的东西 —— 会驮着这句道歉的那条 socket 正是缺掉的那条。日志是这件事
            // 唯一能去的地方。
            tracing::warn!(
                installation_id = %target.installation_id,
                attachments = rows.len(),
                "wecom outbound: no live connection for attachment delivery"
            );
            for _ in rows {
                self.accounting
                    .attachment_dropped(DropReason::NoLiveConnection, None);
            }
            self.reply_failed(
                target,
                carries_the_reply,
                DropReason::NoLiveConnection,
                None,
            );
            return;
        };

        let mut failed = 0usize;
        let mut unknown = 0usize;
        // 回复自己的结局取**确定的**逐文件原因里最坏的那一个（按 `worseDropReason` 的优先级），
        // 于是单独一个被拒的文件会在回复上也显现为 `platform_refused`。未知的结局**不进**它：
        // 一个可能已经到的文件不能让那条回复变成一次确定的丢弃。
        let mut reply_reason: Option<DropReason> = None;
        let mut reply_unconfirmed: Option<&'static str> = None;
        for row in rows {
            let (state, error) = self.send_attachment(&sender, row, target).await;
            match state {
                DeliveryState::DefinitelyFailed => {
                    failed += 1;
                    // 一次**发送**失败按它的原因分类；上传阶段的失败（没有 `media_id`）与
                    // 读对象的失败都归 `transport_error`（上游 `classifyDrop` 的 default 那一支）。
                    let reason = match error.as_ref() {
                        Some(MediaUploadError::Send(sender)) => {
                            classify_drop(&OutboundError::Send(sender.clone()))
                        }
                        _ => DropReason::Transport,
                    };
                    self.accounting.attachment_dropped(
                        reason,
                        error.as_ref().and_then(as_outbound_error).as_ref(),
                    );
                    reply_reason = Some(match reply_reason {
                        Some(existing) => {
                            crate::wecom::outcome::worse_drop_reason(existing, reason)
                        }
                        None => reason,
                    });
                }
                DeliveryState::Unknown => {
                    unknown += 1;
                    // 不是一次失败：帧很可能已经到了。记在它自己的计数器上，好让丢弃率仍然是一个
                    // **确定的**丢弃率。
                    let reason = error
                        .as_ref()
                        .and_then(unconfirmed_of)
                        .unwrap_or("ack_timeout");
                    self.accounting.attachment_unconfirmed(
                        reason,
                        error.as_ref().and_then(as_outbound_error).as_ref(),
                    );
                    reply_unconfirmed = Some(match reply_unconfirmed {
                        Some(existing) => {
                            crate::wecom::outcome::worse_unconfirmed_reason(existing, reason)
                        }
                        None => reason,
                    });
                }
                DeliveryState::Delivered => self.accounting.attachment_delivered(),
            }
            if error.is_some() {
                // 对象 URL 不进日志：它是一个谁持有就能把文件拿去用的地址。
                tracing::warn!(
                    error = error.as_ref().map(ToString::to_string).as_deref(),
                    delivery = state.as_str(),
                    installation_id = %target.installation_id,
                    attachment_id = %row.id,
                    content_type = row.content_type.as_str(),
                    size_bytes = row.size_bytes,
                    "wecom outbound: attachment not confirmed delivered"
                );
            }
        }
        if carries_the_reply {
            // 文件**就是**那条回答。三种诚实的结局：任何一个文件到了 ⇒ 已送达；一个都没到而至少
            // 有一个失败是确定的 ⇒ 按最坏的那个确定原因记一次丢弃；两边都不确定 ⇒ 未确认，
            // 因为把一条"也许已送达"的回复叫成丢弃，会把运维指向一次用户可能已经收到的重发。
            if failed + unknown < rows.len() {
                self.accounting.delivered();
            } else if failed > 0 {
                self.reply_failed(
                    target,
                    true,
                    reply_reason.unwrap_or(DropReason::Transport),
                    None,
                );
            } else {
                self.accounting.unconfirmed_for(
                    &target.session_id,
                    "chat:done",
                    reply_unconfirmed.unwrap_or("ack_timeout"),
                    None,
                );
            }
        }
        // 回答已经在用户屏幕上，而它很可能提到了一个文件。什么都不说会让他们找一个永远不会来的
        // 东西 —— 但对一个**确实**到了的文件说"失败了"是另一种伤害，所以每一组说自己那句话，
        // 而未确认的发送永远不借用确定的措辞。
        let mut lines: Vec<&str> = Vec::new();
        if failed > 0 {
            lines.push(MEDIA_SEND_FAILED_TEXT);
        }
        if unknown > 0 {
            lines.push(MEDIA_SEND_UNKNOWN_TEXT);
        }
        if !lines.is_empty() {
            self.tell_user(target, &lines.join("\n")).await;
        }
    }

    /// 回复自己的结局（只在文件**就是**回复时欠一个）。
    fn reply_failed(
        &self,
        target: &AttachmentTarget,
        carries_the_reply: bool,
        reason: DropReason,
        error: Option<&OutboundError>,
    ) {
        if carries_the_reply {
            self.accounting
                .dropped_for(&target.session_id, "chat:done", reason, error);
        }
    }

    /// 上游 `tellUser`：往会话里放一句话，尽力而为。
    ///
    /// 每个调用方都已经在一条出错的路上，所以这里的失败**记日志并丢掉**，不往上抛 ——
    /// 没有更多可以试的了。
    async fn tell_user(&self, target: &AttachmentTarget, text: &str) {
        let Some(sender) = self.senders.live_sender(target.installation_id) else {
            return;
        };
        if let Err(error) = sender
            .send_text(&target.chat_id, target.chat_type, text, None)
            .await
        {
            tracing::warn!(
                error = %error,
                installation_id = %target.installation_id,
                "wecom outbound: could not tell the user about the file"
            );
        }
    }

    /// 上游 `sendAttachment`：把一个文件从对象存储带进聊天，并报告**已知**它落在哪里。
    /// 错误是给日志的，状态是给用户听的。
    async fn send_attachment(
        &self,
        sender: &WsSender,
        row: &AttachmentRow,
        target: &AttachmentTarget,
    ) -> (DeliveryState, Option<MediaUploadError>) {
        // 记下来的大小在**一个字节都没取**之前就被检查。超帽的附件两条路都会被拒 ——
        // `read_object` 会再查一次它**真的**读到了多少，因为那一列是元数据、对象才是真相 ——
        // 但从存储里读 40 MB 再拒掉它是谁都不受益的工作。
        if row.size_bytes > i64::try_from(MAX_MEDIA_UPLOAD_BYTES).unwrap_or(i64::MAX) {
            return (
                DeliveryState::DefinitelyFailed,
                Some(MediaUploadError::UploadTooLarge),
            );
        }
        let data = match read_object(self.objects.as_ref(), &row.url) {
            Ok(data) => data,
            Err(error) => return (DeliveryState::DefinitelyFailed, Some(error)),
        };
        let kind = wecom_media_kind(&row.content_type, &row.filename, data.len());
        let name = outbound_media_name(&row.filename, &row.content_type);
        let media_id = match upload_media(
            sender,
            &OutboundMedia {
                kind,
                filename: name.clone(),
                data,
            },
            None,
        )
        .await
        {
            Ok(media_id) => media_id,
            Err(error) => {
                // 一次失败的上传从未产出 `media_id` ⇒ 那条消息从未被寻址到那个聊，
                // 包括失败来自 finish 自己那次丢掉的 ack 时。文件确定不在。
                return (DeliveryState::DefinitelyFailed, Some(error));
            }
        };
        // 视频是唯一在 `media_id` 之外还有字段的种类，而两个都是必填。文件自己的名字就是我们
        // 关于它能说的话 —— 附件行不带说明文字，而 agent 的话已经在上面那条消息里了。
        let title = name
            .rsplit_once('.')
            .map_or_else(|| name.clone(), |(stem, _)| stem.to_owned());
        let result = send_media(
            sender,
            &target.chat_id,
            target.chat_type,
            &MediaSend {
                kind,
                media_id,
                title,
                description: name,
            },
            None,
        )
        .await;
        let state = send_outcome(result.as_ref().err());
        (state, result.err())
    }
}

/// `MediaUploadError` → `OutboundError`：只有**发送**那一半有对应物（上游 `classifyDrop` 只认
/// `wecomAPIError` 那一类；上传阶段的失败在上游落进 `default` 那一支 = `transport_error`）。
fn as_outbound_error(error: &MediaUploadError) -> Option<OutboundError> {
    match error {
        MediaUploadError::Send(sender) => Some(OutboundError::Send(sender.clone())),
        _ => None,
    }
}

/// 一次未确认发送的原因 label（上游 `unconfirmedReason`）。
fn unconfirmed_of(error: &MediaUploadError) -> Option<&'static str> {
    match error {
        MediaUploadError::Send(sender) => unconfirmed_send_reason(sender),
        _ => None,
    }
}

/// 上游 `readObject`：把整个文件拉进内存。
///
/// 它**必须**是整份的：上传在第一个块出去之前就声明了 `total_size` 与 `total_chunks`，
/// 所以这一条没有流式可言。
///
/// # Errors
///
/// 见 [`MediaUploadError`]。
pub fn read_object(
    objects: &dyn MediaObjectStore,
    raw_url: &str,
) -> Result<Vec<u8>, MediaUploadError> {
    let key = objects.key_from_url(raw_url);
    if key.is_empty() {
        return Err(MediaUploadError::Storage);
    }
    let mut reader = objects
        .get_reader(&key)
        .map_err(|_| MediaUploadError::Storage)?;
    // 一个字节的余量，好让"恰好读满上限"与"还有更多要来"能分开。上限是**平台的**，不是组帧的，
    // 所以读停在那里而不是停在块协议本来能表达的那 50 MB —— 多出来的 30 MB 只会驻留到被拒为止。
    let mut data = Vec::new();
    // 64 KiB 一个缓冲区（`clippy::large_stack_arrays` 会拒掉栈上的 64 KiB）。
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| MediaUploadError::Storage)?;
        if read == 0 {
            break;
        }
        if data.len() + read > MAX_MEDIA_UPLOAD_BYTES {
            return Err(MediaUploadError::UploadTooLarge);
        }
        data.extend_from_slice(&buffer[..read]);
    }
    Ok(data)
}

/// 上游 `wecomMediaKind`：决定告诉 `WeCom` 这个文件**是什么**。
///
/// 内容类型领跑，因为那是上传者声明的东西。它说不出有用的东西时 —— 空、或者那个意思是"一串字节"
/// 的 `octet-stream` —— 文件名的扩展名是更好的猜法。一个帽被它超过的种类会**降级成文件**，
/// 而不是送出去再被拒。
#[must_use]
pub fn wecom_media_kind(content_type: &str, filename: &str, size: usize) -> MediaMsgType {
    let mut base = base_content_type(content_type);
    if base.is_empty() || base == "application/octet-stream" {
        let extension = std::path::Path::new(filename)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        base = base_content_type(super::media_ingest::content_type_for_extension(extension));
    }
    if base.starts_with("image/") && size <= MAX_OUTBOUND_IMAGE_BYTES {
        return MediaMsgType::Image;
    }
    if base.starts_with("video/") && size <= MAX_OUTBOUND_VIDEO_BYTES {
        return MediaMsgType::Video;
    }
    // 语音**只**认 AMR。一条当语音发出去的 mp3 会被拒，而作为一个文件它至少点一下就能放。
    if base == "audio/amr" && size <= MAX_OUTBOUND_VOICE_BYTES {
        return MediaMsgType::Voice;
    }
    MediaMsgType::File
}

/// 上游 `outboundMediaName`：收件人在文件卡上看到的名字。
///
/// 它被压成**一个**路径段 —— 这个名字要上 wire，而存下来的文件名不保证是一个 —— 并在没有扩展名
/// 时补一个，因为那是 `WeCom` 关于格式拿到的**唯一**提示。
#[must_use]
pub fn outbound_media_name(filename: &str, content_type: &str) -> String {
    let mut name = clean_media_filename(filename);
    if name.is_empty() {
        "attachment".clone_into(&mut name);
    }
    if std::path::Path::new(&name).extension().is_none() {
        let extension = media_extension(content_type);
        if !extension.is_empty() {
            name.push_str(extension);
        }
    }
    name
}

/// 一个 id 字符串 → [`Id`]（解析不了 ⇒ `None`，与 M7-17 的 `parse_uuid` 同语义）。
fn parse_id(raw: &str) -> Result<Id, ()> {
    uuid::Uuid::parse_str(raw).map(Id).map_err(|_| ())
}

#[cfg(test)]
mod tests;
