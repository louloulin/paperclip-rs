//! `send_text`（分帧 + 每聊的顺序）与每聊一把锁的用例。
//!
//! 本文件是 `ws_sender/tests.rs` 的子模块（门 ⑩ 的 800 行硬限拆分，见 `docs/32` §33 的 D10）。

use super::*;

// =====================================================================
// send_text（分帧 + 每聊的顺序）
// =====================================================================

fn acking_sink(sender: &Arc<WsSender>, sink: &FakeSink) {
    let router = Arc::clone(sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        ack_ok(&router, &req_id_of(frame));
    }));
}

#[tokio::test]
async fn send_text_ships_one_frame_for_a_short_answer() {
    let (sender, sink) = harness();
    acking_sink(&sender, &sink);
    sender
        .send_text("chat-1", CHAT_TYPE_GROUP_INT, "短回答", deadline_in(500))
        .await
        .unwrap();
    let written = sink.written();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0]["cmd"], CMD_SEND_MSG);
    assert_eq!(written[0]["body"]["chatid"], "chat-1");
    assert_eq!(written[0]["body"]["msgtype"], "markdown");
    assert_eq!(written[0]["body"]["markdown"]["content"], "短回答");
}

#[tokio::test]
async fn send_text_splits_a_long_answer_into_frames_the_server_accepts() {
    let (sender, sink) = harness();
    acking_sink(&sender, &sink);
    let content = "a".repeat(SEND_MSG_CONTENT_LIMIT * 2 + 100);
    sender
        .send_text("chat-1", CHAT_TYPE_SINGLE_INT, &content, deadline_in(2000))
        .await
        .unwrap();
    let written = sink.written();
    assert!(written.len() >= 3, "got {} frames", written.len());
    for frame in &written {
        let body = frame["body"]["markdown"]["content"].as_str().unwrap();
        assert!(
            body.len() <= SEND_MSG_CONTENT_LIMIT,
            "a body past the cap is refused WHOLE and arrives as errcode 45002"
        );
    }
}

#[tokio::test]
async fn a_failure_after_the_first_piece_is_partially_sent() {
    let (sender, sink) = harness();
    let router = Arc::clone(&sender);
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    sink.on_write(Arc::new(move |frame: &Value| {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            ack_ok(&router, &req_id_of(frame));
        } else {
            // 45002 = 正文超过上限（上游逐字：整条被拒，拒绝在 ack 上到达）。
            ack_code(&router, &req_id_of(frame), 45_002, "content too long");
        }
    }));
    let content = "a".repeat(SEND_MSG_CONTENT_LIMIT * 2 + 100);
    let error = sender
        .send_text("chat-1", CHAT_TYPE_SINGLE_INT, &content, deadline_in(2000))
        .await
        .unwrap_err();
    let SenderError::PartiallySent { cause } = &error else {
        panic!("expected a partially-sent error, got {error:?}");
    };
    assert!(cause.contains("45002"), "{cause}");
    assert!(
        !error.is_not_attempted(),
        "piece one is already in the user's chat"
    );
}

#[tokio::test]
async fn send_text_stops_at_the_first_failed_piece() {
    let (sender, sink) = harness();
    let router = Arc::clone(&sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        ack_code(&router, &req_id_of(frame), 45_002, "refused");
    }));
    let content = "a".repeat(SEND_MSG_CONTENT_LIMIT * 3);
    let error = sender
        .send_text("chat-1", CHAT_TYPE_SINGLE_INT, &content, deadline_in(2000))
        .await
        .unwrap_err();
    assert!(
        matches!(error, SenderError::Api { .. }),
        "the first piece's failure is returned as itself, got {error:?}"
    );
    assert_eq!(sink.written_count(), 1, "the tail is not sent alone");
}

#[tokio::test]
async fn two_answers_to_different_chats_do_not_queue_behind_each_other() {
    // 上游逐字：按**聊**而不是按连接 —— 给另一个房间的第二条回答没有理由排在这条后面。
    let (sender, sink) = harness();
    acking_sink(&sender, &sink);
    let first = sender
        .chat_locks()
        .acquire("chat-a", deadline_in(500))
        .await;
    assert!(first.is_ok());
    let second = sender.chat_locks().acquire("chat-b", deadline_in(50)).await;
    assert!(second.is_ok(), "a different chat's turn is its own");
}

// =====================================================================
// 每聊一把锁
// =====================================================================

#[tokio::test]
async fn a_free_chat_is_taken_without_consulting_the_deadline() {
    // 上游逐字：Go 的 `select` 在就绪的分支里**随机**取 ⇒ 一个预算已经用完的调用方走到一个
    // 没人在用的聊前面会被"半个回合"拒掉，而那个聊根本没人在用。
    let locks = Arc::new(ChatLocks::new());
    let guard = locks.acquire("chat-1", Some(Instant::now())).await;
    assert!(guard.is_ok(), "an unused chat is not a busy chat");
}

#[tokio::test]
async fn a_held_chat_reports_busy_when_the_turn_never_comes() {
    let locks = Arc::new(ChatLocks::new());
    let held = locks.acquire("chat-1", deadline_in(500)).await.unwrap();
    let error = locks.acquire("chat-1", deadline_in(20)).await.unwrap_err();
    assert_eq!(error, SenderError::ChatBusy);
    assert!(
        error.is_not_attempted(),
        "the lock is taken before a frame is built, so this is a provably-non-delivery"
    );
    drop(held);
    assert!(locks.acquire("chat-1", deadline_in(20)).await.is_ok());
}

#[tokio::test]
async fn the_lock_table_forgets_a_chat_once_its_last_holder_leaves() {
    let locks = Arc::new(ChatLocks::new());
    assert_eq!(locks.tracked_chats(), 0);
    {
        let _guard = locks.acquire("chat-1", deadline_in(500)).await.unwrap();
        assert_eq!(locks.tracked_chats(), 1);
    }
    assert_eq!(
        locks.tracked_chats(),
        0,
        "a process that has talked to many chats must not keep a row for each"
    );
}
