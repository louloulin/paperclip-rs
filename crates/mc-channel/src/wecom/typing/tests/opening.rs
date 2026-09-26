//! 开场（`OnIngested` / `OnSettled`）与一个**开着的气泡**的形态。
//!
//! 本文件是 `typing/tests.rs` 的子模块（门 ⑩ 的 800 行硬限拆分，见 `docs/32` §38 的 D9）。

use super::*;

/// 上游 `OnIngested`：开场帧回显**回调的** `req_id`、内容是 think 占位符、`finish=false`，而且
/// 句柄被留下来了（`RecordStreamOpened`）。
#[tokio::test]
async fn on_ingested_paints_a_bubble_with_the_callback_req_id() {
    let harness = Harness::builder().build();
    harness
        .indicator
        .on_ingested_now(
            &installation(harness.installation_id),
            &inbound("req-1", "room"),
            harness.session,
        )
        .await;

    let frames = harness.senders.stream_frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].1, STREAM_THINKING_PLACEHOLDER);
    assert!(!frames[0].2, "开场帧不是收尾帧");
    assert_eq!(harness.senders.opened(), 1);
    assert_eq!(harness.indicator.depth(), 1);
    assert!(harness.indicator.holding());
}

/// 一条在**某一轮还在等它的 run** 时到达的消息**加入**那一轮：再开一个气泡就是一个谁也关不掉的
/// 气泡。
#[tokio::test]
async fn a_second_message_joins_the_round_already_on_screen() {
    let harness = Harness::builder().build();
    for _ in 0..2 {
        harness
            .indicator
            .on_ingested_now(
                &installation(harness.installation_id),
                &inbound("req-1", "room"),
                harness.session,
            )
            .await;
    }
    assert_eq!(harness.senders.stream_frames().len(), 1, "只画一次");
    assert_eq!(harness.indicator.depth(), 1);
    assert_eq!(harness.senders.opened(), 1);
}

/// 三条**刻意不画**的路：`SkipAgentRun`（独立 `/issue` 由 replier 回答、永远没有 chat-done）、
/// 没有 `req_id`（没有流可开）、以及 raw 解不出来。
#[tokio::test]
async fn three_paths_deliberately_paint_nothing() {
    let harness = Harness::builder().build();
    let install = installation(harness.installation_id);

    let mut skipped = inbound("req-1", "room");
    skipped.skip_agent_run = true;
    harness
        .indicator
        .on_ingested_now(&install, &skipped, harness.session)
        .await;

    harness
        .indicator
        .on_ingested_now(&install, &inbound("", "room"), harness.session)
        .await;

    let mut broken = inbound("req-1", "room");
    broken.raw = serde_json::json!({});
    harness
        .indicator
        .on_ingested_now(&install, &broken, harness.session)
        .await;

    assert!(harness.senders.stream_frames().is_empty());
    assert_eq!(harness.indicator.depth(), 0);
    assert_eq!(harness.senders.opened(), 0);
}

/// 开场帧**没有**落地时的三条路：ack 没回来 / busy / superseded / 写进过 socket ⇒ **留着句柄**
/// （重发同一个 stream id 会在帧真的丢了时把消息建出来）；服务端的**判决** ⇒ 把句柄还回去。
#[tokio::test]
async fn an_opening_frame_that_did_not_land_keeps_or_gives_back_the_handle() {
    let keeping = [
        SenderError::StreamAckTimeout,
        SenderError::StreamBusy,
        SenderError::StreamSuperseded,
        SenderError::WriteAttempted {
            cause: "socket".to_string(),
        },
    ];
    for error in keeping {
        let harness = Harness::builder().build();
        harness.senders.fail_streams_with(error.clone());
        harness
            .indicator
            .on_ingested_now(
                &installation(harness.installation_id),
                &inbound("req-1", "room"),
                harness.session,
            )
            .await;
        assert_eq!(harness.indicator.depth(), 1, "{error:?} 必须留着句柄");
        assert_eq!(harness.senders.opened(), 1, "{error:?}");
    }

    for refused in [
        SenderError::Api {
            cmd: "aibot_respond_msg".to_string(),
            code: 84_605,
            message: "invalid req_id".to_string(),
        },
        SenderError::MissingCallbackReqId,
    ] {
        let harness = Harness::builder().build();
        harness.senders.fail_streams_with(refused.clone());
        harness
            .indicator
            .on_ingested_now(
                &installation(harness.installation_id),
                &inbound("req-1", "room"),
                harness.session,
            )
            .await;
        assert_eq!(harness.indicator.depth(), 0, "{refused:?} 必须把句柄还回去");
        assert_eq!(harness.senders.opened(), 0, "{refused:?}");
    }
}

/// 语言面：1:1 用档案语言，群聊用部署语言（`DirectMessageEnglish` 替身把它写死可测）。
#[tokio::test]
async fn the_bubble_language_comes_from_the_destination() {
    let harness = Harness::builder()
        .languages(Arc::new(DirectMessageEnglish))
        .build();

    // 群聊 ⇒ 部署语言（zh-Hans）。
    harness
        .indicator
        .on_ingested_now(
            &installation(harness.installation_id),
            &inbound("req-1", "room"),
            harness.session,
        )
        .await;
    let group_locale = harness
        .senders
        .stream_frames()
        .pop()
        .expect("有一轮")
        .0
        .locale;
    assert_eq!(group_locale, Locale::ZhHans);

    // 1:1 ⇒ 那个人的档案语言（替身说英文）。
    let direct = {
        let mut message = inbound("req-2", "SENDER");
        message.source.chat_type = ChatType::P2p;
        message
    };
    let other_session = Id::new();
    harness
        .indicator
        .on_ingested_now(
            &installation(harness.installation_id),
            &direct,
            other_session,
        )
        .await;
    let direct_locale = harness
        .senders
        .stream_frames()
        .pop()
        .expect("有一轮")
        .0
        .locale;
    assert_eq!(direct_locale, Locale::En);
    assert_eq!(harness.indicator.depth(), 2);
}

// =====================================================================
// on_settled
// =====================================================================

/// 上游 `OnSettled`：关掉那个**从没变成 run** 的轮次，文案是 `stream_not_started`。
#[tokio::test]
async fn on_settled_closes_the_round_that_never_became_a_run() {
    let harness = Harness::builder().build();
    harness
        .indicator
        .on_ingested_now(
            &installation(harness.installation_id),
            &inbound("req-1", "room"),
            harness.session,
        )
        .await;

    harness.indicator.on_settled_now(harness.session).await;
    let frames = harness.senders.stream_frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].1, copy_for(Locale::ZhHans).stream_not_started);
    assert!(frames[1].2, "收尾帧 finish=true");
    // 那一轮已经被取走（封口是**取走**的语义）。
    assert_eq!(harness.indicator.depth(), 0);
    // 没有轮次时它什么都不做（幂等）。
    harness.indicator.on_settled_now(harness.session).await;
    assert_eq!(harness.senders.stream_frames().len(), 2);
}

// =====================================================================
// task:queued / task:failed / task:cancelled
// =====================================================================

/// 上游 `Instant` 没有零值 ⇒ 开场时**必须**给一个真实时刻（M7-16 的 D8）。这条用例钉住
/// "新开的气泡此刻是活的"。
#[tokio::test]
async fn a_fresh_handle_is_live() {
    let harness = Harness::builder().build();
    let started = Instant::now();
    harness
        .indicator
        .on_ingested_now(
            &installation(harness.installation_id),
            &inbound("req-1", "room"),
            harness.session,
        )
        .await;
    let handle = harness.senders.stream_frames().pop().expect("有一轮").0;
    assert!(handle.created_at >= started);
    assert_eq!(handle.req_id, "req-1");
    assert_eq!(handle.chat_id, "room");
    assert_eq!(handle.chat_type, CHAT_TYPE_GROUP_INT);
    let sealed = sealed_streams(&harness.streams, harness.session);
    assert_eq!(sealed.len(), 1);
    assert!(sealed.contains(&handle.stream_id));
}
