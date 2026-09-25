//! 让每条回答落进它那个问题打开的气泡里的**句柄**（上游
//! `internal/integrations/wecom/stream_store.go`，**1,122 行**）。
//!
//! - **写者**：M7-16（`LUM-1781` / `docs/60-M7-PLAN.md` §3.3）。
//! - **`WeCom` 的 aibot API 没有"正在输入"、没有表情回应、没有已读回执、事后也无法编辑
//!   消息**（上游注释逐字）。它唯一有的能力是**流式消息**：一帧 `finish=false` 的
//!   `aibot_respond_msg` 画出一个客户端渲染成"工作中"的气泡，而后一帧带**同一个**
//!   `stream.id` 的帧就地替换那个气泡的正文 —— `finish=true` 封口，此后谁都碰不了它。
//!
//! # 这个存储是**缓存**，仅此而已
//!
//! 它把一个聊天会话映射到"还能写得进去的气泡"。一处收尾如果找到可写的气泡就写进去；
//! 找不到就走普通 `aibot_send_msg` 路径。**这里不记录说过什么、欠着谁、告诉过谁**；
//! 气泡一没，这里谁都不欠谁。（上游注释逐字的十条要点，逐条落在下面的类型与函数上。）
//!
//! # 进程内就是正确的存储 —— 而这是 `R-M7-1` 的**正面**那一半
//!
//! 一个 bot 就是一条长连接，而 `Supervisor` 的 WS 租约已经保证至多一个副本持有它
//! ⇒ 一个句柄只在**创建它的那个进程**里有意义。重启丢掉句柄、回答退回普通消息 ——
//! 是降级而不是损坏。持久化它们是**权衡**而不是修复。
//!
//! 上游这一处**不用** Redis（`docs/60` §2.5 的表里 Redis 的四处用途是
//! `redis_lease_store` / `dedupe_redis` / `install_session_redis_store` / `relay_outbound`）；
//! 本片把上游的**进程内**实现原样搬过来，所以本片对 `R-M7-1`（单副本部署契约）的贡献是
//! **登记**而不是替换：真正被替换成进程内替身的是 M7-20 的 `dedupe_redis.go`。见
//! `docs/32` §33 的 D1。
//!
//! # 本片与后续片的接缝
//!
//! 上游 `streamStore` 依赖三件本片**没有**的东西：`sendersRegistry`（M7-20 的
//! `senders.rs`）、`taskLookup`（读库的 `chat_input_task_id`）、`Locale`（M7-15 已有 ✓）。
//! 前两件在这里落成**端口 trait**（[`StreamSender`] / [`RootResolver`]），
//! 于是「adapter 不得直接写 DB」（`docs/60` §2.6 第 1 条）与「本片不引新依赖」
//! 都是类型层面的事实。见 `docs/32` §33 的 D6 / D7。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mc_core::id::Id;

mod ports;
mod types;

pub use ports::*;
pub use types::*;

/// 一个句柄值得留多久：十分钟。
///
/// 上游 2026-08-09 对**活租户**实测（不是从谁的源码里读来的）：把一个流开着、
/// 每三十秒发一帧，服务端在 600.0s 收下了那一帧、在 630.0s 用 `errcode 846608`
/// 拒了下一帧。所以真实上限在 `(600s, 630s]`，这个常量坐在它的下界上。
///
/// 这份预算属于**流**，不属于承载它的 `req_id`：同一个探测在头一个流 `finish=true`
/// 封口之后，用**新的** stream id 在同一个 `req_id` 上开了第二个，第二个在八分钟大时仍被
/// 接受 —— 远超第一个自己的十分钟，然后在它自己的窗口上死掉。
///
/// 所以这个时钟不是气泡能靠什么续命的东西：跑得比窗口长的一轮会丢掉它的气泡，
/// 回答以普通消息发出。把一轮接着搬到新的流上是**这个层之上的**另一层。
pub const STREAM_MAX_AGE: Duration = Duration::from_secs(600);

/// 一次收尾帧的预算（上游 `streamCloseTimeout`，在 `typing_indicator.go`）。
pub const STREAM_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// 一次 ack 没回来的收尾帧会被重写几次（上游 `streamCloseRetries`）。
pub const STREAM_CLOSE_RETRIES: usize = 3;

/// 两次收尾尝试之间的间隔（上游 `streamCloseRetryDelay`）。
pub const STREAM_CLOSE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// 每个会话已经结束的轮次的记忆上限（上游 `maxFinishedRounds`）。往回十轮，
/// 远超一个 `task:queued` 能落后于一次结束的程度。
pub const MAX_FINISHED_ROUNDS: usize = 10;

/// 一个排好队的 run 可以等它本该绑定的气泡多久（上游 `pendingMaxAge`）。
///
/// 这个时钟是 **ingest goroutine** 的，不是协议的：那个 goroutine 解析一个发送者、
/// 写一帧开场帧、返回 —— 被路由自己的回复超时与一次 ack 等待兜住，几秒钟 ⇒
/// 在那之后还挂着的 run，就是一个气泡永远不会来的 run。
///
/// `STREAM_MAX_AGE` 曾经是它的界，那是**错的**：十分钟是**服务端**让一个流保持可写的时长，
/// 它对一次 ingest 要多久什么都没说。
pub const PENDING_MAX_AGE: Duration = Duration::from_secs(30);

/// 把一个聊会话映射到它的轮次，最老的在前（上游 `streamStore`）。
pub struct StreamStore {
    inner: Mutex<Inner>,
    max_age: Duration,
    pending_max_age: Duration,
    close_retry_delay: Duration,
    now: Clock,
}

#[derive(Default)]
struct Inner {
    /// 会话 → 它的轮次，最老的在前。
    sessions: HashMap<Id, Vec<RoundEntry>>,
    /// 每个会话的"比气泡先到的 run"队列，最老的在前。按顺序、一次一个地被
    /// 这个会话画出的**下一个**气泡排干。
    pending: HashMap<Id, Vec<PendingRun>>,
    /// 每个会话最近几个"轮次已被取走"的 task id，这样**已经结束**的 run 就不能再去绑
    /// 一个后来问题打开的气泡。它是一轮消失之后**唯一**被留下的东西，而且它不说什么
    /// 被说过。由 [`MAX_FINISHED_ROUNDS`] 兜住。
    finished: HashMap<Id, FinishedRing>,
    /// 轮次身份发放器。跨存储单调 ⇒ 在**每个**会话里也单调，而这是这里唯一读它的东西。
    seq: u64,
}

impl Default for StreamStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamStore {
    /// 铸**一个**存储，由写入侧（M7-20 的打字指示）与读取侧（M7-17 的会话完成订阅者）
    /// 共享（上游 `NewStreamStore`）。
    ///
    /// 上游逐字："**一次重连不是一次重启**，而这个区别正是这个存储在建时铸一次、
    /// 放在连接循环**外面**的原因。" 一个句柄活得比铸它的那把 socket 长，而 `WeCom`
    /// 把一个回调的 `req_id` 作用域定在**轮次**上而不是那把 socket 上 ⇒
    /// 断开之前开的气泡由**下一条**连接上的回答封口。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            max_age: STREAM_MAX_AGE,
            pending_max_age: PENDING_MAX_AGE,
            close_retry_delay: STREAM_CLOSE_RETRY_DELAY,
            now: Arc::new(Instant::now),
        }
    }

    /// 换掉窗口（用例把 `max_age` 设成 `Duration::ZERO` 就能走完过期路径而不真的等十分钟）。
    #[must_use]
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// 换掉 `pending` 的界。
    #[must_use]
    pub fn with_pending_max_age(mut self, pending_max_age: Duration) -> Self {
        self.pending_max_age = pending_max_age;
        self
    }

    /// 换掉收尾重试的间隔（用例把三次重试跑得不用等六秒）。
    #[must_use]
    pub fn with_close_retry_delay(mut self, delay: Duration) -> Self {
        self.close_retry_delay = delay;
        self
    }

    /// 换掉时钟（上游 `s.now`）。
    #[must_use]
    pub fn with_clock(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn clock(&self) -> Instant {
        (self.now)()
    }

    // ---- 查表 ----

    /// 一条刚 ingest 的消息属于哪一轮：**从没绑过 run** 的那个，也就是会回答它的去抖窗口
    /// 还开着（上游 `collectingLocked`）。被 `retry_unbind` 释放的轮次**故意**不算 ——
    /// 见 [`RoundEntry::ever_bound`]。
    fn collecting(inner: &Inner, key: Id) -> Option<usize> {
        inner
            .sessions
            .get(&key)?
            .iter()
            .position(|entry| entry.task_id.is_empty() && !entry.ever_bound)
    }

    /// 最老的、还没有 run 绑上去的轮次，**包括**被 `retry_unbind` 释放的那种
    /// （上游 `unboundLocked`）。它是 `bind_next` 的第二选择。
    fn unbound(inner: &Inner, key: Id) -> Option<usize> {
        inner
            .sessions
            .get(&key)?
            .iter()
            .position(|entry| entry.task_id.is_empty() && entry.retry_of.is_empty())
    }

    /// 在等**这个** root 的轮次的索引，以及"这个会话到底有没有轮次在等一次重试"
    /// （上游 `awaitingRetryLocked` 的两个返回值 —— 上游逐字说它们**故意**不可互换）。
    fn awaiting_retry(inner: &Inner, key: Id, root: &str) -> (Option<usize>, bool) {
        let mut at = None;
        let mut held = false;
        if let Some(rounds) = inner.sessions.get(&key) {
            for (index, entry) in rounds.iter().enumerate() {
                if entry.retry_of.is_empty() {
                    continue;
                }
                held = true;
                if !root.is_empty() && entry.retry_of == root {
                    at = Some(index);
                }
            }
        }
        (at, held)
    }

    /// 一个 task id 绑在哪一轮上（上游 `boundLocked`）。
    fn bound(inner: &Inner, key: Id, task_id: &str) -> Option<usize> {
        if task_id.is_empty() {
            return None;
        }
        inner
            .sessions
            .get(&key)?
            .iter()
            .position(|entry| entry.task_id == task_id)
    }

    /// 一个 run 的轮次是不是已经被取走过了（上游 `finishedLocked`）。
    fn finished(inner: &Inner, key: Id, task_id: &str) -> bool {
        inner
            .finished
            .get(&key)
            .is_some_and(|ring| ring.has(task_id))
    }

    // ---- 写面 ----

    /// 登记一条消息的气泡，并说这条消息是不是**画它的人**（上游 `open`）。
    ///
    /// 一条在某个轮次还在收集时到达的消息（已画、还没有 run 绑上去）**加入**那一轮，
    /// 因为会回答它的那个去抖窗口就是那个气泡已经代表的那一个；在那种情况下再开一个气泡
    /// 就是一个谁也封不掉的气泡。否则它立刻开自己的一轮 —— 因为屏幕上什么都没有的等待
    /// 读起来就是一条丢了。
    ///
    /// 返回的序号是调用方在轮次上的句柄，**只**用于一种情况：开场帧被服务端**直接拒掉**
    /// （[`StreamStore::drop_round`]）。
    ///
    /// 上游在 `h.CreatedAt` 为零值时填 `now`；本仓的 [`Instant`] **没有零值** ⇒
    /// 调用方必须给一个真实的时刻（登记为 `docs/32` §33 的 D8）。
    pub fn open(&self, session: Id, handle: StreamHandle) -> (RoundSeq, OpenVerdict) {
        let mut inner = self.lock();
        self.sweep_locked(&mut inner);

        if let Some(index) = Self::collecting(&inner, session) {
            if let Some(entry) = inner
                .sessions
                .get(&session)
                .and_then(|rounds| rounds.get(index))
            {
                return (entry.seq, OpenVerdict::Joined);
            }
        }

        inner.seq += 1;
        let seq = RoundSeq(inner.seq);
        let created_at = handle.created_at;
        let entry = RoundEntry {
            seq,
            handle,
            painted: true,
            task_id: String::new(),
            ever_bound: false,
            retry_of: String::new(),
            created_at,
        };
        inner.sessions.entry(session).or_default().push(entry);
        // 在屏幕上什么都没有之前就排好队的 run，等的**正好是**这个。在这里把它们配起来
        // 而不是留给**下一个**气泡，正是让两个序列保持同步的东西。
        if let Some(task_id) = self.take_pending_locked(&mut inner, session) {
            if let Some(rounds) = inner.sessions.get_mut(&session) {
                if let Some(last) = rounds.last_mut() {
                    last.task_id = task_id;
                    last.ever_bound = true;
                }
            }
        }
        (seq, OpenVerdict::Opened)
    }

    /// 记下这个会话排了一个 run，并把它交给属于它的那一轮：最老的、还在等 run 的那个
    /// （上游 `bindNext`）。此后每一个 task 生命周期事件都按 task id 找到它的气泡。
    ///
    /// 没有轮次在等的 run 会进会话的 `pending` 队列而不是被丢掉，因为路由把 ingest
    /// goroutine 脱离了，而一个会话的第一条消息是在 `dispatch` **里面**入队它的 task 的 ⇒
    /// 事件**经常**比它所属的气泡先到。
    pub fn bind_next(&self, session: Id, task_id: &str) {
        if task_id.is_empty() {
            return;
        }
        let mut inner = self.lock();
        self.sweep_locked(&mut inner);

        // 已经结束的 run 绝不许拿一个气泡：那会是一个没有收尾还剩下来封它的气泡。
        if Self::finished(&inner, session, task_id) {
            return;
        }
        if Self::bound(&inner, session, task_id).is_some() {
            return; // 已在册；重发的 queued 事件什么都不改
        }
        // **先一个从没绑过的轮次，只有在没人等的时候才用被释放的那个。** 两次查表必须对
        // "一个被释放的轮次"给出一致的答案，否则两个轮次会被**交叉接错**：
        // `collecting` 已经拒了它（新消息不得加入一个替代 run 在路上的轮次），所以一个
        // 新问题会开自己的一轮 —— 而如果这次 bind 把**那个问题**的 run 交给了**被释放的**
        // 轮次，两个轮次就会各自封掉对方的气泡。按真实退避顺序驱动，那就是两个人的回答
        // 被**静默对调**。
        let chosen = Self::collecting(&inner, session).or_else(|| Self::unbound(&inner, session));
        if let Some(index) = chosen {
            if let Some(rounds) = inner.sessions.get_mut(&session) {
                if let Some(entry) = rounds.get_mut(index) {
                    entry.retry_of.clear();
                    task_id.clone_into(&mut entry.task_id);
                    entry.ever_bound = true;
                    return;
                }
            }
        }
        let queue = inner.pending.entry(session).or_default();
        if queue.iter().any(|pending| pending.task_id == task_id) {
            return;
        }
        queue.push(PendingRun {
            task_id: task_id.to_owned(),
            at: self.clock(),
        });
    }

    /// 把一个轮次放回队里、等那个会替换它的 run，并报告有没有找到那一轮
    /// （上游 `retryUnbind`）。
    ///
    /// 平台对一个可重试失败的答案是造一个**自动重试的 clone**：一个**新的** task 行、
    /// 新 id、自己发一个 `task:queued`。clone 的 id 是它的收尾**唯一**会带的名字，
    /// 而它属于的轮次就是**这一个** —— 所以这一轮交出去死掉那次尝试的 id、回去等。
    ///
    /// 两种发布顺序都成立（上游逐字：`FailTask` 在 parent 的 `task:failed` **之前**发
    /// clone 的 `task:queued`，而退避子任务可能几分钟后才被延迟清扫器入队）：早到，clone
    /// 已经在 `pending` 队列里、在这里被取走；晚到，它发现这一轮还没绑、那时再取。
    ///
    /// 气泡**故意**不动：用户正看着一个转圈，他那个问题的回答还在路上。
    pub fn retry_unbind(&self, session: Id, task_id: &str) -> bool {
        if task_id.is_empty() {
            return false;
        }
        let mut inner = self.lock();
        let Some(index) = Self::bound(&inner, session, task_id) else {
            return false;
        };
        if let Some(clone) = self.take_pending_locked(&mut inner, session) {
            if let Some(entry) = inner
                .sessions
                .get_mut(&session)
                .and_then(|rounds| rounds.get_mut(index))
            {
                entry.task_id = clone;
                entry.ever_bound = true;
            }
            return true;
        }
        // 还没有 clone：这一轮回去等，但**不**进"下一个 `task:queued` 从里面取"的那个池子
        // —— 一个 clone 的 queued 事件与一个新问题的**逐字节相同**，池子会把这个轮次交给
        // 先到的那个，而按真实顺序驱动那就是两个轮次交叉接错、各自封掉对方的气泡。
        // 它改为**记下自己在等谁的名字**：clone 继承父亲的 `chat_input_task_id`，
        // 而这个 id **就是** clone 会解析到的 root。
        if let Some(entry) = inner
            .sessions
            .get_mut(&session)
            .and_then(|rounds| rounds.get_mut(index))
        {
            entry.task_id.clear();
            task_id.clone_into(&mut entry.retry_of);
        }
        true
    }

    /// 把一个气泡还回去：绑在它上面的 run 结果**不是**这个 adapter 会封的那一个 ——
    /// 一条在 Multica 里敲的问题从 `task:queued` 上把这个房间的轮次拿走了
    /// （上游 `releaseRound`）。
    ///
    /// 与 `retry_unbind` 不同，没有替代者要来，所以这一轮**真的**回去等：房间自己的 run
    /// （它发现轮次被占了、于是去了 `pending` 队列）如果还在等就被绑上来，否则这个会话的
    /// 下一个 `task:queued` 取走它。
    ///
    /// `ever_bound` **保持设置**：这一轮处在两个 run 之间而不是在收集，所以新消息不得加入它。
    pub fn release_round(&self, session: Id, task_id: &str) -> bool {
        if task_id.is_empty() {
            return false;
        }
        let mut inner = self.lock();
        let Some(index) = Self::bound(&inner, session, task_id) else {
            return false;
        };
        let next = self.take_pending_locked(&mut inner, session);
        if let Some(entry) = inner
            .sessions
            .get_mut(&session)
            .and_then(|rounds| rounds.get_mut(index))
        {
            entry.task_id.clear();
            if let Some(next) = next {
                entry.task_id = next;
                entry.ever_bound = true;
            }
        }
        true
    }

    /// 交出最老的、**从没成为 run** 的轮次 —— 一次 flush 落定后开的、再没有东西会回答的
    /// 那个气泡（上游 `takeOldestUnbound`）。被 `retry_unbind` 释放的轮次不是这种：
    /// 它的替代者在路上。
    pub fn take_oldest_unbound(&self, session: Id) -> Option<RoundTurn> {
        let mut inner = self.lock();
        self.sweep_locked(&mut inner);
        let index = Self::collecting(&inner, session)?;
        Some(Self::take_at_locked(
            &mut inner,
            session,
            index,
            self.max_age,
            &self.now,
        ))
    }

    /// 记下一个 run 结束了、却没为它取一轮：把它从 `pending` 队列里丢掉、退休它的 id
    /// （上游 `forget`）。
    ///
    /// 正是它挡住"气泡之前到达的收尾"（一次取消、一次不是本进程该宣布的失败）留下一个
    /// 排在 `pending` 队列里的 run —— 而**下一个**问题的气泡会把自己绑上去、转圈而再没有
    /// 东西能封它。
    pub fn forget(&self, session: Id, task_id: &str) {
        if task_id.is_empty() {
            return;
        }
        let mut inner = self.lock();
        if !inner.sessions.contains_key(&session) && !inner.pending.contains_key(&session) {
            // 这个会话什么都没有在册，那就没什么可忘、也没有理由开始记 ——
            // `task:failed` 对部署里**每一个** run 都会发；为陌生人的会话留一个环就是泄漏。
            return;
        }
        drop_pending_locked(&mut inner, session, task_id);
        self.retire_locked(&mut inner, session, task_id);
    }

    /// 取走 `key` 命名的那一轮，并交出它的气泡（上游 `take`）。
    ///
    /// 第二个返回值说**到底有没有**在册的轮次：`false` 是一次本进程什么都没有的 run ——
    /// 重启之前的一轮、从没开过气泡的一轮、或者已经被另一个收尾器取走的一轮。
    ///
    /// `resolve` 是自动重试的血缘查询，至多被查一次，且只在"事件上的 id 在一个还有轮次开着的
    /// 会话里匹配不到任何东西"时才查。
    ///
    /// # Errors
    ///
    /// 目前不返回错误；签名留 `Result` 是为了让血缘查询的失败有地方去而不必 panic。
    pub async fn take(
        &self,
        session: Id,
        key: &RoundKey,
        resolve: Option<&dyn RootResolver>,
    ) -> (Option<RoundTurn>, bool) {
        let awaiting_retry = {
            let mut inner = self.lock();
            self.sweep_locked(&mut inner);
            // 无论别的怎样，这个 run 结束了：不许把它留在等一个只会让它搁浅的气泡。
            drop_pending_locked(&mut inner, session, &key.task_id);
            Self::awaiting_retry(&inner, session, "").1
        };

        // **等一个具名尝试的轮次由血缘解开，而不是由 `task:queued` 猜出来的东西解开。**
        // clone 的 queued 事件命名不了它属于的轮次，所以 `bind_next` 可能把这个 run 绑到了
        // **一个更新**的问题的轮次上；它的 `chat_input_task_id` 能命名它，而那才是权威。
        // 先在**查中就查**，因为这条路径上第一次查表可能以**错的**轮次成功。
        if awaiting_retry && !key.task_id.is_empty() {
            if let Some(resolver) = resolve {
                if let Some(root) = resolver.root_task_id(&key.task_id).await {
                    let mut inner = self.lock();
                    if let (Some(index), _) = Self::awaiting_retry(&inner, session, &root) {
                        let turn = Self::take_at_locked(
                            &mut inner,
                            session,
                            index,
                            self.max_age,
                            &self.now,
                        );
                        return (Some(turn), true);
                    }
                }
            }
        }

        let worth_resolving = {
            let mut inner = self.lock();
            if let Some(index) = Self::index_locked(&inner, session, key) {
                let turn =
                    Self::take_at_locked(&mut inner, session, index, self.max_age, &self.now);
                return (Some(turn), true);
            }
            if inner.sessions.contains_key(&session) || inner.pending.contains_key(&session) {
                self.retire_locked(&mut inner, session, &key.task_id);
            }
            !key.task_id.is_empty()
                && inner
                    .sessions
                    .get(&session)
                    .is_some_and(|rounds| !rounds.is_empty())
        };

        if !worth_resolving {
            return (None, false);
        }
        let Some(resolver) = resolve else {
            return (None, false);
        };
        let Some(root) = resolver.root_task_id(&key.task_id).await else {
            return (None, false);
        };
        if root.is_empty() || root == key.task_id {
            return (None, false);
        }
        let mut inner = self.lock();
        if let Some(index) = Self::index_locked(&inner, session, &by_task(root)) {
            let turn = Self::take_at_locked(&mut inner, session, index, self.max_age, &self.now);
            return (Some(turn), true);
        }
        (None, false)
    }

    /// 这个存储**在任何地方**有没有东西在册 —— 一轮（画过没画过都算）、或者一个还在等
    /// 它气泡的 run（上游 `holding`）。它是两个收尾订阅者开头那句"这里没有东西要封"的判据。
    ///
    /// 没画过的轮次与 `pending` 的 run **都算**，而这就是重点：`depth()` 按"画过"筛，
    /// 因为它回答的是"屏幕上几个气泡"；而一个气泡还在路上的 run 正是**最不许**被丢掉的
    /// 那个。
    ///
    /// # Panics
    ///
    /// 不会 panic：`Mutex` 的中毒被就地解开。
    #[must_use]
    pub fn holding(&self) -> bool {
        let inner = self.lock();
        inner.sessions.values().any(|rounds| !rounds.is_empty()) || !inner.pending.is_empty()
    }

    /// 忘掉一轮而不发任何东西 —— 用于开场帧被拒、句柄描述的那个气泡从未存在的场合
    /// （上游 `drop`）。`seq` 是 `open` 交回来的那个。
    pub fn drop_round(&self, session: Id, seq: RoundSeq) {
        let mut inner = self.lock();
        let Some(rounds) = inner.sessions.get_mut(&session) else {
            return;
        };
        rounds.retain(|entry| entry.seq != seq);
        if rounds.is_empty() {
            inner.sessions.remove(&session);
        }
    }

    /// 屏幕上开了几个气泡（诊断与用例用，上游 `depth`）。
    #[must_use]
    pub fn depth(&self) -> usize {
        let inner = self.lock();
        inner
            .sessions
            .values()
            .flat_map(|rounds| rounds.iter())
            .filter(|entry| entry.painted)
            .count()
    }

    /// @param 一个轮次在册吗（诊断与用例用）。
    #[must_use]
    pub fn has_round(&self, session: Id, task_id: &str) -> bool {
        let inner = self.lock();
        Self::bound(&inner, session, task_id).is_some()
    }

    // ---- 内部 ----

    fn expired(&self, created_at: Instant, inner_now: Instant) -> bool {
        inner_now.saturating_duration_since(created_at) > self.max_age
    }

    /// 取走第 `index` 轮并交出它剩下的东西：气泡，如果还写得进去（上游 `takeAtLocked`）。
    ///
    /// 条目**无条件**消失。在**发现它的那把锁**里移除它，才是两个收尾器抢同一个 run 时的
    /// 互斥 —— 谁先到这里，谁就是唯一见过句柄的人，于是一个 run 产生**一帧**收尾帧。
    /// 没有气泡的轮次报"没有"，过了 `max_age` 的句柄也报"没有"：服务端会拒那一帧，而一个
    /// 相信自己有气泡的调用方会把用户留在什么都没有里。
    ///
    /// run 进会话的 `finished` 环，这样一个"已经被取走的 run 的"重发 `task:queued`
    /// 就不能绑上某个后来问题打开的气泡。
    fn take_at_locked(
        inner: &mut Inner,
        session: Id,
        index: usize,
        max_age: Duration,
        now: &Clock,
    ) -> RoundTurn {
        let rounds = inner
            .sessions
            .get_mut(&session)
            .expect("index came from this session's own round list");
        let entry = rounds.remove(index);
        if rounds.is_empty() {
            inner.sessions.remove(&session);
        }
        retire(inner, session, &entry.task_id, now);
        let live = (now)().saturating_duration_since(entry.created_at) <= max_age;
        let has_bubble = entry.painted && live;
        RoundTurn {
            handle: entry.handle,
            has_bubble,
        }
    }

    /// 找一次收尾替哪一轮说话（上游 `indexLocked`）。匹配**只按**调用方带进来的那个名字，
    /// 没有**位置上的**退路：一个不在册的 run 在这里没有气泡，而取走别人的会把**错的**
    /// 问题用这个回答封掉。
    fn index_locked(inner: &Inner, session: Id, key: &RoundKey) -> Option<usize> {
        if key.task_id.is_empty() {
            return None;
        }
        inner
            .sessions
            .get(&session)?
            .iter()
            .position(|entry| entry.task_id == key.task_id)
    }

    /// 记下"这个 run 的轮次已经被取走"，并让环保持有界（上游 `retireLocked`）。
    fn retire_locked(&self, inner: &mut Inner, session: Id, task_id: &str) {
        retire(inner, session, task_id, &self.now);
    }

    /// 排干 `pending` 队列里最老的那一个（上游 `takePendingLocked`）。
    /// 取之前先丢掉等过自己那个钟的：队首一个被放弃的 run 否则会把自己交给一个**晚得多**
    /// 才开的气泡。
    fn take_pending_locked(&self, inner: &mut Inner, session: Id) -> Option<String> {
        let now = self.clock();
        let max_age = self.pending_max_age;
        let queue = inner.pending.get_mut(&session)?;
        queue.retain(|pending| now.saturating_duration_since(pending.at) <= max_age);
        if queue.is_empty() {
            inner.pending.remove(&session);
            return None;
        }
        let task_id = queue.remove(0).task_id;
        if queue.is_empty() {
            inner.pending.remove(&session);
        }
        Some(task_id)
    }

    /// 驱逐服务端已经不会接受的轮次、等了太久的气泡的 run，以及安静了一整个窗口的会话的
    /// `finished` 环（上游 `sweepLocked`）。调用方持有锁。
    fn sweep_locked(&self, inner: &mut Inner) {
        let now = self.clock();
        let max_age = self.max_age;
        let pending_max_age = self.pending_max_age;
        inner.sessions.retain(|_, rounds| {
            rounds.retain(|entry| now.saturating_duration_since(entry.created_at) <= max_age);
            !rounds.is_empty()
        });
        inner.pending.retain(|_, queue| {
            queue.retain(|pending| now.saturating_duration_since(pending.at) <= pending_max_age);
            !queue.is_empty()
        });
        inner.finished.retain(|_, ring| {
            ring.at
                .is_some_and(|at| now.saturating_duration_since(at) <= max_age)
        });
    }

    /// 写一个气泡的收尾帧，并且是"收尾帧重试策略"**唯一**在的地方（上游 `seal`）。
    /// 每个收尾器都走它：回答、失败与取消通知、以及落定的 flush。
    ///
    /// 策略（上游对活 bot 实测的逐字）：
    ///
    /// - 一个**永远不来的 ack**（[`super::ws_sender::SenderError::StreamAckTimeout`]）
    ///   对"帧到没到"什么都没说，而断开之前刚写的一帧**确实不会**到。重发**同一帧**无论
    ///   第一次到没到都被 `errcode 0` 接受 ⇒ 帧再写，最多再写 [`STREAM_CLOSE_RETRIES`] 次，
    ///   每次相隔 [`STREAM_CLOSE_RETRY_DELAY`]。**选定的那一侧**：在只是丢了一个 ack 之后
    ///   重试，可能把一帧用户已经见过的收尾帧再放到他眼前 —— 同一条流上的同样内容，
    ///   客户端就地渲染。这比"断开前写过的回答从没到、也从没再发"要好。
    /// - 一次**来自服务端的判决**（`stream_unusable`：`846605` / `846608`）就结束它：
    ///   这条流再也不会接受一帧，调用方退回普通消息。
    /// - `StreamBusy` 与 `StreamSuperseded` **不重试**。Busy 不可能发生在收尾帧上
    ///   （它们排队）；Superseded 意味着另一个收尾器先封了这条流，回答是**它的**。
    /// - `deadline` 到点或流自己的窗口没了，重试就停。
    ///
    /// 无论尝试几次，结束**只被计一次**（上游 `recordEnding` 的位置）。
    ///
    /// # Errors
    ///
    /// 最后一次尝试的错误（或 `deadline` 造成的放弃）。
    pub async fn seal(
        &self,
        sender: &dyn StreamSender,
        handle: &StreamHandle,
        text: &str,
    ) -> Result<(), super::ws_sender::SenderError> {
        use super::ws_sender::SenderError;
        let mut outcome: Result<(), SenderError> = Ok(());
        for attempt in 0..=STREAM_CLOSE_RETRIES {
            // 第一次是一次普通帧、像普通帧一样排队。此后每一次都是**同一帧再写一遍**，
            // 被那道门放过去：拦住它会让第一帧没人应答、并把回答用普通路径再发一次。
            let result = if attempt == 0 {
                sender.stream(handle, text, true).await
            } else {
                sender.stream_rewrite(handle, text, true).await
            };
            match result {
                Ok(()) => {
                    outcome = Ok(());
                    break;
                }
                Err(error) if matches!(error, SenderError::StreamAckTimeout) => {
                    outcome = Err(error);
                }
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
            if attempt == STREAM_CLOSE_RETRIES {
                break;
            }
            if self.expired(handle.created_at, self.clock()) {
                break;
            }
            if self.close_retry_delay > Duration::ZERO {
                tokio::time::sleep(self.close_retry_delay).await;
            }
        }
        sender.record_ending(outcome.as_ref().err());
        outcome
    }
}

#[cfg(test)]
mod tests;
