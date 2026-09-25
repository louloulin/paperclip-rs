//! `stream_store` 的用例（上游 `wecom/stream_store_test.go` 的等价面）。
//!
//! 时钟是**注入**的（上游 `streamStore.now` 那个字段的同款理由）：十分钟的协议窗口与
//! 三十秒的 `pending` 界都靠手动拨表走完，用例一次都不真的等。
//!
//! 替补面：上游 `sendersRegistry` / `taskLookup` 在本片是两个**端口 trait**
//! （[`StreamSender`] / [`RootResolver`]），所以这里不需要任何数据库或 socket。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::*;
use crate::wecom::strings::Locale;

// =====================================================================
// 替补件
// =====================================================================

/// 手动时钟（上游 `s.now` 的注入面的同款用具）。
#[derive(Clone)]
struct ManualClock(Arc<Mutex<Instant>>);

impl ManualClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }

    fn advance(&self, by: Duration) {
        let mut guard = self.0.lock().unwrap();
        *guard += by;
    }

    fn clock(&self) -> Clock {
        let inner = Arc::clone(&self.0);
        Arc::new(move || *inner.lock().unwrap())
    }
}

fn fresh_session() -> Id {
    Id::new()
}

fn handle(clock: &ManualClock, stream_id: &str) -> StreamHandle {
    StreamHandle {
        req_id: "req-1".to_owned(),
        stream_id: stream_id.to_owned(),
        installation_id: Some(Id::new()),
        chat_id: "chat-1".to_owned(),
        chat_type: super::super::ws_frame::CHAT_TYPE_SINGLE_INT,
        locale: Locale::ZhHans,
        created_at: clock.now(),
    }
}

/// 一个拨表可走的存储（窗口即用例要的那个）。
fn store(clock: &ManualClock, max_age: Duration) -> StreamStore {
    StreamStore::new()
        .with_clock(clock.clock())
        .with_max_age(max_age)
        .with_pending_max_age(Duration::from_secs(30))
        .with_close_retry_delay(Duration::ZERO)
}

/// 一个能按脚本回答的收尾器（上游 `sendersRegistry` 的替身）。
#[derive(Default)]
struct FakeSender {
    calls: Mutex<Vec<(bool, bool)>>,
    script: Mutex<VecDeque<Result<(), super::super::ws_sender::SenderError>>>,
    endings: Mutex<Vec<Option<String>>>,
    ending_calls: AtomicUsize,
}

impl FakeSender {
    fn with_script(script: Vec<Result<(), super::super::ws_sender::SenderError>>) -> Self {
        let sender = Self::default();
        *sender.script.lock().unwrap() = script.into();
        sender
    }

    fn calls(&self) -> Vec<(bool, bool)> {
        self.calls.lock().unwrap().clone()
    }

    fn endings(&self) -> Vec<Option<String>> {
        self.endings.lock().unwrap().clone()
    }
}

#[async_trait]
impl StreamSender for FakeSender {
    async fn stream(
        &self,
        _handle: &StreamHandle,
        _text: &str,
        finish: bool,
    ) -> Result<(), super::super::ws_sender::SenderError> {
        self.calls.lock().unwrap().push((false, finish));
        self.script.lock().unwrap().pop_front().unwrap_or(Ok(()))
    }

    async fn stream_rewrite(
        &self,
        _handle: &StreamHandle,
        _text: &str,
        finish: bool,
    ) -> Result<(), super::super::ws_sender::SenderError> {
        self.calls.lock().unwrap().push((true, finish));
        self.script.lock().unwrap().pop_front().unwrap_or(Ok(()))
    }

    fn record_ending(&self, error: Option<&super::super::ws_sender::SenderError>) {
        self.ending_calls.fetch_add(1, Ordering::SeqCst);
        self.endings
            .lock()
            .unwrap()
            .push(error.map(std::string::ToString::to_string));
    }
}

// =====================================================================
// open / bind_next：两个序列
// =====================================================================

#[test]
fn the_first_message_opens_a_round_and_the_next_joins_it() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();

    let (seq, verdict) = store.open(session, handle(&clock, "s-1"));
    assert_eq!(verdict, OpenVerdict::Opened);
    assert_eq!(store.depth(), 1);

    // 一个还在收集的轮次（画了、还没绑 run）⇒ 第二条消息**加入**它：
    // 会回答它的那个去抖窗口就是那个气泡已经代表的那一个。
    let (joined_seq, verdict) = store.open(session, handle(&clock, "s-2"));
    assert_eq!(verdict, OpenVerdict::Joined);
    assert_eq!(joined_seq, seq);
    assert_eq!(
        store.depth(),
        1,
        "a second bubble here is one nobody closes"
    );
}

#[test]
fn a_message_does_not_join_a_round_that_already_had_a_run() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");

    // 绑过 run 的轮次不再收集：新消息是一个**新问题**，要有自己的气泡与 run。
    let (_, verdict) = store.open(session, handle(&clock, "s-2"));
    assert_eq!(verdict, OpenVerdict::Opened);
    assert_eq!(store.depth(), 2);
}

#[test]
fn a_message_does_not_join_a_round_that_was_released_for_a_retry() {
    // 上游 `everBound` 的那条区分：绑过一次、再被释放的轮次在**两个 run 之间**，
    // 不是在收集 ⇒ 新消息不得加入它（否则两个轮次会各自封掉对方的气泡）。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    assert!(store.retry_unbind(session, "task-1"));

    let (_, verdict) = store.open(session, handle(&clock, "s-2"));
    assert_eq!(verdict, OpenVerdict::Opened);
    assert_eq!(
        collecting_count(&store, session),
        1,
        "only the new round collects"
    );
}

#[test]
fn bind_next_takes_the_oldest_round_waiting_for_a_run() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    store.open(session, handle(&clock, "s-2"));
    store.bind_next(session, "task-2");
    assert_eq!(rounds_task_ids(&store, session), vec!["task-1", "task-2"]);
    assert!(store.has_round(session, "task-1"));
    assert!(store.has_round(session, "task-2"));
}

#[test]
fn bind_next_prefers_a_never_bound_round_over_a_released_one() {
    // 上游逐字：两次查表必须对"一个被释放的轮次"给出一致的答案，否则两个轮次会被
    // **交叉接错**（各封对方的气泡、两个人的回答被静默对调）。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    assert!(store.retry_unbind(session, "task-1"));
    // 一个新问题画了它自己的轮次，但它的 run 还没入队。
    store.open(session, handle(&clock, "s-2"));
    // clone 的 `task:queued` 现在到了 ⇒ 它必须拿**新问题**的轮次（从没绑过的那一个），
    // 而不是父任务被释放的那一个。
    store.bind_next(session, "clone-1");
    let ids = rounds_task_ids(&store, session);
    assert_eq!(ids, vec!["", "clone-1"], "the released round stays unbound");
}

#[test]
fn a_run_that_beats_its_bubble_waits_in_pending_and_the_bubble_picks_it_up() {
    // 上游逐字：路由把 ingest goroutine 脱离了，而第一条消息在 `dispatch` 里面入队 task ⇒
    // 事件**经常**比它所属的气泡先到。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.bind_next(session, "task-1");
    assert_eq!(pending_task_ids(&store, session), vec!["task-1"]);

    store.open(session, handle(&clock, "s-1"));
    assert_eq!(
        rounds_task_ids(&store, session),
        vec!["task-1"],
        "pairing them here is what keeps the two sequences in step"
    );
    assert!(pending_task_ids(&store, session).is_empty());
}

#[test]
fn a_pending_run_past_its_own_clock_is_not_handed_to_a_much_later_bubble() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.bind_next(session, "stale");
    clock.advance(PENDING_MAX_AGE + Duration::from_secs(1));
    store.open(session, handle(&clock, "s-1"));
    assert_eq!(
        rounds_task_ids(&store, session),
        vec![""],
        "the bubble is painted (and stays unbound) but the abandoned run must not claim it"
    );
    assert!(pending_task_ids(&store, session).is_empty());
    assert_eq!(store.depth(), 1);
}

#[test]
fn a_finished_run_cannot_take_a_bubble_a_later_question_opened() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    let turn = store.take_oldest_unbound(session);
    assert!(turn.is_none(), "task-1's round is bound, not unbound");
    let (taken, found) = futures_lite_take(&store, session, "task-1");
    assert!(found);
    assert!(taken.unwrap().has_bubble);
    assert_eq!(finished_task_ids(&store, session), vec!["task-1"]);

    // 重发的 `task:queued` 不许绑上某个后来问题打开的气泡。
    store.open(session, handle(&clock, "s-2"));
    store.bind_next(session, "task-1");
    assert_eq!(rounds_task_ids(&store, session), vec![""]);
}

/// `take` 是 async；同步用例里的小垫片（没有血缘查询 ⇒ 它一次都不会 await 到东西）。
fn futures_lite_take(store: &StreamStore, session: Id, task_id: &str) -> (Option<RoundTurn>, bool) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(store.take(session, &by_task(task_id), None))
}

// =====================================================================
// take / forget / release / retry
// =====================================================================

#[test]
fn take_hands_back_the_bubble_bound_to_the_task() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    let (turn, found) = futures_lite_take(&store, session, "task-1");
    let turn = turn.expect("the round is on file");
    assert!(found);
    assert!(turn.has_bubble);
    assert_eq!(turn.handle.stream_id, "s-1");
    assert_eq!(turn.handle.req_id, "req-1");
    assert_eq!(
        store.depth(),
        0,
        "removing it under the finding lock is the point"
    );
    assert_eq!(finished_task_ids(&store, session), vec!["task-1"]);
}

#[test]
fn take_reports_nothing_for_a_run_this_process_holds_nothing_for() {
    // 重启之前的一轮、从没开过气泡的一轮、已被另一个收尾器取走的一轮。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    let (turn, found) = futures_lite_take(&store, session, "stranger");
    assert!(turn.is_none());
    assert!(!found);
    assert!(
        finished_task_ids(&store, session).is_empty(),
        "a stranger's session must not start remembering"
    );
}

#[test]
fn take_retires_a_run_only_while_the_session_still_has_rounds() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    let (_, found) = futures_lite_take(&store, session, "task-2");
    assert!(!found);
    assert_eq!(
        finished_task_ids(&store, session),
        vec!["task-2"],
        "a republished task:queued for it must not bind a later bubble"
    );
}

#[test]
fn take_hands_back_a_bubble_that_is_still_inside_the_window() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    clock.advance(STREAM_MAX_AGE.saturating_sub(Duration::from_secs(1)));
    let (turn, found) = futures_lite_take(&store, session, "task-1");
    assert!(found);
    assert!(
        turn.unwrap().has_bubble,
        "one second inside the window is still a writable bubble"
    );
}

#[test]
fn a_round_past_the_window_is_gone_rather_than_handed_back_stale() {
    // 上游逐字：过了窗口的句柄**比没有句柄还坏** —— 它会吞掉回答而不是投递它。
    //
    // ⚠️ 实测发现（写进 `docs/32` §33 的 D9）：上游 `takeAtLocked` 里那句
    // `!s.expiredLocked(entry.handle.CreatedAt)` 在 `take` 已先跑过 `sweepLocked`（两者
    // 读的是**同一个** `createdAt` 字段、**同一个**时钟、**同一个** `maxAge`）之后是
    // **够不着的** —— 能活过扫除的条目就不可能已经过期。所以那个 `has_bubble=false` 分支
    // 是一条防御性的死路，而真实的降级形态是：扫除把轮次删掉，`take` 报**不在册**。
    // 两种形态对调用方是同一件事（退回普通消息），这里钉的是真实那一种。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-1");
    clock.advance(STREAM_MAX_AGE + Duration::from_secs(1));
    let (turn, found) = futures_lite_take(&store, session, "task-1");
    assert!(turn.is_none());
    assert!(
        !found,
        "the sweep already evicted it; the answer goes out plain"
    );
    assert_eq!(store.depth(), 0);
}

#[test]
fn take_resolves_a_clone_through_the_parent_it_inherits() {
    // clone 的 `task:queued` 命名不了它属于的轮次；它的 `chat_input_task_id` 能，而那才是权威。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "parent");
    // 父任务失败 ⇒ 轮次回去等它的替代者。
    assert!(store.retry_unbind(session, "parent"));
    let resolver = StaticRootResolver::new([("clone".to_owned(), "parent".to_owned())]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let (turn, found) = runtime.block_on(store.take(session, &by_task("clone"), Some(&resolver)));
    assert!(found, "lineage resolves what task:queued could not");
    assert_eq!(turn.unwrap().handle.stream_id, "s-1");
}

#[test]
fn retry_unbind_hands_the_round_to_a_clone_that_already_queued() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "parent");
    // 平台在 parent 的 `task:failed` **之前**就发了 clone 的 `task:queued`
    // ⇒ clone 正排在 pending 队列里。
    store.bind_next(session, "clone");
    assert_eq!(pending_task_ids(&store, session), vec!["clone"]);
    assert!(store.retry_unbind(session, "parent"));
    assert_eq!(rounds_task_ids(&store, session), vec!["clone"]);
    assert!(pending_task_ids(&store, session).is_empty());
}

#[test]
fn retry_unbind_records_the_root_when_no_clone_has_arrived() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "parent");
    assert!(store.retry_unbind(session, "parent"));
    assert_eq!(
        rounds_task_ids(&store, session),
        vec![""],
        "the round gives up the dead attempt's id"
    );
    assert!(
        !store.retry_unbind(session, "nobody"),
        "an unknown run is a no-op"
    );
    assert!(!store.retry_unbind(session, ""));
}

#[test]
fn take_oldest_unbound_hands_back_a_flush_that_never_became_a_run() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    let turn = store
        .take_oldest_unbound(session)
        .expect("an unbound round");
    assert!(turn.has_bubble);
    assert_eq!(turn.handle.stream_id, "s-1");
    assert_eq!(store.depth(), 0);
}

#[test]
fn take_oldest_unbound_skips_a_round_waiting_for_its_replacement() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "parent");
    assert!(store.retry_unbind(session, "parent"));
    assert!(
        store.take_oldest_unbound(session).is_none(),
        "its replacement is on the way; OnSettled must not close it"
    );
    assert_eq!(store.depth(), 1);
}

#[test]
fn release_round_gives_the_bubble_to_the_room_own_run() {
    // 一条在 Multica 里敲的问题从 `task:queued` 上把这个房间的轮次拿走了。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "web-question");
    // 房间自己的 run 发现轮次被占了 ⇒ 去了 pending。
    store.bind_next(session, "room-run");
    assert!(store.release_round(session, "web-question"));
    assert_eq!(rounds_task_ids(&store, session), vec!["room-run"]);
    assert!(!store.release_round(session, "nobody"));
    assert!(!store.release_round(session, ""));
}

#[test]
fn forget_drops_a_pending_run_that_arrived_before_its_bubble() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.bind_next(session, "task-1");
    store.forget(session, "task-1");
    assert!(pending_task_ids(&store, session).is_empty());
    assert_eq!(finished_task_ids(&store, session), vec!["task-1"]);
    // 于是**下一个**问题的气泡不会把这个死 run 绑上去。
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-2");
    assert_eq!(rounds_task_ids(&store, session), vec!["task-2"]);
}

#[test]
fn forget_on_a_stranger_session_does_not_start_remembering() {
    // `task:failed` 对部署里**每一个** run 都会发 ⇒ 为陌生人的会话留一个环就是泄漏。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let stranger = fresh_session();
    store.forget(stranger, "task-x");
    assert!(finished_task_ids(&store, stranger).is_empty());
    assert!(!store.holding());
    store.forget(stranger, "");
    assert!(finished_task_ids(&store, stranger).is_empty());
}

#[test]
fn drop_round_forgets_a_bubble_the_server_refused() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    let (seq, _) = store.open(session, handle(&clock, "s-refused"));
    assert_eq!(store.depth(), 1);
    store.drop_round(session, seq);
    assert_eq!(store.depth(), 0, "the bubble never existed");
    assert!(!store.holding());
    // 未知序号、未知会话都是 no-op。
    store.drop_round(session, seq);
    store.drop_round(fresh_session(), seq);
}

// =====================================================================
// holding / depth / sweep
// =====================================================================

#[test]
fn holding_counts_unpainted_rounds_and_pending_runs_too() {
    // 上游逐字：`depth()` 按"画过"筛，因为它回答"屏幕上几个气泡"；而一个气泡还在路上的 run
    // 正是**最不许**被丢掉的。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    assert!(!store.holding());
    store.bind_next(session, "task-1");
    assert!(store.holding(), "a run whose bubble is in flight counts");
    assert_eq!(store.depth(), 0, "nothing is on screen yet");
    store.open(session, handle(&clock, "s-1"));
    assert_eq!(store.depth(), 1);
}

#[test]
fn the_sweep_evicts_rounds_pending_runs_and_rings_past_the_window() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "task-2");

    // 任何一个带扫除的入口都会清掉过期的东西。这一次前进的是**流**的窗口（601s），
    // 它比 `pending` 的 30s 长得多 ⇒ `task-2` 的两个钟都已过期。
    clock.advance(STREAM_MAX_AGE + Duration::from_secs(1));
    store.bind_next(session, "task-3");
    assert_eq!(store.depth(), 0, "the round past the window is gone");
    assert_eq!(
        pending_task_ids(&store, session),
        vec!["task-3"],
        "task-2's own 30s clock expired long before this"
    );

    // 再前进超过 `pending` 的界：队列也清空，环随会话一起退休。
    clock.advance(PENDING_MAX_AGE + Duration::from_secs(1));
    assert!(store.take_oldest_unbound(session).is_none());
    assert!(pending_task_ids(&store, session).is_empty());
    assert!(finished_task_ids(&store, session).is_empty());
    assert!(!store.holding());
}

#[test]
fn remember_at_most_ten_finished_rounds_per_session() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    for index in 0..(MAX_FINISHED_ROUNDS + 5) {
        store.open(session, handle(&clock, &format!("s-{index}")));
        store.bind_next(session, &format!("task-{index}"));
        let _ = futures_lite_take(&store, session, &format!("task-{index}"));
    }
    assert_eq!(
        finished_task_ids(&store, session).len(),
        MAX_FINISHED_ROUNDS
    );
}

#[test]
fn a_handle_address_is_known_only_with_an_installation() {
    let clock = ManualClock::new();
    let mut handle = handle(&clock, "s-1");
    assert!(handle.address().is_known());
    assert_eq!(handle.address().chat_id, "chat-1");
    handle.installation_id = None;
    assert!(
        !handle.address().is_known(),
        "no live socket is what sends the round down the plain path"
    );
}

// =====================================================================
// seal：收尾帧的重试策略
// =====================================================================

fn error_unusable() -> super::super::ws_sender::SenderError {
    super::super::ws_sender::SenderError::Stream(super::super::ws_frame::StreamError {
        code: super::super::ws_frame::ERRCODE_STREAM_EXPIRED,
        message: "expired".to_owned(),
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

#[test]
fn seal_retries_a_lost_ack_and_reports_the_ending_once() {
    // 上游逐字：一次没回来的 ack 对"帧到没到"什么都没说，而重发**同一帧**无论第一次到没到
    // 都被接受 ⇒ 再写，最多 streamCloseRetries 次。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let sender = FakeSender::with_script(vec![
        Err(super::super::ws_sender::SenderError::StreamAckTimeout),
        Ok(()),
    ]);
    let handle = handle(&clock, "s-1");
    runtime()
        .block_on(store.seal(&sender, &handle, "收尾"))
        .expect("the second attempt lands");
    assert_eq!(
        sender.calls(),
        vec![(false, true), (true, true)],
        "the first attempt is an ordinary frame, every later one is a rewrite"
    );
    assert_eq!(
        sender.endings(),
        vec![None],
        "the ending is counted once whatever the number of attempts"
    );
    assert_eq!(sender.ending_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn seal_stops_on_a_verdict_that_makes_the_stream_unusable() {
    // 846605 / 846608 意味着这条流再也不会接受一帧 ⇒ 调用方退回普通消息，不重试。
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let sender = FakeSender::with_script(vec![Err(error_unusable()), Ok(())]);
    let handle = handle(&clock, "s-1");
    let error = runtime()
        .block_on(store.seal(&sender, &handle, "收尾"))
        .unwrap_err();
    assert!(error.stream_unusable());
    assert_eq!(sender.calls().len(), 1, "a verdict ends it");
    assert_eq!(sender.endings().len(), 1);
}

#[test]
fn seal_gives_up_after_the_retry_budget() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let sender = FakeSender::with_script(
        (0..8)
            .map(|_| Err(super::super::ws_sender::SenderError::StreamAckTimeout))
            .collect(),
    );
    let handle = handle(&clock, "s-1");
    let error = runtime()
        .block_on(store.seal(&sender, &handle, "收尾"))
        .unwrap_err();
    assert!(matches!(
        error,
        super::super::ws_sender::SenderError::StreamAckTimeout
    ));
    assert_eq!(
        sender.calls().len(),
        STREAM_CLOSE_RETRIES + 1,
        "an upper bound, and the deadline is the authority"
    );
}

#[test]
fn seal_stops_retrying_once_the_streams_own_window_is_gone() {
    // 上游逐字：服务端马上就要因年龄拒掉的一帧，不值得等。
    let clock = ManualClock::new();
    let store = store(&clock, Duration::ZERO);
    let sender = FakeSender::with_script(vec![
        Err(super::super::ws_sender::SenderError::StreamAckTimeout),
        Ok(()),
    ]);
    let handle = handle(&clock, "s-1");
    clock.advance(Duration::from_millis(1));
    let error = runtime()
        .block_on(store.seal(&sender, &handle, "收尾"))
        .unwrap_err();
    assert!(matches!(
        error,
        super::super::ws_sender::SenderError::StreamAckTimeout
    ));
    assert_eq!(sender.calls().len(), 1, "the window is gone");
}

#[test]
fn seal_does_not_retry_busy_or_superseded() {
    for error in [
        super::super::ws_sender::SenderError::StreamBusy,
        super::super::ws_sender::SenderError::StreamSuperseded,
    ] {
        let clock = ManualClock::new();
        let store = store(&clock, STREAM_MAX_AGE);
        let sender = FakeSender::with_script(vec![Err(error), Ok(())]);
        let handle = handle(&clock, "s-1");
        let outcome = runtime().block_on(store.seal(&sender, &handle, "收尾"));
        assert!(outcome.is_err());
        assert_eq!(
            sender.calls().len(),
            1,
            "Busy cannot happen to a closing frame, and Superseded means the answer is someone else's"
        );
    }
}

// =====================================================================
// round taker
// =====================================================================

#[test]
fn a_round_taker_without_a_store_finds_nothing() {
    // 上游逐字：没有存储就内联回复被禁用：什么都找不到。
    let key = by_task("task-1");
    let taker = RoundTaker::disabled();
    let (turn, found) = runtime().block_on(taker.take(fresh_session(), &key));
    assert!(turn.is_none());
    assert!(!found);
}

#[test]
fn a_round_taker_hands_the_store_the_lineage_resolver() {
    let clock = ManualClock::new();
    let store = Arc::new(store(&clock, STREAM_MAX_AGE));
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    store.bind_next(session, "parent");
    assert!(store.retry_unbind(session, "parent"));
    let resolver = Arc::new(StaticRootResolver::new([(
        "clone".to_owned(),
        "parent".to_owned(),
    )]));
    let taker = RoundTaker::new(Arc::clone(&store), Some(resolver));
    let (turn, found) = runtime().block_on(taker.take(session, &by_task("clone")));
    assert!(found);
    assert_eq!(turn.unwrap().handle.stream_id, "s-1");
}

#[test]
fn the_no_op_resolver_answers_nothing() {
    let resolver = NoRootResolver;
    assert!(runtime()
        .block_on(resolver.root_task_id("task-1"))
        .is_none());
}

#[test]
fn sealed_streams_lists_what_is_painted_today() {
    let clock = ManualClock::new();
    let store = store(&clock, STREAM_MAX_AGE);
    let session = fresh_session();
    store.open(session, handle(&clock, "s-1"));
    assert!(sealed_streams(&store, session).contains("s-1"));
    assert!(sealed_streams(&store, fresh_session()).is_empty());
}
