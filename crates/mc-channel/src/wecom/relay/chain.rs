//! **重投递链**：一个需要再试一次的帧**不回队列尾部**，它停在自己那条安装线的队首。
//!
//! 本文件是 `relay.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分
//! （上游 1,578 行 ⇒ 按「重投递链 / 优先级队列」拆），逐条清单见 `docs/32` §34 的 D9。
//!
//! # 为什么"还回去"必须由**放弃它的那一方**重投递（上游逐字，本仓照抄）
//!
//! 一个被释放的 claim **除了本进程之外没有别人可以重试它**：每个副本对一条给定的流帧都只读
//! 一次、只在重启时重放，所以"释放出来让持有者拿"会在持有者已经处理过它那份副本时把这条回复
//! 搁浅 —— 它在 claim 竞争里输了、返回了，而再也没有东西会把它叫醒。
//! ⇒ **放弃的一方就是重投递的一方**，按 [`RelayConfig::retry_plan`]，直到租约落定。
//!
//! # 线的形状
//!
//! [`Hold::items`]`[0]` 是正在重试的那个帧；它后面的一切到达得更晚，**不许超车**。
//! 一条线只属于**一个** worker 任务，所以它不加锁（上游逐字：`hold` 由一个 worker 独占）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::queue::Queued;
use super::{RelayBudget, RelayOutbound};

/// 两条投递之间最长的暂停（用它当"没有待办"的哨兵时刻）。
const FAR_FUTURE: Duration = Duration::from_secs(3600);

/// 一条安装的队列，当它的队首在等一次重投递时（上游 `hold`）。
///
/// # 不变量
///
/// - `items` 非空时 `items[0]` 是**正在重试**的那个帧；
/// - `ready_at` 是 `items[0]` 下一次可以被 offer 的时刻；
/// - 一条线只被一个 worker 碰 ⇒ 没有锁、也没有原子。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hold {
    /// 队首是正在重试的帧，后面的是到达更晚、不许超车的帧。
    pub items: Vec<Queued>,
    /// 队首下一次可以被 offer 的时刻。
    pub ready_at: Option<Instant>,
}

impl Hold {
    /// 一条只含一个帧、已经可以再 offer 的线。
    #[must_use]
    pub fn due_now(item: Queued) -> Self {
        Self {
            items: vec![item],
            ready_at: None,
        }
    }

    /// 这个帧到达时，它前面那个什么时候可以被 offer。
    #[must_use]
    pub fn due_at(&self) -> Option<Instant> {
        self.ready_at
    }
}

/// 一个 worker 手上的全部安装线（上游 `lines map[string]*hold`）。
///
/// 包一层而不是裸 `HashMap`：`HashMap` 的迭代顺序是随机的，而排空与到期处理需要一个**确定性**
/// 的顺序（否则"两条回答的顺序"会随每次运行的哈希种子变 —— 那正是这份顺序存在要防的东西）。
#[derive(Debug, Default)]
pub struct Lines {
    holds: HashMap<String, Hold>,
    /// 插入顺序（只用于给出一个确定的排空顺序）。
    order: Vec<String>,
}

impl Lines {
    /// 空的线集。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 这个安装有线吗。
    #[must_use]
    pub fn get(&self, installation_id: &str) -> Option<&Hold> {
        self.holds.get(installation_id)
    }

    /// 可变取（`offer` / `park` 用）。
    pub fn get_mut(&mut self, installation_id: &str) -> Option<&mut Hold> {
        self.holds.get_mut(installation_id)
    }

    /// 记一条线。第一个进来的安装排在最前（排空顺序因此可复现）。
    pub fn insert(&mut self, installation_id: &str, hold: Hold) {
        if !self.holds.contains_key(installation_id) {
            self.order.push(installation_id.to_string());
        }
        self.holds.insert(installation_id.to_string(), hold);
    }

    /// 丢一条线。
    pub fn remove(&mut self, installation_id: &str) -> Option<Hold> {
        self.order.retain(|id| id != installation_id);
        self.holds.remove(installation_id)
    }

    /// 有多少条线。
    #[must_use]
    pub fn len(&self) -> usize {
        self.holds.len()
    }

    /// 一条线都没有吗。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.holds.is_empty()
    }

    /// 按插入顺序列出线（排空用：确定、且与"谁先到"一致）。
    #[must_use]
    pub fn in_order(&self) -> Vec<(String, Hold)> {
        self.order
            .iter()
            .filter_map(|id| self.holds.get(id).map(|hold| (id.clone(), hold.clone())))
            .collect()
    }

    /// 最早的一个"队首可以再 offer"的时刻（上游 `earliestDue`）。
    ///
    /// 只看 `items[0]`：线后面的帧没有自己的时刻，它们等队首。
    #[must_use]
    pub fn earliest_due(&self) -> Option<Instant> {
        self.holds
            .values()
            .filter(|hold| !hold.items.is_empty())
            .filter_map(Hold::due_at)
            .min()
    }

    /// 队首已经到期的安装（上游 `fireDue` 的遍历；顺序确定）。
    #[must_use]
    pub fn due_installations(&self, now: Instant) -> Vec<String> {
        self.order
            .iter()
            .filter(|id| {
                self.holds.get(*id).is_some_and(|hold| match hold.due_at() {
                    Some(due) => due <= now,
                    // 还没等过（`ready_at = None`）⇒ 立刻到期。
                    None => !hold.items.is_empty(),
                })
            })
            .cloned()
            .collect()
    }
}

/// 上游 `earliestDue` 的自由函数形态（`relay.rs` 再导出它，供用例与队列的 select 用）。
#[must_use]
pub fn earliest_due(lines: &Lines) -> Option<Instant> {
    lines.earliest_due()
}

/// 把"最多等多久"翻成一个 `sleep` 要的时长（没有待办就睡很久）。
#[must_use]
pub(crate) fn wait_until(due: Option<Instant>, now: Instant) -> Duration {
    match due {
        Some(due) => due.saturating_duration_since(now).min(FAR_FUTURE),
        None => FAR_FUTURE,
    }
}

impl RelayOutbound {
    /// 上游 `offer`：把一个帧从分片队列上取下来。
    ///
    /// 它的安装已经有一条在等重投递的线时，它加入那条线的**尾部**：超到它前面那个帧之前，
    /// 正是这份顺序存在要防的重排。
    pub(crate) async fn offer(&self, lines: &mut Lines, item: Queued) {
        let installation = item.frame.installation_id.clone();
        if let Some(hold) = lines.get_mut(&installation) {
            if hold.items.len() >= self.config().depth() {
                self.shed(&item, "the installation's line is full behind a retry");
                return;
            }
            hold.items.push(item);
            return;
        }
        self.step(lines, item).await;
    }

    /// 上游 `step`：跑一次投递，并归档它对这条线意味着什么。
    ///
    /// 需要再 offer 一次的帧停在自己那条安装线的**队首**，带上下一次延迟；一个完事的帧让排在
    /// 它后面的一切往前提。
    pub(crate) async fn step(&self, lines: &mut Lines, item: Queued) {
        let installation = item.frame.installation_id.clone();
        if self.perform(&item, None).await {
            RelayOutbound::advance(lines, &installation);
            return;
        }
        let Some(delay) = self.delay_for(item.attempts) else {
            // 链耗尽最可能的理由**远远**是"另一个副本已经投递了、所以 claim 被握着"，
            // 所以这里**不**算一次丢弃。发布方的结局观察是那个给"没人投递的回复"命名的东西，
            // 而它只命名一次。
            tracing::warn!(
                installation_id = installation.as_str(),
                task_id = item.frame.task_id.as_str(),
                attempts = item.attempts + 1,
                "wecom relay: giving up on a routed delivery after retries"
            );
            RelayOutbound::advance(lines, &installation);
            return;
        };
        let mut item = item;
        item.attempts += 1;
        match lines.get_mut(&installation) {
            Some(hold) => {
                // 它就在队首（`step` 只会被队首调）。
                hold.items[0] = item;
                hold.ready_at = Some(Instant::now() + delay);
            }
            None => {
                lines.insert(
                    &installation,
                    Hold {
                        items: vec![item],
                        ready_at: Some(Instant::now() + delay),
                    },
                );
            }
        }
    }

    /// 上游 `advance`：让一个完事的队首退休，并让这条安装的下一个帧立刻到期。
    pub(crate) fn advance(lines: &mut Lines, installation: &str) {
        // 它不读 `self`：这是**纯**的线记账（`unused_self` 的判据）。
        let Some(hold) = lines.get_mut(installation) else {
            // 它直接从队列上下来、从没进过一条线。
            return;
        };
        hold.items.remove(0);
        if hold.items.is_empty() {
            lines.remove(installation);
            return;
        }
        // 立刻到期（上游 `time.Time{}`）：下一轮循环会捡起它。
        hold.ready_at = None;
    }

    /// 上游 `fireDue`：把每一条队首已经等完退避的线 offer 出去。
    pub(crate) async fn fire_due(&self, lines: &mut Lines) {
        let now = Instant::now();
        for installation in lines.due_installations(now) {
            let Some(item) = lines
                .get(&installation)
                .and_then(|hold| hold.items.first().cloned())
            else {
                lines.remove(&installation);
                continue;
            };
            self.step(lines, item).await;
        }
    }

    /// 上游 `delayFor`：下一次 offer 等多久，以及**还有没有下一次**。
    #[must_use]
    pub(crate) fn delay_for(&self, attempts: usize) -> Option<Duration> {
        self.retry_plan().get(attempts).copied()
    }

    /// 上游 `shed`：记一个因为队列没地方而被拒的帧。
    ///
    /// 它是一次**准入**决定，仅此而已：按 kind 打标签的 `relay_shed`，记在**拒了它的那个副本**
    /// 上。
    ///
    /// 它**刻意不碰**回复计数器。每个副本都读每一条帧，所以没有任何副本能从这里判断一次削减
    /// 有没有让用户损失什么 —— 削减的那个副本可能并不是会去发它的那个，而在一次租约交接期间
    /// 两个副本可以同时握着发送者，于是连"我握着 socket 吗"都不是"我是唯一能发的那个"的证明。
    /// 每个副本各自回答这个问题，正是"一条回复被同时数成送到与丢弃"的来处。
    ///
    /// 所以回复的结局留在那个**唯一**能在事后结算它的所有者手里：发布方的 `watch_outcomes`，
    /// 它问"到底有没有**任何**副本认领过这次投递"，没人认领就记一次丢失。一次**真的**让用户
    /// 损失了这条回复的削减，会以"没人认领的投递"到达那个所有者，并在那里被计数。
    pub(crate) fn shed(&self, item: &Queued, why: &str) {
        self.mx().record_relay_shed(item.frame.kind.as_str());
        tracing::warn!(
            kind = item.frame.kind.as_str(),
            installation_id = item.frame.installation_id.as_str(),
            task_id = item.frame.task_id.as_str(),
            why,
            "wecom relay: shedding a routed delivery"
        );
    }

    /// 上游 `drainRemaining`：把队列里已经有的东西执行掉，让一次优雅停机不会把一条正有人在等的
    /// 回复搁浅 —— 在**一个**给整次排空的界之下，这样一条满的 shard 不会叠出串行的 ack 等待。
    ///
    /// `first` 是取消赢了赛跑时 worker 已经从队列上拿下来的那个项。停在一条线上等重投递的帧
    /// **先走、且不等退避**：那份等待存在的意义是给租约时间搬家，而停机已经超过去了。
    pub(crate) async fn drain_remaining(
        &self,
        lines: &mut Lines,
        shard: usize,
        first: Option<Queued>,
    ) {
        let budget = RelayBudget::lasting(self.config().drain_budget());
        let deadline = budget.deadline();
        if let Some(first) = first {
            // 加入它那条安装线的**尾部**，而不是插队：这个帧是在那条线队首那个**之后**从队列上
            // 拿下来的，先发它会把两条回答在用户的聊里颠倒过来 —— 正是 `offer` 存在要防的重排，
            // 在出去的路上被撤销。那条安装没有线时，前面也就没有可超的东西。
            match lines.get_mut(&first.frame.installation_id) {
                Some(hold) => hold.items.push(first),
                None => {
                    self.perform(&first, Some(deadline)).await;
                }
            }
        }
        let ordered = lines.in_order();
        for (installation, hold) in ordered {
            for item in hold.items {
                if budget.exceeded(Instant::now()) {
                    return;
                }
                self.perform(&item, Some(deadline)).await;
            }
            lines.remove(&installation);
        }
        if let Some(queue) = self.queues().get(shard) {
            while let Some(item) = queue.pop() {
                if budget.exceeded(Instant::now()) {
                    return;
                }
                self.perform(&item, Some(deadline)).await;
            }
        }
    }

    /// 上游 `perform`：在一条至多一次的 claim 之下跑一次投递，并报"这一帧**完事了吗**"。
    ///
    /// `false` 意味着它还欠一次 offer，调用方把它排在自己那条安装线的队首。
    ///
    /// # 归属**先于**全局 claim
    ///
    /// claim 是一次跨副本的 `SET NX`：每个副本读每一条帧，而如果一个**发不出去**的副本赢了它，
    /// 那个能发的副本就会输掉竞争、认定"别人拿着它"而返回 —— 与此同时赢家发现自己没有 socket、
    /// 把键释放掉，而**再也没有东西会去叫醒输家**。每一方都表现得正确，这条回复仍然丢了。
    /// 所以一个副本只在确立"自己能honour 它"之后才去争这次 claim。socket 仍然可能在这个检查
    /// 与发送之间消失；那段窗口正是 `deliver_relayed` 再查一次、以及 `NotOurs` 结局必须把 claim
    /// 还回去的原因。
    pub(crate) async fn perform(&self, item: &Queued, deadline: Option<Instant>) -> bool {
        let Some(handler) = self.handler() else {
            // handler 还没附着：这一帧还欠一次 offer（`start` 之前的帧本来就该在队列里等）。
            return false;
        };
        if !handler.owns_socket(&item.frame.installation_id) {
            return true;
        }
        if !self.seen.claim(&item.event_id) {
            return true;
        }
        let key = super::dedupe_key(&item.event_id);
        let token = self.token_for(&item.event_id);
        if let Some(dedupe) = self.dedupe.as_ref() {
            match dedupe.claim(&key, &token, self.dedupe_ttl()).await {
                Err(message) => {
                    // 一个答不上来的去重存储**不许**静默变成一条至少一次路径：聊里一条重复的回答
                    // 比一条迟到的更糟。但一个正在跑的进程也不会被外部重试 —— 重放只在重启时发生
                    // —— 所以这次重试是我们的。
                    self.seen.forget(&item.event_id);
                    tracing::warn!(
                        error = message.as_str(),
                        installation_id = item.frame.installation_id.as_str(),
                        "wecom relay: dedupe unavailable, retrying locally"
                    );
                    return false;
                }
                Ok(false) => {
                    // 输掉 claim **不**意味着别人会投递它。在一次飞在半路的租约搬家时，这里的输家
                    // 可能正是**此刻**握着 socket 的那个副本，而赢家即将发现自己不再握有它并释放
                    // —— 而每个副本对一条流帧都只读一次，所以没有任何外部的东西会把它还回来。
                    // 因此输家再查一次，有界且带退避；常见情况（claim 被握着是因为这条回复已经
                    // 投递了）会把链烧完然后停下。
                    self.seen.forget(&item.event_id);
                    return false;
                }
                Ok(true) => {}
            }
        }
        // `delivery_budget` 是发布方的结局宽每次 offer 收的那一份，所以它就得是这次投递**真正**
        // 拿到的那一份。一次 offer 一个预算而不是整条链一个，因为**这里**正是 claim 被取走又
        // 还回去的地方：一次以"可证明没发出"结束的 offer 会在下面几行释放 claim，
        // 而下一次 offer 到达这里、开一份全新的。
        //
        // 默认值是 `ACK_TIMEOUT`，所以什么都不配的部署看不到变化。在这里被切掉的投递以一条
        // 上下文错误结束，而 `unconfirmed_reason` 把它读成**结局未知**而不是失败 —— 当那次切割
        // 落在写之后时是对的，落在写之前时被标成确定（`SenderError::NotAttempted`）。
        let budget = match deadline {
            Some(deadline) => RelayBudget::at(deadline),
            None => RelayBudget::lasting(self.config().delivery_budget()),
        };
        let result = handler.deliver_relayed(&item.frame).await;
        if result.outcome == super::RelayOutcome::Done {
            // **完事了。** 持有者的记录只在 claim 说可以时才做：`Settle` 是这个副本 token 上的
            // compare-and-set，被拒意味着发布方已经在它的宽结束时把这条回复解成丢了 ——
            // 它有一个记录，所以这一个**不许**被做。这正是 `record` 是一个闭包、而不是在
            // `deliver_relayed` 里挪一下计数器的全部理由。
            if self.dedupe.is_some() && !self.settle_claim(&key, &token, &item.frame, budget).await
            {
                return true;
            }
            if let Some(record) = result.record {
                handler.record(record);
            }
            return true;
        }
        // 不是我们的，或者可证明从没写过：把 claim 还回去 —— 并且**本进程也再 offer 一次**，
        // 因为只有 release 是叫不醒任何人的（见上面的 `Ok(false)`）。
        //
        // `Release` 是这个副本 token 上的 compare-and-delete，所以重试安全，而且永远不可能取走
        // 一个更晚的持有者握着的 claim。它的三个答案都指向"再 offer 一次"：
        //   - 释放成功：下一次 offer 的 `Claim` 从一个空键开始；
        //   - 已经不是我们的了：删除已经落地，或者现在别的副本握着它 —— 下一次 `Claim` 决定是哪个；
        //   - 错误：**结局未知**。那个词上什么都不记。如果键还握着这个 token，下一次 `Claim`
        //     会重新取走它（同一个持有者）、投递再跑一次；如果删除真的落地了，下一次 `Claim`
        //     会新取一个，或者输给一个先到的副本 —— 那一个接着投递并记账，**一次**。
        // 一条被搁在这个 token 下的 claim（每次释放都出错、每次重 offer 都失败）正是发布方的
        // `Resolve` 在宽结束时找到的东西：被握着、而这个持有者什么都没记 —— 它在那里记下这次丢失，
        // **一次**。
        if let Some(dedupe) = self.dedupe.as_ref() {
            match dedupe.release(&key, &token).await {
                Err(message) => tracing::warn!(
                    error = message.as_str(),
                    kind = item.frame.kind.as_str(),
                    installation_id = item.frame.installation_id.as_str(),
                    task_id = item.frame.task_id.as_str(),
                    "wecom relay: claim release outcome unknown; the next offer re-claims"
                ),
                Ok(false) => tracing::debug!(
                    kind = item.frame.kind.as_str(),
                    installation_id = item.frame.installation_id.as_str(),
                    task_id = item.frame.task_id.as_str(),
                    "wecom relay: claim no longer ours at release"
                ),
                Ok(true) => {}
            }
        }
        self.seen.forget(&item.event_id);
        false
    }

    /// 上游 `settleClaim`：把一个已投递帧的 claim 标成已结算，并报"它的持有者可以记那个结局吗"。
    ///
    /// `false` 有两个含义，而只有一个是别处覆盖了的。
    ///
    /// **覆盖了的**：发布方已经把这条回复解成丢了。它的记录成立，这里再来一个会翻倍。
    ///
    /// **不总被覆盖**：每一次尝试都回来"未知"。持有者分不清存储处在哪个状态。如果那些结算
    /// 从没落地，claim 仍被这个 token 握着，而发布方的 `Resolve` 给这条回复结一次尾 ——
    /// 常见情况，也是不在这里计数的理由。但如果一次结算**真的**落地了、只是它的响应丢了，
    /// 键已经读作 settled，那个 `Resolve` 也就保持安静，于是这条回复以**没有任何记录**结束。
    /// 在这里记会把常见情况变成这整条路径存在要防的双计数，所以这个漏是**刻意**选的那一侧：
    /// 一个不断失败的存储下的监控缺口，而不是一条用户没拿到的回复。下面那条 warn 是让它可见的东西。
    pub(crate) async fn settle_claim(
        &self,
        key: &str,
        token: &str,
        frame: &super::RelayFrame,
        budget: RelayBudget,
    ) -> bool {
        let Some(dedupe) = self.dedupe.as_ref() else {
            return true;
        };
        let mut last_error: Option<String> = None;
        let mut made = 0usize;
        for attempt in 0..super::CLAIM_SETTLE_ATTEMPTS {
            if attempt > 0 {
                if super::settle_budget_spent(Some(budget.deadline()), Instant::now()) {
                    break;
                }
                // 停机不豁免这次结算：帧已经在用户的聊里了。所以这里只是**跳过等待**、
                // 不跳过尝试（上游逐字）。
                tokio::time::sleep(self.settle_retry_backoff()).await;
                if super::settle_budget_spent(Some(budget.deadline()), Instant::now()) {
                    break;
                }
            }
            made += 1;
            match dedupe.settle(key, token).await {
                Err(message) => last_error = Some(message),
                Ok(true) => return true,
                Ok(false) => {
                    tracing::warn!(
                        kind = frame.kind.as_str(),
                        installation_id = frame.installation_id.as_str(),
                        task_id = frame.task_id.as_str(),
                        "wecom relay: delivered after the publisher resolved the reply as lost; not counted again"
                    );
                    return false;
                }
            }
        }
        // 每一次被允许的尝试之后仍然未知。见上面那段文档：那些结算从没落地时，发布方给这条回复
        // 结一次尾；而其中一次落地、只是它的回答丢了时，没有东西给它结。**哪一种是真相，正是这里
        // 不可知的东西** ⇒ 结局留作未记录，而不是被记两次。
        tracing::warn!(
            error = last_error.as_deref(),
            attempts = made,
            kind = frame.kind.as_str(),
            installation_id = frame.installation_id.as_str(),
            task_id = frame.task_id.as_str(),
            detail = "the delivery itself reached the chat; if the settle landed and only its response was lost, \
                      the publisher's Resolve reads the claim as settled and stays quiet as well",
            "wecom relay: delivered, but the claim could not be settled"
        );
        false
    }
}
