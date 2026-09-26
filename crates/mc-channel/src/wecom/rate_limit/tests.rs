//! `rate_limit` 的用例：上游 `rate_limit_test.go` 覆盖的那几条判据（滑动窗口的等待与拒绝、
//! 退避的上下界、"不值得再试"的两条理由），加上本片新增的**分片**与**门**的形态。
//!
//! 时间一律**注入**（`admit` / `next_slot` / `wait_for` 都收一个显式的 `Instant`）⇒ 窗口的边界
//! 不靠睡真觉去撞；只有 [`SendQuota::reserve`] 与 [`send_msg_frame`] 这两条真的有 `await` 的路
//! 用**毫秒级**的窗口跑完。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mc_core::id::Id;

use super::*;

// =====================================================================
// 夹具
// =====================================================================

/// 脚本化的写入面：按顺序把预置的结果交出去，并数自己被调了几次（上游用例的 `fakeSender`）。
struct ScriptedWriter {
    results: Mutex<Vec<Result<(), SenderError>>>,
    calls: AtomicUsize,
    seen: Mutex<Vec<String>>,
}

impl ScriptedWriter {
    fn new(results: Vec<Result<(), SenderError>>) -> Self {
        Self {
            results: Mutex::new(results),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<String> {
        match self.seen.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

#[async_trait]
impl SendMsgWriter for ScriptedWriter {
    async fn write_send_msg(
        &self,
        chat_id: &str,
        _chat_type: i32,
        content: &str,
        _deadline: Deadline,
    ) -> Result<(), SenderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.seen.lock() {
            Ok(mut seen) => seen.push(format!("{chat_id}:{content}")),
            Err(poisoned) => poisoned.into_inner().push(format!("{chat_id}:{content}")),
        }
        let next = {
            let mut results = match self.results.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if results.is_empty() {
                Ok(())
            } else {
                results.remove(0)
            }
        };
        next
    }
}

fn freq_limit() -> SenderError {
    SenderError::Api {
        cmd: "aibot_send_msg".to_string(),
        code: ERR_CODE_API_FREQ_LIMIT,
        message: "api freq out of limit".to_string(),
    }
}

/// 一个"立刻就放弃"的配额（窗口长、上限 1）⇒ 第二次预留必然报 `NoSlot`。
fn spent_quota() -> SendQuota {
    SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[QuotaWindow::new(Duration::from_hours(1), 1)],
    )
}

/// 一个**宽裕**的配额：门永远放行 ⇒ 用例可以只看那次写自己的结局。
fn generous_quota() -> SendQuota {
    SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[QuotaWindow::new(
            Duration::from_mins(1),
            RATE_LIMIT_PER_MINUTE,
        )],
    )
}

// =====================================================================
// 滑动计数
// =====================================================================

/// 上游那条"为什么不是令牌桶"的判据：公布的数字是**一个窗口里的次数**，所以第 `limit + 1` 次
/// 必须等到窗口里最老的那一项老化出去。
#[test]
fn the_window_is_a_sliding_count_that_waits_for_the_oldest_entry() {
    let quota = SendQuota::with_windows(
        Duration::from_secs(3),
        &[QuotaWindow::new(
            Duration::from_mins(1),
            RATE_LIMIT_PER_MINUTE,
        )],
    );
    let t0 = Instant::now();
    for index in 0..RATE_LIMIT_PER_MINUTE {
        assert_eq!(
            quota.admit("room", t0),
            Duration::ZERO,
            "第 {index} 次该被放行"
        );
    }
    assert_eq!(quota.sent_count("room"), RATE_LIMIT_PER_MINUTE);
    // 第 31 次：下一个空位是**窗口里最老那一项**老化出去的时刻 —— 整整一个窗口。
    assert_eq!(quota.admit("room", t0), Duration::from_mins(1));
    // 而且它**没有**记下这次被拒的尝试（"leave the count untouched"）。
    assert_eq!(quota.sent_count("room"), RATE_LIMIT_PER_MINUTE);
    // 三十秒之后（窗口里最老那些已经在半路上），空位仍然来自最老的那一项：还差三十秒。
    assert_eq!(
        quota.admit("room", t0 + Duration::from_secs(30)),
        Duration::from_secs(30)
    );
    // 另一个聊**不受影响**（单位是会话，不是成员也不是安装）。
    assert_eq!(quota.admit("other", t0), Duration::ZERO);
}

/// 窗口不止一个的时候，等待是**所有**窗口里最长的那个（上游 `waitFor` 是唯一读窗口的地方）。
#[test]
fn every_window_gets_a_vote_and_the_longest_one_wins() {
    let quota = SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[
            QuotaWindow::new(Duration::from_secs(10), 100),
            QuotaWindow::new(Duration::from_mins(1), 3),
        ],
    );
    let t0 = Instant::now();
    for offset in [0, 1, 2] {
        assert_eq!(
            quota.admit("room", t0 + Duration::from_secs(offset)),
            Duration::ZERO
        );
    }
    // 分钟窗口满了（10 秒窗口还很空）⇒ 下一个空位来自**分钟**窗口：t0 那一项要等 60 秒，
    // 而此刻是 t0 + 2s ⇒ 还差 58 秒。
    assert_eq!(
        quota.admit("room", t0 + Duration::from_secs(2)),
        Duration::from_secs(58)
    );
    assert_eq!(quota.windows().len(), 2);
    // 恰好在上限上：第三项的 `span` 之后空出来（`sent[first + inWindow - limit]`）。
    assert_eq!(
        quota.wait_for(
            &[t0, t0 + Duration::from_secs(1), t0 + Duration::from_secs(2)],
            t0 + Duration::from_secs(1)
        ),
        Duration::from_secs(59)
    );
}

/// `next_slot` 是 `admit` 的**只读**孪生：问它不会花掉那个正在被问的东西。
#[test]
fn next_slot_asks_without_taking() {
    let quota = spent_quota();
    let t0 = Instant::now();
    assert_eq!(quota.admit("room", t0), Duration::ZERO);
    assert_eq!(quota.sent_count("room"), 1);

    let wait = quota.next_slot("room", t0);
    assert!(wait > Duration::ZERO, "满了就必须报一个空位");
    assert_eq!(quota.sent_count("room"), 1, "问一次不该多记一次");
    // `admit` 同一刻还是报同一个等待。
    assert_eq!(quota.admit("room", t0), wait);
    assert_eq!(quota.sent_count("room"), 1);
}

/// 清扫：一个在最长的窗口里什么都不剩的聊被丢掉 —— 否则一个跟很多聊说过话的进程会为每一个聊
/// 永远留着一个时刻切片。
#[test]
fn quiet_chats_are_swept_out() {
    let quota = SendQuota::with_windows(
        Duration::from_secs(3),
        &[QuotaWindow::new(Duration::from_secs(10), 5)],
    );
    let t0 = Instant::now();
    assert_eq!(quota.admit("room", t0), Duration::ZERO);
    assert_eq!(quota.admit("quiet", t0), Duration::ZERO);
    assert_eq!(quota.tracked_chats(), 2);

    // 过一个窗口之后换一个聊发送 ⇒ 清扫把前两个都丢掉、只留这个新的。
    assert_eq!(
        quota.admit("fresh", t0 + Duration::from_secs(30)),
        Duration::ZERO
    );
    assert_eq!(quota.tracked_chats(), 1);
    assert_eq!(quota.sent_count("room"), 0);
    assert!(format!("{quota:?}").contains("tracked_chats"));
}

// =====================================================================
// 退避
// =====================================================================

/// 抖动落在**下半**：`[base/2, base)`（上游 `base/2 + Int64N(base)` 的半开区间）。
#[test]
fn the_jitter_lives_in_the_lower_half() {
    let base = Duration::from_secs(2);
    for entropy in [0u64, 1, 7, 999_999, u64::MAX] {
        let delay = retry_delay(base, entropy);
        assert!(delay >= Duration::from_secs(1), "{entropy} → {delay:?}");
        assert!(delay < base, "{entropy} → {delay:?}");
    }
    assert_eq!(retry_delay(base, 0), Duration::from_secs(1));
    // 半宽为 0（基础延迟是 0 或 1ns）⇒ 原样返回基础延迟，而不是"抖一个零"。
    assert_eq!(retry_delay(Duration::ZERO, 12345), Duration::ZERO);
    assert_eq!(
        retry_delay(Duration::from_nanos(1), 12345),
        Duration::from_nanos(1)
    );
}

/// 一次**并发**拒绝会同时打到每一个并发调用方，所以两次重试的延迟不该相同 —— 那正是
/// "调低并发数"的反面。
#[test]
fn two_plans_do_not_come_back_at_the_same_instant() {
    let plan = RetryPlan::new(SEND_RETRY_BACKOFF);
    assert_eq!(plan.backoff(), SEND_RETRY_BACKOFF);
    assert_eq!(RetryPlan::default().backoff(), SEND_RETRY_BACKOFF);
    let first = plan.delay();
    let mut differ = false;
    for _ in 0..64 {
        let next = plan.delay();
        assert!(next >= SEND_RETRY_BACKOFF / 2 && next < SEND_RETRY_BACKOFF);
        if next != first {
            differ = true;
        }
    }
    assert!(differ, "64 次抽样全都相同 ⇒ 抖动源没有在动");
    assert_eq!(RetryPlan::new(Duration::ZERO).delay(), Duration::ZERO);
}

/// 只有那两个 `errcode` 是"平台自己会解除"的限流。
#[test]
fn only_the_two_throttle_codes_are_throttles() {
    assert!(throttled(&freq_limit()));
    assert!(throttled(&SenderError::Api {
        cmd: "aibot_send_msg".to_string(),
        code: ERR_CODE_API_CONCURRENCY_LIMIT,
        message: "api concurrency out of limit".to_string(),
    }));
    // 别的 API 拒绝、以及每一种传输层失败，都不是限流。
    for other in [
        SenderError::Api {
            cmd: "aibot_send_msg".to_string(),
            code: 40_001,
            message: "invalid credential".to_string(),
        },
        SenderError::AckTimeout,
        SenderError::AckAbandoned {
            cause: "budget".to_string(),
        },
        SenderError::NotAttempted,
        SenderError::ChatBusy,
        SenderError::WriteAttempted {
            cause: "socket".to_string(),
        },
        SenderError::FrameTooLarge { len: 9, limit: 8 },
    ] {
        assert!(!throttled(&other), "{other:?}");
    }
}

/// `retry_unaffordable` 的两条理由，以及它说"可以试"的那一格。
#[test]
fn the_retry_is_refused_for_two_reasons() {
    let quota = SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[QuotaWindow::new(Duration::from_secs(60), 2)],
    );
    let now = Instant::now();
    let delay = Duration::from_secs(2);

    // ① 调用方没预算了：剩余 < delay + ackTimeout。
    let tight = (now + delay + ACK_TIMEOUT).checked_sub(Duration::from_millis(1));
    let reason = retry_unaffordable(&quota, tight, "room", delay).expect("必须拒");
    assert_eq!(reason, "the caller has no budget for another attempt");
    // 连截止时刻都已经过去。
    assert!(retry_unaffordable(&quota, Some(now), "room", delay).is_some());

    // ② 我们自己的窗口也说没有空位。
    assert_eq!(quota.admit("room", now), Duration::ZERO);
    assert_eq!(quota.admit("room", now), Duration::ZERO);
    let reason = retry_unaffordable(&quota, None, "room", delay).expect("必须拒");
    assert!(reason.contains("no slot"), "{reason}");

    // ③ 两条都不成立 ⇒ 值得试一次。
    assert!(retry_unaffordable(&quota, None, "fresh", delay).is_none());
    assert!(retry_unaffordable(
        &quota,
        Some(now + delay + ACK_TIMEOUT + Duration::from_secs(1)),
        "fresh",
        delay
    )
    .is_none());
}

// =====================================================================
// 那扇门
// =====================================================================

/// 顺利的一次：一次尝试、Ok、花掉一个槽。
#[tokio::test]
async fn a_frame_under_the_quota_is_written_once() {
    let writer = ScriptedWriter::new(vec![Ok(())]);
    let quota = SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[QuotaWindow::new(
            Duration::from_mins(1),
            RATE_LIMIT_PER_MINUTE,
        )],
    );
    let result = send_msg_frame(
        &writer,
        &quota,
        &RetryPlan::new(Duration::ZERO),
        "room",
        2,
        "hello",
        None,
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(writer.calls(), 1);
    assert_eq!(quota.sent_count("room"), 1);
    assert_eq!(writer.seen(), vec!["room:hello".to_string()]);
}

/// 一道**不是**限流的拒绝原样交出，而且**不**重试（"只有系统失败需要重试"这条建议的另一半）。
#[tokio::test]
async fn a_definite_refusal_is_not_retried() {
    let refusal = SenderError::Api {
        cmd: "aibot_send_msg".to_string(),
        code: 40_001,
        message: "invalid credential".to_string(),
    };
    let writer = ScriptedWriter::new(vec![Err(refusal.clone())]);
    let quota = spent_quota();
    let result = send_msg_frame(
        &writer,
        &quota,
        &RetryPlan::new(Duration::ZERO),
        "room",
        2,
        "hello",
        None,
    )
    .await;
    assert_eq!(result, Err(refusal));
    assert_eq!(writer.calls(), 1, "非限流不许重试");
}

/// 一次限流**重试一次**，第二次成功 ⇒ 整体成功，而且两次都花了槽。
#[tokio::test]
async fn a_throttle_is_retried_once_and_can_still_succeed() {
    let writer = ScriptedWriter::new(vec![Err(freq_limit()), Ok(())]);
    let quota = SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[QuotaWindow::new(
            Duration::from_mins(1),
            RATE_LIMIT_PER_MINUTE,
        )],
    );
    let result = send_msg_frame(
        &writer,
        &quota,
        &RetryPlan::new(Duration::ZERO),
        "room",
        2,
        "hello",
        None,
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(writer.calls(), 2);
    assert_eq!(quota.sent_count("room"), 2);
}

/// 🔴 结尾那个 `match` 的**第一臂**：第二次尝试自己的结局**真的未知**，而未知压过第一次的拒绝 ——
/// 第二帧可能此刻就在那个人眼前。
#[tokio::test]
async fn the_second_attempts_unknown_outranks_the_first_refusal() {
    for unknown in [
        SenderError::AckTimeout,
        SenderError::StreamAckTimeout,
        SenderError::AckAbandoned {
            cause: "budget".to_string(),
        },
        SenderError::WriteAttempted {
            cause: "socket".to_string(),
        },
    ] {
        let writer = ScriptedWriter::new(vec![Err(freq_limit()), Err(unknown.clone())]);
        // 门不许插进来：这两条要钉的是**结尾那个 match**，不是预留。
        let quota = generous_quota();
        let result = send_msg_frame(
            &writer,
            &quota,
            &RetryPlan::new(Duration::ZERO),
            "room",
            2,
            "hello",
            None,
        )
        .await;
        assert_eq!(result, Err(unknown.clone()), "{unknown:?}");
        assert_eq!(writer.calls(), 2);
    }
}

/// 🔴 结尾那个 `match` 的**第二臂**：第二次什么都没写出去（这道闸又挡了一次，或者 `request` 的
/// 写前检查做了同一件事）⇒ **第一次的确定拒绝**是更好的答案，而它记成 `platform_refused`
/// 且可证明没发出去 ⇒ 不会引出任何重复。
#[tokio::test]
async fn a_second_attempt_that_wrote_nothing_keeps_the_first_refusal() {
    for nothing in [SenderError::NotAttempted, SenderError::ChatBusy] {
        let writer = ScriptedWriter::new(vec![Err(freq_limit()), Err(nothing.clone())]);
        let quota = generous_quota();
        let result = send_msg_frame(
            &writer,
            &quota,
            &RetryPlan::new(Duration::ZERO),
            "room",
            2,
            "hello",
            None,
        )
        .await;
        assert_eq!(result, Err(freq_limit()), "{nothing:?} 该保留第一次的拒绝");
        assert_eq!(writer.calls(), 2);
    }
    // 第三臂：别的一类失败原样交出（既不是未知、也不是"什么都没写"）。
    let oversized = SenderError::FrameTooLarge { len: 9, limit: 8 };
    let writer = ScriptedWriter::new(vec![Err(freq_limit()), Err(oversized.clone())]);
    let quota = generous_quota();
    let result = send_msg_frame(
        &writer,
        &quota,
        &RetryPlan::new(Duration::ZERO),
        "room",
        2,
        "hello",
        None,
    )
    .await;
    assert_eq!(result, Err(oversized));
    assert!(!nothing_new_reached_the_wire(&SenderError::FrameTooLarge {
        len: 9,
        limit: 8
    }));
}

/// 重试**不值得**做的两条理由各钉一次，两条都交出**第一次的拒绝**而**不**再发。
#[tokio::test]
async fn the_retry_is_skipped_when_it_is_not_affordable() {
    // ① 调用方的预算不够 delay + ackTimeout。
    let writer = ScriptedWriter::new(vec![Err(freq_limit())]);
    let quota = SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[QuotaWindow::new(
            Duration::from_mins(1),
            RATE_LIMIT_PER_MINUTE,
        )],
    );
    let deadline = Some(Instant::now() + ACK_TIMEOUT);
    let result = send_msg_frame(
        &writer,
        &quota,
        &RetryPlan::new(SEND_RETRY_BACKOFF),
        "room",
        2,
        "hello",
        deadline,
    )
    .await;
    assert_eq!(result, Err(freq_limit()));
    assert_eq!(writer.calls(), 1, "不值得就不许再发");

    // ② 我们自己的窗口也说没有空位（那条告警里的另一句话）。
    let writer = ScriptedWriter::new(vec![Err(freq_limit())]);
    let quota = spent_quota();
    assert_eq!(quota.admit("room", Instant::now()), Duration::ZERO);
    let result = send_msg_frame(
        &writer,
        &quota,
        &RetryPlan::new(SEND_RETRY_BACKOFF),
        "room",
        2,
        "hello",
        None,
    )
    .await;
    assert_eq!(
        result,
        Err(SenderError::NotAttempted),
        "门在自己这一侧先拒了：发都没发出去"
    );
    assert_eq!(writer.calls(), 0);
}

/// 切分之后每条消息**一次算清**段数（模块文档差异 4）：一条三段的消息一次花掉三个槽。
#[tokio::test]
async fn a_split_answer_reserves_one_slot_per_piece() {
    let content = "汉".repeat(super::super::ws_frame::SEND_MSG_CONTENT_LIMIT);
    let pieces = split_for_wire(&content).len();
    assert!(pieces >= 2, "夹具必须真的被切开：{pieces} 段");

    let writer = ScriptedWriter::new(vec![Ok(())]);
    let quota = SendQuota::with_windows(
        RATE_WAIT_BUDGET,
        &[QuotaWindow::new(Duration::from_mins(1), 64)],
    );
    let result = send_msg_frame(
        &writer,
        &quota,
        &RetryPlan::new(Duration::ZERO),
        "room",
        2,
        &content,
        None,
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(quota.sent_count("room"), pieces);
    assert_eq!(writer.calls(), 1, "一次调用 = 一条逻辑消息");
}

// =====================================================================
// 预留：等还是拒
// =====================================================================

/// 空位**近到值得等**的时候它等（一道闸的意义就是一次突发迟到而不是不来），不值得等的时候立刻拒。
#[tokio::test]
async fn reserve_waits_for_a_close_slot_and_refuses_a_far_one() {
    // 窗口 200ms / 1 条，放弃线 1s ⇒ 第二个请求等到第一个老化出去。
    let quota = SendQuota::with_windows(
        Duration::from_secs(1),
        &[QuotaWindow::new(Duration::from_millis(200), 1)],
    );
    assert!(quota.reserve(None, "room").await.is_ok());
    let started = Instant::now();
    let second = quota.reserve(None, "room").await;
    let waited = started.elapsed();
    assert!(second.is_ok(), "{second:?}");
    assert!(
        waited >= Duration::from_millis(100),
        "它必须真的等了一会儿：{waited:?}"
    );

    // 同一个窗口，但放弃线只有 10ms ⇒ 立刻拒（什么都没写出去）。
    let impatient = SendQuota::with_windows(
        Duration::from_millis(10),
        &[QuotaWindow::new(Duration::from_millis(200), 1)],
    );
    assert!(impatient.reserve(None, "room").await.is_ok());
    let refused = impatient.reserve(None, "room").await;
    let refusal = refused.expect_err("必须被拒");
    assert!(
        refusal.next_slot() >= Duration::from_millis(100),
        "{refusal:?}"
    );
    assert!(refusal.to_string().contains("quota is spent"), "{refusal}");
}

/// 🔴 放弃时刻已经把等待界在 `deadline − write_budget` 之内 ⇒ 上游 `ctx.Done()` 那一臂在本仓是
/// **够不着**的（模块文档差异 3）：一个已经过期的截止时刻报的是 [`RateLimitRefusal::NoSlot`]，
/// 而不是 [`RateLimitRefusal::CallerGaveUp`]。
#[tokio::test]
async fn an_expired_deadline_refuses_without_waiting_or_giving_up() {
    let quota = spent_quota();
    let now = Instant::now();
    assert!(quota.reserve(None, "room").await.is_ok());
    let refused = quota.reserve(Some(now), "room").await;
    assert!(
        matches!(refused, Err(RateLimitRefusal::NoSlot { .. })),
        "{refused:?}"
    );
    assert_eq!(RateLimitRefusal::CallerGaveUp.next_slot(), Duration::ZERO);
    // 一个"还剩不到 write_budget"的截止时刻同样报 `NoSlot`。
    let tight = Some(Instant::now() + ACK_TIMEOUT / 2);
    let refused = quota.reserve(tight, "room").await;
    assert!(
        matches!(refused, Err(RateLimitRefusal::NoSlot { .. })),
        "{refused:?}"
    );
    // 与此同时：有整份预算的调用方**现在**就拿到它的那一回合（空位空着时它不花任何预算）。
    assert!(quota.reserve(None, "fresh-room").await.is_ok());
}

// =====================================================================
// 分片
// =====================================================================

/// 桶**按 installation 分片**：同一条安装永远拿到同一个桶（`Arc` 同一个），不同的安装各拿各的；
/// 而且 `get` **不**铸桶。
#[test]
fn the_buckets_are_sharded_by_installation() {
    let shards = QuotaShards::new(8);
    let one = Id::new();
    let two = Id::new();
    assert!(shards.is_empty());
    assert!(shards.get(one).is_none(), "get 不许铸桶");

    let first = shards.bucket(one);
    assert!(
        Arc::ptr_eq(&first, &shards.bucket(one)),
        "同一个安装必须同一个桶"
    );
    let other = shards.bucket(two);
    assert!(!Arc::ptr_eq(&first, &other));
    assert_eq!(shards.len(), 2);

    // 桶**跟着 installation 走、不跟着 socket 走**：一次重连（同一安装再问一次）拿到的还是它，
    // 于是一个已经花掉的额度**不会**被清零（这正是上游指出的那条缝，本仓关掉了它）。
    assert_eq!(first.admit("room", Instant::now()), Duration::ZERO);
    assert_eq!(shards.bucket(one).sent_count("room"), 1);
    assert_eq!(shards.bucket(two).sent_count("room"), 0);

    // 表有界：装满之后每多一条就淘汰一条最老的。
    for _ in 0..16 {
        let _ = shards.bucket(Id::new());
    }
    assert!(shards.len() <= 8, "容量必须被守住：{}", shards.len());
    assert!(format!("{shards:?}").contains("capacity"));
    assert_eq!(DEFAULT_QUOTA_SHARDS, 1024);
    assert!(QuotaShards::default().is_empty());
}

/// 生产形态的桶与上游那两个窗口逐字一致（常量与 `SendQuota::new()` 是同一个来源）。
#[test]
fn the_production_bucket_carries_the_published_windows() {
    let quota = SendQuota::default();
    assert_eq!(quota.windows().len(), 2);
    assert_eq!(quota.windows()[0].limit, RATE_LIMIT_PER_MINUTE);
    assert_eq!(quota.windows()[0].span, Duration::from_mins(1));
    assert_eq!(quota.windows()[1].limit, RATE_LIMIT_PER_HOUR);
    assert_eq!(quota.windows()[1].span, Duration::from_hours(1));
    assert_eq!(quota.max_wait(), RATE_WAIT_BUDGET);
    assert_eq!(RATE_LIMIT_PER_MINUTE, 30);
    assert_eq!(RATE_LIMIT_PER_HOUR, 1000);
    // 窗口的 `limit` 至少是 1（一个什么都过不去的窗口是部署层面的事）。
    assert_eq!(QuotaWindow::new(Duration::from_secs(1), 0).limit, 1);
}
