//! 错误分类的用例：`is_not_attempted` / `stream_unusable` 两张表。
//!
//! 本文件是 `ws_sender/tests.rs` 的子模块（门 ⑩ 的 800 行硬限拆分，见 `docs/32` §33 的 D10）。

use super::*;
use crate::wecom::ws_frame::StreamError;

// =====================================================================
// 错误分类
// =====================================================================

#[tokio::test]
async fn a_failure_inside_the_socket_write_is_not_proof_of_non_delivery() {
    let (sender, sink) = harness();
    sink.fail_next(SinkError::write_attempted("broken pipe"));
    let error = sender.ping().await.unwrap_err();
    assert!(
        matches!(error, SenderError::WriteAttempted { .. }),
        "got {error:?}"
    );
    assert!(!error.is_not_attempted());
}

#[tokio::test]
async fn a_failure_before_the_write_is_reported_as_a_plain_sink_error() {
    let (sender, sink) = harness();
    sink.fail_next(SinkError::before_write("cannot set the write deadline"));
    let error = sender.ping().await.unwrap_err();
    assert!(matches!(error, SenderError::Sink(_)), "got {error:?}");
    // 上游 `provablyNotSent` 的 `default: true`：写调用都没进去 ⇒ 一个字节都没出去，
    // 而这个事实正是重试之所以免费的根据（`devices/32` §33 的 D9）。
    assert!(
        error.is_not_attempted(),
        "a failure before the write is proof nothing left this process"
    );
}

#[tokio::test]
async fn the_two_classifiers_do_not_overlap() {
    assert!(SenderError::NotAttempted.is_not_attempted());
    assert!(SenderError::ChatBusy.is_not_attempted());
    assert!(!SenderError::AckAbandoned {
        cause: "budget".to_owned()
    }
    .is_not_attempted());
    assert!(!SenderError::AckTimeout.is_not_attempted());
    assert!(!SenderError::WriteAttempted {
        cause: "pipe".to_owned()
    }
    .is_not_attempted());

    assert!(SenderError::Stream(StreamError {
        code: ERRCODE_STREAM_BAD_REQ_ID,
        message: String::new(),
    })
    .stream_unusable());
    assert!(!SenderError::Stream(StreamError {
        code: 4_502,
        message: String::new(),
    })
    .stream_unusable());
    assert!(!SenderError::AckTimeout.stream_unusable());
}

#[tokio::test]
async fn a_request_that_loses_its_budget_after_the_write_says_so() {
    let (sender, sink) = harness();
    let router = Arc::clone(&sender);
    // 钩子只记录，不回答 ⇒ 判决永远不来，而调用方的预算先到。
    sink.on_write(Arc::new(move |_frame: &Value| {
        let _ = &router;
    }));
    let error = sender
        .request(deadline_in(10), CMD_SEND_MSG, Value::Null)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SenderError::AckAbandoned { .. }),
        "got {error:?}"
    );
    assert!(
        !error.is_not_attempted(),
        "the frame went out; this is the opposite fact from a pre-write cancellation"
    );
    assert_eq!(sink.written_count(), 1, "the frame did reach the socket");
}
