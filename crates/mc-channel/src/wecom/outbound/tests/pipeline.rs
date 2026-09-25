//! `outbound` 的**判决链**用例：一条 `chat:done` 从判定到记账的每一条路。

use super::*;
use crate::wecom::stream_store::by_task;

/// 没有 `chat_session` 的事件（issue / autopilot 任务）与本订阅者无关。
#[tokio::test]
async fn an_event_without_a_chat_session_is_ignored() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let outbound =
        Outbound::new(Arc::new(FakeQueries::default()), None).with_outbound_metrics(counting);
    let mut event = chat_done(Id::new(), Id::new(), "hi");
    event.chat_session_id = String::new();
    let verdict = outbound.handle_chat_done(&event).await;
    assert_eq!(verdict.recorded, Recorded::Nothing);
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
}

/// task 解不出来（事件上没有 id，或者那一行已经被回收）⇒ `task_missing`，**不是**静默。

#[tokio::test]
async fn a_missing_task_is_dropped_with_a_reason() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let outbound =
        Outbound::new(Arc::new(FakeQueries::default()), None).with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(Id::new(), session(), "hi"))
        .await;
    assert_eq!(
        verdict.recorded,
        Recorded::Dropped(crate::wecom::outcome::DropReason::TaskMissing)
    );
    assert_eq!(counting.labels(), vec!["task_missing".to_string()]);
}

/// 一次在 Multica 的 web UI 上打的 run：**跳过**（`origin_not_channel`），而且把气泡**还回去**
/// —— 否则提问者会看着一个永远不结束的气泡。

#[tokio::test]
async fn a_run_from_the_web_ui_releases_the_round_and_is_skipped() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let (store, session) = store_with_bubble("s-1", installation, task_id, Locale::ZhHans);
    let queries = Arc::new(FakeQueries {
        // 批次里**没有**渠道递进来的消息（origin 门要问的正是这一格）。
        ingested: false,
        ..FakeQueries::default()
    });
    // `chat_input_task_id` 存在 + 批次里没有渠道消息 ⇒ origin 不是渠道。
    *queries.task.lock().expect("lock") = Some(AgentTask {
        id: task_id,
        chat_input_task_id: Some(Id::new()),
        batch_has_channel_ingested_messages: false,
    });
    let outbound = Outbound::new(queries as Arc<dyn OutboundQueries>, None)
        .with_streams(Arc::clone(&store))
        .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session, "answer"))
        .await;
    assert_eq!(
        verdict.recorded,
        Recorded::Skipped(crate::wecom::outcome::SkipReason::OriginNotChannel)
    );
    assert_eq!(counting.skipped.load(Ordering::SeqCst), 1);
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 0);
    // 气泡被**释放**（轮次回到"没人绑"，下一条消息可以重新开一个）。
    assert_eq!(store.depth(), 1, "气泡本身还在屏幕上");
    let (turn, _) = store
        .take(session, &by_task(task_id.0.to_string()), None)
        .await;
    assert!(
        turn.is_none() || !turn.expect("turn").has_bubble,
        "被释放的轮次不该再被取走并被封"
    );
}

/// 有气泡 ⇒ 回答**就地**封进气泡，回复计数一次。

#[tokio::test]
async fn an_answer_with_a_bubble_is_sealed_in_place() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let (store, session) = store_with_bubble("s-1", installation, task_id, Locale::ZhHans);
    let stream: Arc<dyn StreamSender> = Arc::new(RecordingStreamSender::default());
    let stream_probe = Arc::clone(&stream);
    let senders = Arc::new(FakeSenders::default().with_stream(stream));
    let queries = FakeQueries::with_task(task(task_id));
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_streams(store)
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session, "the answer"))
        .await;
    assert_eq!(verdict.recorded, Recorded::Delivered);
    assert!(verdict.answer.spoke);
    assert!(!verdict.answer.routed);
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 1);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
    let _ = stream_probe;
}

/// 一次空完成**仍然**要封气泡（转不完的圈比一句短回答更糟）—— 它**不**是 `nothing_to_say`。

#[tokio::test]
async fn an_empty_completion_under_a_bubble_still_closes_it() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let (store, session) = store_with_bubble("s-1", installation, task_id, Locale::ZhHans);
    let senders =
        Arc::new(FakeSenders::default().with_stream(Arc::new(RecordingStreamSender::default())));
    let queries = FakeQueries::with_task(task(task_id));
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_streams(store)
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session, " \n\t "))
        .await;
    assert_eq!(verdict.recorded, Recorded::Delivered);
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 1);
    assert_eq!(counting.skipped.load(Ordering::SeqCst), 0);
}

/// 没有气泡、也没什么可说 ⇒ **跳过**（`nothing_to_say`），不是丢弃。

#[tokio::test]
async fn nothing_to_say_is_a_skip_not_a_drop() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let task_id = Id::new();
    let queries = FakeQueries::with_task(task(task_id));
    let outbound =
        Outbound::new(queries as Arc<dyn OutboundQueries>, None).with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session(), "\n"))
        .await;
    assert_eq!(
        verdict.recorded,
        Recorded::Skipped(crate::wecom::outcome::SkipReason::NothingToSay)
    );
    assert_eq!(counting.skipped.load(Ordering::SeqCst), 1);
    assert_eq!(counting.labels(), vec!["nothing_to_say".to_string()]);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
}

/// 收尾的判决**没回来** ⇒ `unconfirmed`（`seal_unacked`），**不是**丢弃：重发一条用户可能
/// 已经看到的消息是唯一回不了头的错误。

#[tokio::test]
async fn a_seal_whose_verdict_never_came_is_unconfirmed() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let (store, session) = store_with_bubble("s-1", installation, task_id, Locale::ZhHans);
    let stream = RecordingStreamSender::failing(SenderError::StreamAckTimeout);
    let endings = Arc::clone(&stream);
    let senders = Arc::new(FakeSenders::default().with_stream(stream));
    let queries = FakeQueries::with_task(task(task_id));
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_streams(store)
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session, "answer"))
        .await;
    assert_eq!(verdict.recorded, Recorded::Unconfirmed("seal_unacked"));
    assert_eq!(counting.unconfirmed.load(Ordering::SeqCst), 1);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
    assert!(verdict.answer.spoke, "话**可能**已经在屏幕上");
    // **结束只被计一次**（上游 `recordEnding` 的位置）：收尾重试了三次，但记账一次。
    assert_eq!(endings.endings(), 1);
}

/// 收尾**被证明没落进气泡** ⇒ 同一句话**退回普通消息**发出去，并带上那份气泡花不掉的预算。

#[tokio::test]
async fn a_refused_seal_falls_back_to_a_plain_message() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let (store, session) = store_with_bubble("s-1", installation, task_id, Locale::ZhHans);
    let stream = RecordingStreamSender::failing(SenderError::StreamBusy);
    let live = Arc::new(RecordingSender::default());
    let senders = Arc::new(FakeSenders::with(installation, live.clone()).with_stream(stream));
    let queries = FakeQueries::with_task(task(task_id));
    queries.with_delivery(delivery(installation), active(installation));
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_streams(store)
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session, "answer"))
        .await;
    assert_eq!(verdict.recorded, Recorded::Delivered);
    assert!(verdict.answer.spoke);
    let sent = live.sent();
    assert_eq!(sent.len(), 1, "同一句话作为普通消息发了一次");
    assert_eq!(sent[0].text, "answer");
    assert_eq!(sent[0].chat_id, "chat-1");
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 1);
}

/// 投递行命名的是**另一个平台** ⇒ 静默的寻常情况（一行零地址、没有 skip 原因）。

#[tokio::test]
async fn a_task_delivery_for_another_platform_is_a_silent_no_op() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let queries = FakeQueries::with_task(task(task_id));
    let mut row = delivery(installation);
    row.channel_type = "slack".to_string();
    queries.with_delivery(row, active(installation));
    let outbound =
        Outbound::new(queries as Arc<dyn OutboundQueries>, None).with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session(), "answer"))
        .await;
    assert_eq!(verdict.recorded, Recorded::Nothing);
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(counting.skipped.load(Ordering::SeqCst), 0);
}

/// 安装在触发与回复之间被撤销 ⇒ **跳过**（`installation_inactive`），不是投递失败。

#[tokio::test]
async fn a_revoked_installation_is_a_skip_not_a_drop() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let queries = FakeQueries::with_task(task(task_id));
    queries.with_delivery(
        delivery(installation),
        InstallationRecord {
            id: installation,
            status: InstallationStatus::Revoked,
        },
    );
    let outbound =
        Outbound::new(queries as Arc<dyn OutboundQueries>, None).with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session(), "answer"))
        .await;
    assert_eq!(
        verdict.recorded,
        Recorded::Skipped(crate::wecom::outcome::SkipReason::InstallationInactive)
    );
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
}

/// 本副本没有活的 socket、也没有中继 ⇒ 一条**带原因**的丢弃（不是静默）。

#[tokio::test]
async fn no_live_socket_without_a_relay_is_a_drop_with_a_reason() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let queries = FakeQueries::with_task(task(task_id));
    queries.with_delivery(delivery(installation), active(installation));
    // 注册表**在**（装配是好的），只是里面没有这条安装的活 socket。
    let senders = Arc::new(FakeSenders::default());
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session(), "answer"))
        .await;
    assert_eq!(
        verdict.recorded,
        Recorded::Dropped(crate::wecom::outcome::DropReason::NoLiveConnection)
    );
    assert_eq!(counting.labels(), vec!["no_live_connection".to_string()]);
}

/// 有中继 ⇒ 这一帧被**路由**给握着 socket 的副本，本副本不记回复的账（中继记）。

#[tokio::test]
async fn a_reply_is_routed_when_another_replica_holds_the_socket() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let queries = FakeQueries::with_task(task(task_id));
    queries.with_delivery(delivery(installation), active(installation));
    let relay = Arc::new(FakeRelay {
        accept: true,
        ..FakeRelay::default()
    });
    // 注册表在、但没有活 socket ⇒ 这一帧被交给握着 socket 的那个副本。
    let senders = Arc::new(FakeSenders::default());
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_relay(relay.clone())
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session(), "answer"))
        .await;
    assert_eq!(verdict.recorded, Recorded::Nothing);
    assert!(verdict.answer.routed);
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
    let published = relay.published.lock().expect("lock").clone();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].0.kind, crate::wecom::relay::RelayKind::Reply);
    assert_eq!(published[0].0.chat_id, "chat-1");
    // 事件 id 从**那一轮**派生 ⇒ 一次重发布是同一条 claim。
    assert_eq!(
        published[0].1,
        crate::wecom::relay::relay_event_id("chat:done", task_id)
    );
}

/// 平台**说出来了**的拒绝：`platform_refused`，而且**只记一次**（`errOutcomeRecorded`）。

#[tokio::test]
async fn a_platform_refusal_is_counted_exactly_once() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let live = RecordingSender::failing(SenderError::Api {
        cmd: "aibot_send_msg".to_string(),
        code: 846_609,
        message: "bot is not in this chat".to_string(),
    });
    let senders = Arc::new(FakeSenders::with(installation, live));
    let queries = FakeQueries::with_task(task(task_id));
    queries.with_delivery(delivery(installation), active(installation));
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session(), "answer"))
        .await;
    assert_eq!(
        verdict.recorded,
        Recorded::Dropped(crate::wecom::outcome::DropReason::PlatformRefused)
    );
    assert_eq!(
        counting.dropped.load(Ordering::SeqCst),
        1,
        "一个发送只动一个计数器"
    );
    assert_eq!(counting.labels(), vec!["platform_refused".to_string()]);
}

/// 部分发送 ⇒ **算送达**（用户已经在读第一段）。

#[tokio::test]
async fn a_partial_send_counts_as_delivered() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    let live = RecordingSender::failing(SenderError::PartiallySent {
        cause: "piece 2 refused".to_string(),
    });
    let senders = Arc::new(FakeSenders::with(installation, live));
    let queries = FakeQueries::with_task(task(task_id));
    queries.with_delivery(delivery(installation), active(installation));
    let outbound = Outbound::new(
        queries as Arc<dyn OutboundQueries>,
        Some(senders as Arc<dyn SenderLookup>),
    )
    .with_outbound_metrics(counting);
    let verdict = outbound
        .handle_chat_done(&chat_done(task_id, session(), "answer"))
        .await;
    assert_eq!(verdict.recorded, Recorded::Delivered);
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 1);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
}

// =====================================================================
// 附件准入（闸在 outbound.go 里，工作归 M7-18）
// =====================================================================
