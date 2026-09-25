//! `outbound` 的**附件面**：两个准入计数器（上游 `admittedAttachments` / `pendingAttachments`）
//! 与逐文件投递的接缝（实现归 **M7-18** 的 `outbound_media.rs`）。
//!
//! 本文件是 `outbound.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::id::Id;

use crate::wecom::stream_store::RoundAddress;

use super::events::ChatDone;
use super::{
    spawn_detached, uuid_string, Outbound, MAX_ADMITTED_ATTACHMENT_DELIVERIES,
    MAX_PENDING_ATTACHMENT_DELIVERIES,
};

// =====================================================================
// 附件面（上游 `outbound.go` 的两个计数器 + `outbound_media.go` 的工作，M7-18）
// =====================================================================

/// 一轮回答的**文件往哪儿去**（上游 `attachmentTarget`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTarget {
    pub installation_id: Id,
    pub chat_id: String,
    pub chat_type: i32,
    pub session_id: String,
}

/// 逐文件投递（上游 `sendAttachments` / `sendAttachment` / `tellUser` / `readObject`，
/// **M7-18** 的 `outbound_media.rs`）。
///
/// 本片**不实现**它：准入与记账骨架在 [`AttachmentGates`]（上游那两个计数器本来就是
/// `Outbound` 的字段），而查表 / 上传 / 逐文件计数是 M7-18 的事。
///
/// # 🔴 写集勘误（`docs/32` §35 的 D12）：这一个接缝的形状本片必须改两处
///
/// 1. **同步方法 → `async`**：准入名额本来由 `deliver_attachments_by_id` 持有到投递结束，
///    而一个同步方法在它返回的那一刻就把名额交回去了 ⇒ `MAX_ADMITTED_ATTACHMENT_DELIVERIES`
///    不再约束**它本来要约束的那个东西**（那次查表）。改成 `async` 之后名额活到 `await` 结束，
///    语义与上游的 `goroutine` 逐字相同。
/// 2. **多带两个 id**：上游的 `sendAttachments(ctx, messageID, workspaceID, to, carries)` 靠
///    `ListAttachmentsByChatMessage(messageID, workspaceID)` 查表，而原来的 `deliver(target, …)`
///    把两个 id **丢掉**了 ⇒ 端口实现拿不到它为哪条消息投递。中继那条路径（`relay/relayed.rs`）
///    本来就是按 id 调的，所以两个调用点各多传两个引用。
#[async_trait]
pub trait AttachmentDelivery: Send + Sync {
    /// 把一条回答产出的文件送到 `target`。在**脱离任务**里跑（准入已经由调用方拿到手）。
    ///
    /// `message_id` / `workspace_id` 是这条回答的助手消息与它所属的 workspace
    /// （上游 `deliverAttachmentsByID` 的两个参数）。
    async fn deliver(
        &self,
        message_id: &str,
        workspace_id: &str,
        target: AttachmentTarget,
        carries_the_reply: bool,
    );
}

/// 上游 `Outbound` 的两个准入计数器（`admittedAttachments` / `pendingAttachments`）。
///
/// 它们是**两个**，因为一个不可能同时待在两个地方：
///
/// - **admitted** 数"本订阅者起了、还没看到返回"的任务。它在 **spawn 之前**就被认领 ⇒ 它既
///   约束这个任务自己那次查表、也约束任务本身。那一刻关于这一轮**什么都还不知道**
///   （它带不带着文件正是查表要回答的），所以越过它只能**记日志**、不能说话。
/// - **pending** 数"已经查过一轮、并且**找到了**文件"的投递。它在查表**之后**被认领，
///   这正是"因为没容量而被拒的投递可以被告知用户、而绝不会为一份从不存在的文件报警"的来路。
///
/// admitted 上限故意是 pending 的两倍（见 [`MAX_ADMITTED_ATTACHMENT_DELIVERIES`]）。
#[derive(Debug, Clone)]
pub struct AttachmentGates {
    /// `Arc` 而不是裸 `Mutex`：一个 admitted 名额要活到**那个脱离任务**结束，
    /// 所以句柄得自己带着状态（`'static`），不能借 `Outbound`。
    state: Arc<std::sync::Mutex<GatesInner>>,
    admitted_cap: usize,
    pending_cap: usize,
}

#[derive(Debug, Default)]
struct GatesInner {
    admitted: usize,
    pending: usize,
}

impl Default for AttachmentGates {
    fn default() -> Self {
        Self::new(
            MAX_ADMITTED_ATTACHMENT_DELIVERIES,
            MAX_PENDING_ATTACHMENT_DELIVERIES,
        )
    }
}

impl AttachmentGates {
    /// 换掉两个上限（用例把 `1` 塞进去就能看到削减路径，而不必真起 65 个任务）。
    #[must_use]
    pub fn new(admitted_cap: usize, pending_cap: usize) -> Self {
        Self {
            state: Arc::new(std::sync::Mutex::new(GatesInner::default())),
            admitted_cap,
            pending_cap,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GatesInner> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 上游 `admitAttachmentDelivery`：占一个 admitted 名额，或者报"已经在跑的有太多了"。
    pub fn admit(&self) -> Option<AttachmentAdmission> {
        let mut inner = self.lock();
        if inner.admitted >= self.admitted_cap {
            return None;
        }
        inner.admitted += 1;
        drop(inner);
        Some(AttachmentAdmission {
            state: Arc::clone(&self.state),
        })
    }

    /// 上游 `claimAttachmentSlot`：占一个 pending 名额，或者报"积压已满"。
    pub fn claim_pending(&self) -> bool {
        let mut inner = self.lock();
        if inner.pending >= self.pending_cap {
            return false;
        }
        inner.pending += 1;
        true
    }

    /// 上游 `releaseAttachmentSlot`。
    pub fn release_pending(&self) {
        let mut inner = self.lock();
        inner.pending = inner.pending.saturating_sub(1);
    }

    /// 当前计数（诊断与用例用）。
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let inner = self.lock();
        (inner.admitted, inner.pending)
    }
}

/// admitted 名额的 RAII 句柄（上游 `releaseAttachmentAdmission` 的 `defer`）。
#[derive(Debug)]
pub struct AttachmentAdmission {
    state: Arc<std::sync::Mutex<GatesInner>>,
}

impl Drop for AttachmentAdmission {
    fn drop(&mut self) {
        let mut inner = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.admitted = inner.admitted.saturating_sub(1);
    }
}

impl Outbound {
    /// 上游 `mayCarryAttachments`：这一轮值不值得那几次查表 —— 即使 agent 什么都没说。
    /// 它检查的每一样都已经在手，所以一个没有对象存储的部署（或者一个不命名任何消息的事件）
    /// 一分钱查询都不花。
    #[must_use]
    pub fn may_carry_attachments(&self, event: &ChatDone) -> bool {
        self.has_attachment_storage()
            && !event.workspace_id.is_empty()
            && !event.message_id.is_empty()
    }

    /// 上游 `deliverAttachments`：把回答的文件交给**它们自己的**一个任务，如果有的话。
    /// 它在话出去之后被调，并立即返回。
    pub fn deliver_attachments(
        &self,
        event: &ChatDone,
        addr: &RoundAddress,
        carries_the_reply: bool,
    ) {
        self.deliver_attachments_by_id(
            &event.message_id,
            &event.workspace_id,
            &AttachmentTarget {
                installation_id: addr
                    .installation_id
                    .unwrap_or_else(|| Id(uuid::Uuid::nil())),
                chat_id: addr.chat_id.clone(),
                chat_type: addr.chat_type,
                session_id: event.chat_session_id.clone(),
            },
            carries_the_reply,
        );
    }

    /// 上游 `deliverAttachmentsByID`：附件投递的真正入口（本地路径与中继路径**共用**它 ——
    /// 中继帧驮的是 id，所以它按 id 调）。
    ///
    /// `carries_the_reply` 说文件**就是**这条回复的实质 —— agent 什么都没说，绑了一个文件
    /// 代替。它为真时还没有任何回复结局被记下，而这条路径正好欠一个；为假时话已经落地、
    /// 回复的结局已经结算，所以这里只动逐文件的计数器。
    pub fn deliver_attachments_by_id(
        &self,
        message_id: &str,
        workspace_id: &str,
        target: &AttachmentTarget,
        carries_the_reply: bool,
    ) {
        let Some(port) = self.attachments.as_ref() else {
            // 没有对象存储 ⇒ 文件面整体关着（上游 `o.objects == nil`）。
            return;
        };
        if !self.attachment_storage {
            return;
        }
        let installation_id = target.installation_id;
        if installation_id.0.is_nil() || target.chat_id.is_empty() {
            // 上游逐字：没有可用的安装、没有聊 ⇒ 这份投递不是任何人的。
            return;
        }
        if message_id.is_empty() || workspace_id.is_empty() {
            // 一次没有 assistant 消息的轮次，没有任何东西绑在它上面。
            return;
        }
        // 准入在**这里**认领，而不是在那个任务内部：一个已经起来的任务是这个上限**没有**
        // 约束到的任务，它跑的那次查表也在这道门的另一侧 —— 在一个慢库下，无界的查表与无界的
        // 任务是同一个失败换了顶帽子。
        //
        // 关于这一轮**什么都还不知道** —— 它带不带着文件正是查表要回答的 —— 所以在这里被拒只能
        // 记日志、不能说给用户听：在一个可能一个文件都没带的轮次上告诉用户"你的文件被丢了"，
        // 正是查表之后那道门存在的理由要避免的误报。
        let Some(admission) = self.gates.admit() else {
            // 一次**调度**拒绝，记在它自己的单位上，**别的什么都不记**：这道门跑在查表之前，
            // 它既不知道这一轮带几个文件、也不知道它到底带不带。从这里喂一个逐文件计数器是在
            // 编造基数，从这里断言一个回复结局是在编造那条回复：一次零绑定的空完成会被记成
            // 一条被丢弃的回复，而查表本来会把它分类成 `nothing_to_say`。
            // **这里不可能知道的东西，这里就不记。**
            self.attachment_shed();
            tracing::warn!(
                installation_id = %uuid_string(installation_id),
                admitted = MAX_ADMITTED_ATTACHMENT_DELIVERIES,
                "wecom outbound: attachment delivery not admitted, too many already running"
            );
            return;
        };
        let target = target.clone();
        let message_id = message_id.to_owned();
        let workspace_id = workspace_id.to_owned();
        let port = Arc::clone(port);
        spawn_detached(async move {
            // admission 随这个任务活到结束（含它那次查表，含一次根本没带文件的轮次）——
            // 这正是本片把 `deliver` 改成 async 的原因（写集勘误 D12）。
            let _admission = admission;
            port.deliver(&message_id, &workspace_id, target, carries_the_reply)
                .await;
        });
    }
}
