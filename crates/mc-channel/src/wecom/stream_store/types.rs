//! `stream_store` 的**值类型**：句柄、地址、轮次的键与条目、以及收尾交回来的那一轮。
//!
//! 本文件是 `stream_store.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分
//! 加上门 ⑩ 的 800 行硬限。逐条清单见 `docs/32` §33 的 D10。

use std::sync::Arc;
use std::time::Instant;

use mc_core::id::Id;

use super::{Inner, MAX_FINISHED_ROUNDS};
use crate::wecom::strings::Locale;

/// 时钟（上游 `streamStore.now`，一个字段以便用例注入）。
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// `open` 的判决：一条刚到的消息会不会拿到自己的气泡（上游 `openVerdict`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenVerdict {
    /// 上游 `roundOpened`：没有轮次在收集，所以这条消息开一轮。**调用方去画开场帧**；
    /// 此后这一轮拥有它登记的句柄。
    Opened,
    /// 上游 `roundJoined`：已经有轮次在屏幕上、而且还在等它的 run，所以会回答这条消息的
    /// 那个去抖窗口就是那个气泡代表的那一个。**不画任何东西。**
    Joined,
}

/// 继续往一个打开的气泡里写所需的**全部**信息（上游 `streamHandle`）。
///
/// 寻址在 ingest 时就被捕获，而不是以后再查：等回答到达时，绑定行可能已经被重指，
/// 而帧必须回到**当初问的那个聊**去。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamHandle {
    /// `aibot_msg_callback` 的 `req_id`。`WeCom` 拒掉任何一个别的值，
    /// **包括**来自事件回调的 `req_id`（`errcode 846605`）。
    pub req_id: String,
    /// 我们自己选的 stream id。复用 = 更新消息；换一个 = 再开一条 ——
    /// 这正是"一个会话会同时持有好几个气泡"的来路。
    pub stream_id: String,
    /// 哪把 socket 在跑。`None` = 这条安装没有活的 socket（上游 `pgtype.UUID.Valid == false`）。
    pub installation_id: Option<Id>,
    /// 会话（单聊是 userid，群聊是 chatid）。
    pub chat_id: String,
    /// 上游的 aibot 接收者形态整数（1 / 2），**不是** engine 的 [`mc_core::channel::message::ChatType`]：
    /// 上游把 `ChatType` → int 的换算放在 `ws_frame.go` 的 [`super::ws_frame::aibot_chat_type_from_channel`]。
    pub chat_type: i32,
    /// 这一轮收尾话用哪种语言写。它随句柄走，因为每个收尾器都在**之后**从一命名了 task、
    /// 不命名别人的事件上跑 —— 离那个知道"是谁问的"的 goroutine 已经过去几分钟。
    pub locale: Locale,
    /// 流是什么时候开的 —— 也就是协议窗口从哪儿开始数。
    pub created_at: Instant,
}

/// 一个轮次的气泡没了之后，它的话往哪儿去（上游 `roundAddress`）。
///
/// 两个 stream id **故意**不在这里：它们命名的是一个谁也写不了的气泡了，
/// 带上它们只会招来另一次尝试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundAddress {
    pub installation_id: Option<Id>,
    pub chat_id: String,
    pub chat_type: i32,
}

impl RoundAddress {
    /// 上游 `known()`：这条地址有没有一把可以用的 socket。
    #[must_use]
    pub fn is_known(&self) -> bool {
        self.installation_id.is_some()
    }
}

impl StreamHandle {
    /// 这个句柄降级后的落脚点（上游 `streamHandle.address`）。
    #[must_use]
    pub fn address(&self) -> RoundAddress {
        RoundAddress {
            installation_id: self.installation_id,
            chat_id: self.chat_id.clone(),
            chat_type: self.chat_type,
        }
    }
}

/// 一轮收尾时说"我没什么要说的"（上游 `errNothingToSay`）：一次空完成、没有气泡可封、
/// 也没有文件要发，或者这个会话根本没有 `WeCom` 路由。什么也没到用户那儿、也不欠什么，
/// 所以它不值得一条 warning —— **跳过**不等于**丢弃**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("wecom: nothing to say for this round")]
pub struct NothingToSay;

/// [`StreamStore::take`] 交回来的东西：这一轮的气泡，如果它还有一个能写的话
/// （上游 `roundTurn`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundTurn {
    /// 这一轮打开的气泡。
    pub handle: StreamHandle,
    /// 有没有气泡：一个没画过开场帧的轮次、或一个过了协议窗口的轮次报 `false`，
    /// 它的话以普通消息发出 —— 句柄仍然命名着当初问的那个聊。
    pub has_bubble: bool,
}

/// 一次收尾替**哪一轮**说话：这个会话的 `task:queued` 绑给它的那个 task id。
/// 权威、且从不推断 —— 一个不在册的 run 在这里没有气泡（上游 `roundKey`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundKey {
    pub task_id: String,
}

/// 按 task id 取键（上游 `byTask`）——**唯一**的构造方式，因为"推断一个键"没有定义。
#[must_use]
pub fn by_task(task_id: impl Into<String>) -> RoundKey {
    RoundKey {
        task_id: task_id.into(),
    }
}

/// 存储自己给一轮起的名字，按**画开场帧的顺序**发放（上游 `roundSeq`）。
/// 它是内部句柄而不是平台 id：这个文件之外唯一持有它的地方是 [`StreamStore::open`] 的
/// 调用方，它在服务端**直接拒掉**开场帧时把它交回来（[`StreamStore::drop_round`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RoundSeq(pub u64);

/// 一轮在一个会话里的位置，从"它有一点什么已知"到"有东西取走它"（上游 `roundEntry`）。
///
/// 气泡与 run 从**两个方向**、**任意顺序**到达，所以这个条目独立于两者存在。
/// 一个跑着 task 却没有气泡的条目，就是一个还没到那儿的 ingest goroutine（或者开场帧被
/// 服务端拒了）：它的收尾仍然被正确匹配，只是屏幕上没有落脚点、退回普通消息。
#[derive(Debug, Clone)]
pub(crate) struct RoundEntry {
    /// 这一轮在它的会话的**画帧顺序**里的位置，也是它还没别的名字时的身份。
    pub(crate) seq: RoundSeq,
    pub(crate) handle: StreamHandle,
    /// 有没有画过开场帧。
    pub(crate) painted: bool,
    /// 绑到这一轮的 run（来自会话的 `task:queued`）。空 = 这一轮在等一个。
    pub(crate) task_id: String,
    /// 区分"等待"的**两种**方式 —— 两者行为不同，且不许混（上游 `everBound`）：
    ///
    /// - **从没绑过**：会回答它的那个去抖窗口还没 flush。现在到的消息属于同一个窗口，
    ///   所以它**加入**而不是新开一个气泡。
    /// - **绑过一次、被 `retry_unbind` 释放**：平台正在用自动重试的 clone 替换这一轮的
    ///   run。这一轮处在**两个 run 之间**而不是在收集：新消息**不得**加入它。
    pub(crate) ever_bound: bool,
    /// 一个被释放的轮次在等哪个尝试的替代者 —— 那个 parent 的 clone 会来回答这一轮的问题。
    /// 由 [`StreamStore::retry_unbind`] 设置，一绑上 run 就清掉。
    ///
    /// 它存在是因为**别的什么都区分不了 clone 与一个新问题的 run**：两者都是同一会话上
    /// 一个新 id 的新 task 行，而 adapter 不得在总线上读库。两种顺序是对称的
    /// （clone 可能在一个新问题的 run 之前或之后到）⇒ 没有任何"到达顺序"规则能解开它们，
    /// 轮次必须**带着它在等谁的名字**，直到一次收尾能解开这条血缘。
    pub(crate) retry_of: String,
    /// 没有句柄可读时间时，扫除用的界（上游 `createdAt`）。
    pub(crate) created_at: Instant,
}

/// 一个排在会话里、却没有轮次等它的 run（上游 `pendingRun`）：要去画它那个气泡的
/// ingest goroutine 还没到。它一直挂着，直到一个气泡出现，或者 [`PENDING_MAX_AGE`]
/// 说不会来了。
#[derive(Debug, Clone)]
pub(crate) struct PendingRun {
    pub(crate) task_id: String,
    pub(crate) at: Instant,
}

/// 一个会话最近结束的 run（上游 `finishedRing`），带"最后一个是什么时候加的"，
/// 好让扫除把整个环退休掉。
#[derive(Debug, Clone, Default)]
pub(crate) struct FinishedRing {
    pub(crate) tasks: Vec<String>,
    pub(crate) at: Option<Instant>,
}

impl FinishedRing {
    pub(crate) fn has(&self, task_id: &str) -> bool {
        !task_id.is_empty() && self.tasks.iter().any(|task| task == task_id)
    }
}

/// 把一个 run 记进 `finished` 环（上游 `retireLocked` 的自由函数形式，供
/// [`StreamStore::take_at_locked`] 这个不持有 `&self` 的地方调用）。
pub(super) fn retire(inner: &mut Inner, session: Id, task_id: &str, now: &Clock) {
    if task_id.is_empty() {
        return;
    }
    let ring = inner.finished.entry(session).or_default();
    if !ring.has(task_id) {
        ring.tasks.push(task_id.to_owned());
        if ring.tasks.len() > MAX_FINISHED_ROUNDS {
            let excess = ring.tasks.len() - MAX_FINISHED_ROUNDS;
            ring.tasks.drain(..excess);
        }
    }
    ring.at = Some((now)());
}

/// 从一个会话的 `pending` 队列里去掉一个 run（上游 `dropPendingLocked`）。
pub(super) fn drop_pending_locked(inner: &mut Inner, session: Id, task_id: &str) {
    let Some(queue) = inner.pending.get_mut(&session) else {
        return;
    };
    let before = queue.len();
    // 上游只去掉**第一个**匹配（`queue = append(queue[:i], queue[i+1:]...)` 之后 return）。
    if let Some(index) = queue.iter().position(|pending| pending.task_id == task_id) {
        queue.remove(index);
    }
    if before != queue.len() && queue.is_empty() {
        inner.pending.remove(&session);
    }
}
