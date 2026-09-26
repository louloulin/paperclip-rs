//! 三次 run 结束各自怎么收尾：绑定、失败（气泡 / 普通消息 / 中继）、取消。
//!
//! 本文件是 `typing/tests.rs` 的子模块（门 ⑩ 的 800 行硬限拆分，见 `docs/32` §38 的 D9）。

use super::*;

/// `task:queued` 只绑**会话里的** run：一个带 issue id 的 run（`/issue`、autopilot、web 重跑）
/// 拿走一个 `WeCom` 气泡会用一个房间里没人问过的答案封掉一个陌生人的问题。
#[tokio::test]
async fn only_chat_runs_get_the_bubble() {
    let harness = Harness::builder().build();
    let _ = harness.open_round("").await;

    // 一次 issue 的 run：不绑。
    harness.indicator.handle_task_queued(
        &TaskEvent::queued(TASK_ISSUE, Some(harness.session.to_string())).with_issue("issue-1"),
    );
    assert!(rounds_task_ids(&harness.streams, harness.session)[0].is_empty());

    // 一次会话里的 run：绑上。
    harness.indicator.handle_task_queued(&TaskEvent::queued(
        TASK_ID,
        Some(harness.session.to_string()),
    ));
    assert_eq!(
        rounds_task_ids(&harness.streams, harness.session),
        vec![TASK_ID.to_string()]
    );
}

/// 一次失败把**平台自己的脱敏原因**写进气泡（前缀逐字），而且 origin 门跑在**取气泡之前**。
#[tokio::test]
async fn a_failed_run_writes_the_platform_reason_into_the_bubble() {
    let harness = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(Id::new(), None, Some(Id::new())),
            false,
        ))
        .build();
    let _ = harness.open_round(TASK_ID).await;

    harness
        .indicator
        .handle_task_failed(&TaskEvent::failed(
            TASK_ID,
            Some(harness.session.to_string()),
            Some("上下文超出模型限制"),
        ))
        .await;

    let frames = harness.senders.stream_frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        frames[1].1,
        format!("{TASK_FAILED_PREFIX}上下文超出模型限制")
    );
    assert!(frames[1].2);
    assert_eq!(harness.indicator.depth(), 0);
    // 没有原因时退到文案包。
    assert_eq!(
        failure_text(
            &TaskEvent::failed("t", None::<String>, None::<String>),
            Locale::ZhHans
        ),
        copy_for(Locale::ZhHans).stream_failed
    );
}

/// 🔴 一次**平台已经在重试**的失败不是一个结局：气泡**留着**，那一轮交出死掉那次尝试的 id 回去等
/// （`retry_unbind`）—— 否则重试的答案会落在一个已经宣告失败的气泡下面。
#[tokio::test]
async fn a_retry_pending_failure_leaves_the_bubble_alone() {
    let harness = Harness::builder().build();
    let _ = harness.open_round(TASK_ID).await;

    harness
        .indicator
        .handle_task_failed(
            &TaskEvent::failed(TASK_ID, Some(harness.session.to_string()), Some("boom"))
                .with_retry_pending(true),
        )
        .await;

    assert_eq!(harness.senders.stream_frames().len(), 1, "只有开场帧");
    assert_eq!(harness.indicator.depth(), 1, "气泡留着");
    assert_eq!(
        task_failed_content(
            &TaskEvent::failed("t", None::<String>, Some("boom")).with_retry_pending(true)
        ),
        "",
        "retry_pending 时连文本都扣住"
    );
    // 那一轮回去了：task id 清掉（`retry_unbind` 留下的是"在等谁"的名字）。
    assert!(rounds_task_ids(&harness.streams, harness.session)[0].is_empty());
}

/// 一次**别的平台**的失败在**读 task 行之前**就被挡掉（上游那条
/// `TestAnotherChannelsFailureNeverReachesTheTaskRow`）—— 而且它**不**释放这个房间的轮次。
#[tokio::test]
async fn another_channels_failure_never_reaches_the_task_row() {
    let harness = Harness::builder()
        .deliveries(FakeDeliveries::with_foreign_row())
        .build();
    let _ = harness.open_round(TASK_ID).await;
    // 让 task 行的读**失败**：门必须在它之前就返回（否则这一次读会被记在发布者账上）。
    harness.tasks.fail.store(true, Ordering::SeqCst);

    harness
        .indicator
        .handle_task_failed(&TaskEvent::failed(
            TASK_ID,
            Some(harness.session.to_string()),
            Some("boom"),
        ))
        .await;

    assert_eq!(
        harness.senders.stream_frames().len(),
        1,
        "别人的 run 什么都不说"
    );
    assert_eq!(
        rounds_task_ids(&harness.streams, harness.session),
        vec![TASK_ID.to_string()],
        "也不许释放这个房间的轮次"
    );
}

/// 一次**第一方**的失败（在 Multica 里敲的问题）**释放**这个房间的轮次：不释放会把那一轮留在一个
/// 永远不会收尾的 run 上 —— 提问者看着气泡转，而他们自己的回答找不到轮次。
#[tokio::test]
async fn a_first_party_failure_releases_the_rooms_bubble() {
    let root = Id::new();
    let harness = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(Id::new(), Some(root), Some(Id::new())),
            false,
        ))
        .build();
    // 没有投递行 ⇒ `NoRow` ⇒ 唯一一个值得再花读的答案。
    let _ = harness.open_round(TASK_ID).await;

    harness
        .indicator
        .handle_task_failed(&TaskEvent::failed(
            TASK_ID,
            Some(harness.session.to_string()),
            Some("boom"),
        ))
        .await;

    assert_eq!(
        harness.senders.stream_frames().len(),
        1,
        "不是我们的 run 不许封"
    );
    assert!(
        rounds_task_ids(&harness.streams, harness.session)[0].is_empty(),
        "轮次必须被释放回去等"
    );
    // 气泡**还在**（释放不是封口）：下一个 `task:queued` 会取走它。
    assert_eq!(harness.indicator.depth(), 1);
}

/// 一次**够不着**的 origin 读**什么都不说**、也**不**释放：一次失败的读不是"这次 run 属于别处"的
/// 证据，而在它上面把轮次交出去会丢掉一个这个聊自己的回答还在路上的气泡。
#[tokio::test]
async fn an_unanswerable_origin_neither_speaks_nor_releases() {
    let harness = Harness::builder()
        .tasks(FakeTasks::with_task(task_row(Id::new(), None, None), false))
        .build();
    let _ = harness.open_round(TASK_ID).await;
    harness.tasks.fail.store(true, Ordering::SeqCst);

    harness
        .indicator
        .handle_task_failed(&TaskEvent::failed(
            TASK_ID,
            Some(harness.session.to_string()),
            Some("boom"),
        ))
        .await;

    assert_eq!(harness.senders.stream_frames().len(), 1);
    assert_eq!(
        rounds_task_ids(&harness.streams, harness.session),
        vec![TASK_ID.to_string()]
    );
}

/// 一次失败**没有**气泡可写、但有一个活的投递行 ⇒ 话作为一条普通消息去那个聊（失败的告知是
/// `WeCom` 唯一产出的"那次运行没跑通"，所以值得为它追一个地址）。
#[tokio::test]
async fn a_failure_without_a_bubble_goes_to_the_chat_as_a_plain_message() {
    let installation_id = Id::new();
    let harness = Harness::builder()
        .deliveries(FakeDeliveries::with_live_row(installation_id, "room"))
        .socket(true)
        .build();
    // 没有 `open`：这一轮从没被画出来（重启中途 / 开场帧被拒）。

    harness
        .indicator
        .handle_task_failed(&TaskEvent::failed(
            TASK_ID,
            Some(harness.session.to_string()),
            None::<String>,
        ))
        .await;

    let texts = harness.senders.plain_texts();
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0].0, "room");
    assert_eq!(texts[0].1, CHAT_TYPE_GROUP_INT);
    assert_eq!(texts[0].2, copy_for(Locale::ZhHans).stream_failed);
}

/// 本副本没有 socket 时，那条告知**请求握着它的那个副本**（带着 task id，所以它封的是**对的**
/// 那个气泡），而不是在这里静默丢掉。
#[tokio::test]
async fn a_notice_from_a_replica_without_the_socket_is_routed() {
    let installation_id = Id::new();
    let router = Arc::new(FakeRouter::default());
    let harness = Harness::builder()
        .deliveries(FakeDeliveries::with_live_row(installation_id, "room"))
        .socket(false)
        .router(Arc::clone(&router))
        .build();

    harness
        .indicator
        .handle_task_failed(&TaskEvent::failed(
            TASK_ID,
            Some(harness.session.to_string()),
            None::<String>,
        ))
        .await;

    let frames = router.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, RelayKind::Reply);
    assert_eq!(frames[0].2, TASK_ID);
    assert!(harness.senders.plain_texts().is_empty(), "本副本不发");
}

/// 上游 `handleTaskCancelled`：在气泡里封上 `stream_cancelled`。
#[tokio::test]
async fn a_cancelled_run_seals_its_bubble() {
    let harness = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(Id::new(), None, Some(Id::new())),
            false,
        ))
        .build();
    let _ = harness.open_round(TASK_ID).await;

    harness
        .indicator
        .handle_task_cancelled(&TaskEvent::cancelled(
            TASK_ID,
            Some(harness.session.to_string()),
        ))
        .await;

    let frames = harness.senders.stream_frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].1, copy_for(Locale::ZhHans).stream_cancelled);
    assert!(frames[1].2);
}

/// 🔴 上游逐字：**这里根本没有轮次**并不能说明轮次不存在 —— 气泡在**画它的那个副本**上。没有轮次
/// 时请中继去封（一帧封印帧在没有轮次时什么都不做，所以那道 origin 门不为它而被咨询）。
#[tokio::test]
async fn a_cancellation_without_local_rounds_asks_the_relay() {
    let router = Arc::new(FakeRouter::default());
    let harness = Harness::builder().router(Arc::clone(&router)).build();
    assert!(!harness.indicator.holding());

    harness
        .indicator
        .handle_task_cancelled(&TaskEvent::cancelled(
            TASK_ID,
            Some(harness.session.to_string()),
        ))
        .await;

    let frames = router.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, RelayKind::Seal);
    assert_eq!(frames[0].1, SEAL_REASON_CANCELLED);
    assert_eq!(frames[0].2, TASK_ID);
    assert!(harness.senders.stream_frames().is_empty());

    // 没有中继时它什么都不做（单副本部署的形态）。
    let bare = Harness::builder().build();
    bare.indicator
        .handle_task_cancelled(&TaskEvent::cancelled(
            TASK_ID,
            Some(bare.session.to_string()),
        ))
        .await;
    assert!(bare.senders.stream_frames().is_empty());
}

// =====================================================================
// sessionFor / originOf / 寻址等价
// =====================================================================

/// 🔴 **一次被拒的收尾帧退回一条普通消息**（上游 `writeClosing` 的 `sealNotOnScreen` 那一格）。
///
/// 这一格很重要：`StreamFailed` 是 `WeCom` 唯一产出的"那次运行没跑通"，所以一帧被判了**永久**拒绝的
/// 收尾帧会让用户留着一个转圈、而那句解释**永远**不会到。
#[tokio::test]
async fn a_refused_closing_frame_falls_back_to_a_plain_message() {
    let harness = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(Id::new(), None, Some(Id::new())),
            false,
        ))
        .socket(true)
        .build();
    let _ = harness.open_round(TASK_ID).await;
    // `846605`：这条流再也不会接受一帧 ⇒ `sealNotOnScreen` ⇒ 授权"把话再说一遍"。
    // 它**必须**以 `SenderError::Stream`（服务端的判决）到达：一个裸的 `Api` 拒绝在
    // `is_not_attempted` 上是 `false`、在 `stream_unusable` 上也是 `false` ⇒ 落到 `Unknown`
    // （那一格**不**许再说一遍）。这条正是 `seal::classify_seal` 的三格在类型层面的分野。
    harness
        .senders
        .fail_streams_with(SenderError::Stream(crate::wecom::ws_frame::StreamError {
            code: crate::wecom::ws_frame::ERRCODE_STREAM_BAD_REQ_ID,
            message: "invalid req_id".to_string(),
        }));

    harness
        .indicator
        .handle_task_cancelled(&TaskEvent::cancelled(
            TASK_ID,
            Some(harness.session.to_string()),
        ))
        .await;

    let texts = harness.senders.plain_texts();
    assert_eq!(texts.len(), 1, "话必须作为一条新消息出去");
    assert_eq!(texts[0].2, copy_for(Locale::ZhHans).stream_cancelled);
    assert_eq!(harness.indicator.depth(), 0);

    // 而**结局未知**（ack 没回来）那一格**不**再说一遍：这些话可能已经在气泡里了，而 `WeCom`
    // 没有撤回。
    let unknown = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(Id::new(), None, Some(Id::new())),
            false,
        ))
        .socket(true)
        .build();
    let _ = unknown.open_round(TASK_ID).await;
    unknown.senders.fail_streams_with(SenderError::AckTimeout);
    unknown
        .indicator
        .handle_task_cancelled(&TaskEvent::cancelled(
            TASK_ID,
            Some(unknown.session.to_string()),
        ))
        .await;
    assert!(
        unknown.senders.plain_texts().is_empty(),
        "未知那一格不许重说一遍"
    );

    // 连兜底那条路也失败时它只记一条 warn（不 panic、也不留下一个悬着的句柄）。
    let hopeless = Harness::builder()
        .tasks(FakeTasks::with_task(
            task_row(Id::new(), None, Some(Id::new())),
            false,
        ))
        .socket(true)
        .build();
    let _ = hopeless.open_round(TASK_ID).await;
    hopeless
        .senders
        .fail_streams_with(SenderError::Stream(crate::wecom::ws_frame::StreamError {
            code: crate::wecom::ws_frame::ERRCODE_STREAM_EXPIRED,
            message: "stream finished".to_string(),
        }));
    hopeless.senders.fail_texts_with(SenderError::NotAttempted);
    hopeless
        .indicator
        .handle_task_cancelled(&TaskEvent::cancelled(
            TASK_ID,
            Some(hopeless.session.to_string()),
        ))
        .await;
    assert!(hopeless.senders.plain_texts().is_empty());
    assert_eq!(hopeless.indicator.depth(), 0);
}
