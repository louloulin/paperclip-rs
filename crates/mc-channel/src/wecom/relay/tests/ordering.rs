//! `relay` 的**顺序与准入**用例（本片专属验收第 2 条）。

use super::super::{Hold, Lines, Queued};
use super::*;

#[tokio::test]
async fn a_re_offered_frame_keeps_its_place_at_the_head_of_its_line() {
    let cfg = fast_config();
    let handler = FakeHandler::scripted(
        true,
        vec![
            // A 第一次：可证明没发出 ⇒ 它会停在自己那条线的队首等重投递。
            RelayResult::new(RelayOutcome::ProvablyNotSent),
            // A 第二次（重投递）：成功。
            RelayResult::new(RelayOutcome::Done),
            // B：成功。
            RelayResult::new(RelayOutcome::Done),
        ],
    );
    let relay = Arc::new(RelayOutbound::new(None, None, cfg));
    relay.attach(handler.clone());
    let mut lines = Lines::new();

    let a = Queued::new(reply_frame("inst-1", "task-a", "A"), "ev-a");
    let b = Queued::new(reply_frame("inst-1", "task-b", "B"), "ev-b");
    relay.offer(&mut lines, a).await;
    assert_eq!(handler.calls(), 1, "A 被尝试过一次");
    let hold = lines.get("inst-1").expect("A 停在自己的线上");
    assert_eq!(hold.items.len(), 1);
    assert_eq!(hold.items[0].frame.task_id, "task-a");
    assert!(hold.ready_at.is_some(), "它在等一次重投递");

    // B 到达：它加入 A 的**尾部**，不许超车，也不许被投递。
    relay.offer(&mut lines, b).await;
    assert_eq!(handler.calls(), 1, "B 不许在 A 前面被投递");
    let hold = lines.get("inst-1").expect("线还在");
    assert_eq!(hold.items.len(), 2);
    assert_eq!(hold.items[0].frame.task_id, "task-a");
    assert_eq!(hold.items[1].frame.task_id, "task-b");

    // 退避走完（用例把时刻直接拨到"已经到期"，不睡真觉）：A 先被重投递，然后才轮到 B。
    lines.get_mut("inst-1").expect("线还在").ready_at = None;
    relay.fire_due(&mut lines).await;
    relay.fire_due(&mut lines).await;
    let order: Vec<String> = handler
        .delivered()
        .iter()
        .map(|frame| frame.task_id.clone())
        .collect();
    assert_eq!(
        order,
        vec!["task-a", "task-a", "task-b"],
        "A 的重投递不许被 B 超过"
    );
    assert!(lines.get("inst-1").is_none(), "线排空了");
}

/// 排空同样不许颠倒顺序：停机时已经排队的帧按**到达顺序**走完。

#[tokio::test]
async fn drain_keeps_the_arrival_order() {
    let handler = FakeHandler::owns(true);
    let relay = Arc::new(RelayOutbound::new(None, None, fast_config()));
    relay.attach(handler.clone());
    let mut lines = Lines::new();
    // 一条线上停两个（第一个在等重投递），队列里还有第三个。
    lines.insert(
        "inst-1",
        Hold {
            items: vec![
                Queued::new(reply_frame("inst-1", "task-a", "A"), "ev-a"),
                Queued::new(reply_frame("inst-1", "task-b", "B"), "ev-b"),
            ],
            ready_at: Some(Instant::now() + Duration::from_secs(3600)),
        },
    );
    relay
        .queues()
        .first()
        .expect("至少一片")
        .push(Queued::new(reply_frame("inst-1", "task-c", "C"), "ev-c"));
    let shard = relay.shard_for("inst-1");
    relay
        .queues()
        .get(shard)
        .expect("片存在")
        .push(Queued::new(reply_frame("inst-1", "task-c", "C"), "ev-c"));
    relay.drain_remaining(&mut lines, shard, None).await;
    let order: Vec<String> = handler
        .delivered()
        .iter()
        .map(|frame| frame.task_id.clone())
        .collect();
    assert!(
        order.starts_with(&["task-a".to_string(), "task-b".to_string()]),
        "线里的顺序不许被颠倒，实际 {order:?}"
    );
    assert!(order.len() >= 3, "队列里那个也要走完：{order:?}");
}

// =====================================================================
// 准入与削减
// =====================================================================

/// 帧进不了队列时**削减**（并记在 `relay_shed` 上），而不是卡住分片读循环。

#[tokio::test]
async fn a_full_queue_sheds_instead_of_stalling_the_shard() {
    let counting = Box::leak(Box::new(CountingShed::default()));
    let cfg = RelayConfig {
        queue_depth: Some(1),
        ..fast_config()
    };
    let relay = RelayOutbound::new(None, None, cfg);
    relay.set_metrics(counting);
    let frame = reply_frame("inst-1", "task-1", "x");
    let encoded = frame.encode().expect("encode");
    relay.deliver_outbound("inst-1", &encoded, "ev-1");
    relay.deliver_outbound("inst-1", &encoded, "ev-2");
    assert_eq!(counting.shed.load(Ordering::SeqCst), 1);
    let shard = relay.shard_for("inst-1");
    assert_eq!(relay.queues()[shard].len(), 1);
    // 读不出来的帧不 panic、不进队列。
    relay.deliver_outbound("inst-1", b"{not json", "ev-3");
    assert_eq!(relay.queues()[shard].len(), 1);
}

/// 削减**不许**动回复计数器（每个副本都读每条帧 ⇒ 谁也说不清一次削减让用户损失了什么）。

#[tokio::test]
async fn shedding_never_touches_the_reply_counters() {
    let counting = Box::leak(Box::new(CountingShed::default()));
    let relay = RelayOutbound::new(None, None, fast_config());
    relay.set_metrics(counting);
    let item = Queued::new(reply_frame("inst-1", "task-1", "x"), "ev-1");
    relay.shed(&item, "test");
    assert_eq!(counting.shed.load(Ordering::SeqCst), 1);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
}

/// 没有 socket 的副本**不去争 claim**（否则能发的那个副本会输掉竞争，而这条回复谁都不送）。

#[tokio::test]
async fn a_replica_without_the_socket_never_claims() {
    let dedupe = Arc::new(InProcessDedupe::new(8));
    let relay = Arc::new(RelayOutbound::new(
        None,
        Some(Arc::clone(&dedupe) as Arc<dyn DedupeStore>),
        fast_config(),
    ));
    let handler = FakeHandler::owns(false);
    relay.attach(handler.clone());
    let item = Queued::new(reply_frame("inst-1", "task-1", "x"), "ev-1");
    assert!(
        relay.perform(&item, None).await,
        "不是我们的 ⇒ 这一帧完事（别人会接）"
    );
    assert_eq!(handler.calls(), 0);
    // 键从没被碰过（claim 一次都没发生）。
    assert!(
        dedupe.is_empty(),
        "没有 socket 的副本不许在 claim 存储里留下痕迹"
    );
}

// =====================================================================
// 发布
// =====================================================================

/// 发布走 publisher，并且**只有回复**会被登记给结局观察者。

#[tokio::test]
async fn publishing_enrols_only_replies_for_an_outcome() {
    let publisher = Arc::new(FakePublisher::default());
    let dedupe: Arc<dyn DedupeStore> = Arc::new(InProcessDedupe::new(16));
    let relay = RelayOutbound::new(
        Some(Arc::clone(&publisher) as Arc<dyn RelayPublisher>),
        Some(dedupe),
        fast_config(),
    );
    let reply = reply_frame("inst-1", "task-1", "answer");
    assert!(relay.publish(&reply, "ev-1"));
    assert_eq!(relay.pending_count(), 1);
    let inbox = RelayFrame::inbox("inst-1".into(), "user-1".into(), 1, "card".into());
    assert!(relay.publish(&inbox, "ev-2"));
    assert_eq!(relay.pending_count(), 1, "收件箱推送的结局不在这里观察");
    let sent = publisher.published.lock().expect("lock").clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].0, RELAY_SCOPE);
    assert!(sent[0].1.starts_with("inst-1|"));
}

/// 没有发布方 ⇒ 不发布（`route_frame` 的 `false` 就是"没人接这一帧"）。

#[tokio::test]
async fn without_a_publisher_there_is_nothing_to_route_through() {
    let relay = RelayOutbound::new(None, None, fast_config());
    assert!(!relay.publish(&reply_frame("inst-1", "task-1", "x"), "ev-1"));
}

// =====================================================================
// 端到端的那根线（worker / attach / 停机）
// =====================================================================

/// 宿主会走的那条路：`deliver_outbound`（分片读者的同步接缝）→ 每片的 worker → handler →
/// 停机排空。它把"注册得比 handler 早"（上游要求 3）与 `RelayHandle` 的两半都跑一遍。

#[tokio::test]
async fn a_frame_survives_the_shard_worker_and_the_shutdown() {
    let handler = FakeHandler::owns(true);
    let dedupe: Arc<dyn DedupeStore> = Arc::new(InProcessDedupe::new(16));
    let relay = Arc::new(RelayOutbound::new(None, Some(dedupe), fast_config()));
    // 注册**先于** attach：帧在队列里等，而不是对着空槽被丢掉。
    let frame = reply_frame("inst-worker", "task-1", "answer");
    let encoded = frame.encode().expect("encode");
    relay.deliver_outbound("inst-worker", &encoded, "ev-worker");
    let handle = relay.start();
    relay.attach(handler.clone());
    for _ in 0..200 {
        if handler.calls() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(handler.calls(), 1, "worker 把那一帧送到了");
    assert_eq!(handler.delivered()[0].task_id, "task-1");
    handle.shutdown();
    handle.wait().await;
}

// =====================================================================
// 错误分类
// =====================================================================
