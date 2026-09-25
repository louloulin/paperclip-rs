//! `relay` 的**幂等**用例（本片专属验收第 1 条）：同一条事件 id 只送一次。

use super::super::Queued;
use super::*;

#[tokio::test]
async fn a_replayed_frame_is_delivered_exactly_once() {
    let cfg = fast_config();
    let dedupe: Arc<dyn DedupeStore> = Arc::new(InProcessDedupe::new(16));
    let handler = FakeHandler::owns(true);
    let relay = Arc::new(RelayOutbound::new(None, Some(dedupe.clone()), cfg));
    relay.attach(handler.clone());

    let frame = reply_frame("inst-1", "task-1", "answer");
    let item = Queued::new(frame.clone(), "ev-1");
    assert!(relay.perform(&item, None).await, "第一次：完事");
    assert_eq!(handler.calls(), 1);

    // 同一个进程里的第二次：本地闸（`seen`）挡住，连 claim 都不去问。
    assert!(relay.perform(&item, None).await);
    assert_eq!(handler.calls(), 1, "本地闸挡住重放");

    // **另一个副本**：新的调度器、同一个 claim 存储 ⇒ 输掉 claim（= 欠一次 offer），但什么都没送。
    let other = Arc::new(RelayOutbound::new(None, Some(dedupe), cfg));
    other.attach(handler.clone());
    assert!(
        !other.perform(&item, None).await,
        "拿不到 claim ⇒ 这一帧还欠一次 offer（见 perform 的注释）"
    );
    assert_eq!(handler.calls(), 1, "另一个副本也不许送第二遍");
}

/// claim 被结算之后，这条回复的结局**恰好**记一次：`perform` 施加记录，`settle` 保持安静。

#[tokio::test]
async fn the_outcome_is_recorded_once_after_the_claim_is_settled() {
    let cfg = fast_config();
    let dedupe: Arc<dyn DedupeStore> = Arc::new(InProcessDedupe::new(16));
    let handler = FakeHandler::scripted(
        true,
        vec![RelayResult::recorded(
            RelayOutcome::Done,
            RelayRecord::Delivered,
        )],
    );
    let relay = Arc::new(RelayOutbound::new(None, Some(dedupe.clone()), cfg));
    relay.attach(handler.clone());

    let item = Queued::new(reply_frame("inst-1", "task-1", "answer"), "ev-2");
    assert!(relay.perform(&item, None).await);
    assert_eq!(handler.records(), vec![RelayRecord::Delivered]);
    // 键现在是 settled ⇒ 发布方的结算什么都不做（一个记录）。
    let pending = PendingOutcome {
        key: dedupe_key("ev-2"),
        session_id: "sess-1".into(),
        installation_id: "inst-1".into(),
        task_id: "task-1".into(),
        due_at: Instant::now(),
    };
    let counting = Box::leak(Box::new(CountingShed::default()));
    relay.set_metrics(counting);
    relay.settle(&pending).await;
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
    assert!(counting.dropped_labels.lock().expect("lock").is_empty());
}

/// 一条**没人接**（`absent`）的被路由回复：那一趟记一次 `no_live_connection`。
///
/// 第二趟会**再记一次** —— 这是上游**刻意**的行为，不是这里漏了：`Resolve` 对 `absent`
/// **不设围栏**（上游逐字：给 absent 设围栏会把"一次过早的过期被记成丢失"换成"一个任何
/// offer 都拿不到的 claim，也就是一条用户永远收不到的回复" —— 更糟）。
/// 所以"一条回复只被记一次"靠的是**发布方只登记一次**，以及被 fence 的 `held` 只有第一次
/// 读得到 `Held`（见下一条用例）。

#[tokio::test]
async fn an_absent_claim_records_the_loss_on_the_pass_that_reads_it() {
    let counting = Box::leak(Box::new(CountingShed::default()));
    let dedupe: Arc<dyn DedupeStore> = Arc::new(InProcessDedupe::new(16));
    let relay = RelayOutbound::new(None, Some(dedupe), fast_config());
    relay.set_metrics(counting);
    let pending = PendingOutcome {
        key: dedupe_key("ev-nobody"),
        session_id: "sess-1".into(),
        installation_id: "inst-1".into(),
        task_id: "task-1".into(),
        due_at: Instant::now(),
    };
    relay.settle(&pending).await;
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        counting.dropped_labels.lock().expect("lock").as_slice(),
        ["no_live_connection"]
    );
}

/// 一个拿了 claim、什么都没记的副本 ⇒ `Resolve` 把键围栏成 lost，记一次 `transport_error`。

#[tokio::test]
async fn a_claim_held_by_a_silent_replica_is_fenced_and_recorded_once() {
    let counting = Box::leak(Box::new(CountingShed::default()));
    let dedupe: Arc<dyn DedupeStore> = Arc::new(InProcessDedupe::new(16));
    let key = dedupe_key("ev-stranded");
    assert!(dedupe
        .claim(&key, "owner-x/ev-stranded", MIN_DEDUPE_TTL)
        .await
        .expect("claim"));
    let relay = RelayOutbound::new(None, Some(dedupe), fast_config());
    relay.set_metrics(counting);
    let pending = PendingOutcome {
        key: key.clone(),
        session_id: "sess-1".into(),
        installation_id: "inst-1".into(),
        task_id: "task-1".into(),
        due_at: Instant::now(),
    };
    relay.settle(&pending).await;
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        counting.dropped_labels.lock().expect("lock").as_slice(),
        ["transport_error"]
    );
    // 键已经被围栏成 `lost` ⇒ **同一趟之后的每一次结算都保持安静**（上游：`Resolve` 读且围栏）。
    relay.settle(&pending).await;
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 1, "第二次不许再记");
    // 一个更晚回来的持有者也**什么都记不了**（上游逐字）：被围栏成 `lost` 的 claim
    // 拒绝任何人的结算（`settled` 那一格则报 `true` —— 重试安全）。
    let store = InProcessDedupe::new(4);
    assert!(store.claim("k", "t", MIN_DEDUPE_TTL).await.expect("claim"));
    assert_eq!(store.resolve("k").await.expect("resolve"), ClaimState::Held);
    assert!(!store.settle("k", "t").await.expect("settle"), "已被围栏");
    assert!(store
        .claim("k2", "t2", MIN_DEDUPE_TTL)
        .await
        .expect("claim"));
    assert!(store.settle("k2", "t2").await.expect("settle"));
    assert!(store.settle("k2", "t2").await.expect("settle"), "重试安全");
}

// =====================================================================
// 顺序（**本片专属验收第 2 条**）
// =====================================================================

/// 一个需要重投递的帧**停在队首**，它后面到达的帧**不许超车**：
/// 两条回答按到达顺序到达用户面前。

#[tokio::test]
async fn a_provably_unsent_frame_releases_its_claim_without_counting_a_drop() {
    let dedupe: Arc<dyn DedupeStore> = Arc::new(InProcessDedupe::new(16));
    let handler =
        FakeHandler::scripted(true, vec![RelayResult::new(RelayOutcome::ProvablyNotSent)]);
    let relay = Arc::new(RelayOutbound::new(None, Some(dedupe), fast_config()));
    relay.attach(handler);
    let counting: &'static CountingShed = Box::leak(Box::new(CountingShed::default()));
    relay.set_metrics(counting);
    let item = Queued::new(reply_frame("inst-1", "task-1", "x"), "ev-1");
    assert!(!relay.perform(&item, None).await, "还欠一次 offer");
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
}
