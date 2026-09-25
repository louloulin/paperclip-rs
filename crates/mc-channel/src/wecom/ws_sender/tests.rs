//! `ws_sender` 的用例（上游 `wecom/ws_sender_test.go` 的等价面）。
//!
//! # 替身纪律
//!
//! 替身是**假 socket**，不是假 sender：所有用例都跑真的 [`WsSender`] / [`AckBook`] /
//! [`ChatLocks`]，只有一个实现了 [`WsSink`] 的 [`FakeSink`] 顶掉 `tokio-tungstenite`。
//! 上游 `wsConn` 接口的存在理由逐字就是这件事（"测试能注入一个假实现而不必把整个 gorilla
//! 的表面嵌进来"）。
//!
//! 断言的是**写给 socket 的原始帧**（`serde_json::Value` 字段级），以及**并发写不交错**
//! （`max_inflight` 必须恒为 1）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::wecom::ws_frame::{
    subscribe_body, FrameHeaders, CHAT_TYPE_GROUP_INT, CHAT_TYPE_SINGLE_INT,
    ERRCODE_STREAM_BAD_REQ_ID, ERRCODE_STREAM_EXPIRED,
};

// =====================================================================
// 假 socket
// =====================================================================
mod classify;
mod send;

/// 一次写完之后被调用的钩子（用它把判决送回去，模拟读循环）。
type Hook = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Default)]
struct FakeInner {
    written: Mutex<Vec<Value>>,
    /// 此刻正在写的调用方数。`WsSink::write_text` 拿 `&mut self` ⇒ 它**必须**恒为 1。
    inflight: AtomicUsize,
    max_inflight: AtomicUsize,
    /// 下一次写要扔的错误（扔完就清）。
    failure: Mutex<Option<SinkError>>,
    hook: Mutex<Option<Hook>>,
    closed: AtomicUsize,
}

#[derive(Clone, Default)]
struct FakeSink(Arc<FakeInner>);

impl FakeSink {
    fn written(&self) -> Vec<Value> {
        self.0.written.lock().unwrap().clone()
    }

    fn written_count(&self) -> usize {
        self.0.written.lock().unwrap().len()
    }

    fn written_cmds(&self) -> Vec<String> {
        self.written()
            .iter()
            .map(|frame| frame["cmd"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn max_inflight(&self) -> usize {
        self.0.max_inflight.load(Ordering::SeqCst)
    }

    fn fail_next(&self, error: SinkError) {
        *self.0.failure.lock().unwrap() = Some(error);
    }

    fn on_write(&self, hook: Hook) {
        *self.0.hook.lock().unwrap() = Some(hook);
    }

    fn closed(&self) -> usize {
        self.0.closed.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl WsSink for FakeSink {
    async fn write_text(&mut self, payload: &[u8], _deadline: Instant) -> Result<(), SinkError> {
        let now = self.0.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.max_inflight.fetch_max(now, Ordering::SeqCst);
        // 让出一次执行权：没有串行化的话，这一段就是两帧交错的地方。
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);

        if let Some(error) = self.0.failure.lock().unwrap().take() {
            return Err(error);
        }
        let frame: Value = serde_json::from_slice(payload).expect("every frame must be valid JSON");
        self.0.written.lock().unwrap().push(frame.clone());
        let hook = self.0.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(&frame);
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), SinkError> {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// 一个 sender + 它的假 socket（`ack_timeout` / `ack_poll` 缩到用例尺度）。
fn harness() -> (Arc<WsSender>, FakeSink) {
    let sink = FakeSink::default();
    let sender = WsSender::new(Box::new(sink.clone()))
        .with_ack_timeout(Duration::from_millis(40))
        .with_ack_poll(Duration::from_millis(1));
    (Arc::new(sender), sink)
}

/// 把一次成功判决（`errcode 0`）按 `req_id` 送回去。
fn ack_ok(sender: &WsSender, req_id: &str) {
    let frame = FrameEnvelope {
        headers: FrameHeaders {
            req_id: req_id.to_owned(),
        },
        ..FrameEnvelope::default()
    };
    let _ = sender.route_response(&frame);
}

fn ack_code(sender: &WsSender, req_id: &str, code: i32, message: &str) {
    let frame = FrameEnvelope {
        headers: FrameHeaders {
            req_id: req_id.to_owned(),
        },
        errcode: code,
        error_message: message.to_owned(),
        ..FrameEnvelope::default()
    };
    let _ = sender.route_response(&frame);
}

fn req_id_of(frame: &Value) -> String {
    frame["headers"]["req_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[allow(clippy::unnecessary_wraps)] // 这个助手的存在就是为了在调用点上**写出**生产类型
fn deadline_in(millis: u64) -> Deadline {
    Some(Instant::now() + Duration::from_millis(millis))
}

// =====================================================================
// 并发写（本片专属验收）
// =====================================================================

#[tokio::test]
async fn concurrent_writers_never_interleave() {
    let (sender, sink) = harness();
    let mut tasks = Vec::new();
    for index in 0..8_u32 {
        let sender = Arc::clone(&sender);
        tasks.push(tokio::spawn(async move {
            sender
                .write(&frame_with(
                    &format!("r-{index}"),
                    CMD_PING,
                    serde_json::json!({"n": index}),
                ))
                .await
        }));
    }
    for task in tasks {
        task.await.unwrap().expect("every write succeeds");
    }
    assert_eq!(sink.written_count(), 8);
    assert_eq!(
        sink.max_inflight(),
        1,
        "gorilla forbids concurrent writes; the writer slot is what makes that true here too"
    );
    assert_eq!(sender.written_frames(), 8);
    let mut req_ids: Vec<String> = sink.written().iter().map(req_id_of).collect();
    req_ids.sort();
    assert_eq!(req_ids.len(), 8, "every frame landed exactly once");
}

#[tokio::test]
async fn every_frame_that_reaches_the_socket_is_one_whole_json_document() {
    let (sender, sink) = harness();
    sender.ping().await.unwrap();
    sender
        .write(&frame_with("r", CMD_SEND_MSG, serde_json::json!({"a": 1})))
        .await
        .unwrap();
    assert_eq!(sink.written_cmds(), vec![CMD_PING, CMD_SEND_MSG]);
    for frame in sink.written() {
        assert!(frame["headers"]["req_id"].is_string());
    }
}

#[tokio::test]
async fn a_frame_past_the_cap_is_refused_before_it_reaches_the_socket() {
    let (sender, sink) = harness();
    let huge = "x".repeat(MAX_FRAME_BYTES);
    let error = sender
        .write(&frame_with(
            "r",
            CMD_SEND_MSG,
            serde_json::json!({ "c": huge }),
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(error, SenderError::FrameTooLarge { .. }),
        "{error:?}"
    );
    assert_eq!(sink.written_count(), 0, "nothing may reach the socket");
}

#[tokio::test]
async fn close_reaches_the_sink() {
    let (sender, sink) = harness();
    sender.close().await.unwrap();
    assert_eq!(sink.closed(), 1);
}

// =====================================================================
// request / response 配对
// =====================================================================

#[tokio::test]
async fn request_gets_the_body_of_the_reply_that_echoes_its_req_id() {
    let (sender, sink) = harness();
    let router = Arc::clone(&sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        // 读循环的替身：把应答按 req_id 交回去。
        let _ = router.route_response(&FrameEnvelope {
            headers: FrameHeaders {
                req_id: req_id_of(frame),
            },
            body: serde_json::json!({"ok": true}),
            ..FrameEnvelope::default()
        });
    }));
    let body = sender
        .request(
            deadline_in(500),
            CMD_SUBSCRIBE,
            serde_json::json!({"bot_id": "b"}),
        )
        .await
        .expect("the reply was routed back");
    assert_eq!(body["ok"], true);
    assert_eq!(sink.written_cmds(), vec![CMD_SUBSCRIBE]);
}

#[tokio::test]
async fn subscribe_carries_the_bot_id_and_the_plaintext_on_the_wire() {
    // 明文**只**允许在这条路径上变成 wire 字节（[`SubscribeBody::into_value`] 的出口）。
    let (sender, sink) = harness();
    let secret = super::super::credentials::PlaintextSecret::new("DO-NOT-LOG-secret");
    let router = Arc::clone(&sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        let _ = router.route_response(&FrameEnvelope {
            headers: FrameHeaders {
                req_id: req_id_of(frame),
            },
            ..FrameEnvelope::default()
        });
    }));
    sender
        .subscribe(deadline_in(500), subscribe_body("bot-1", &secret))
        .await
        .expect("subscribed");
    let written = sink.written();
    assert_eq!(written[0]["body"]["bot_id"], "bot-1");
    assert_eq!(written[0]["body"]["secret"], "DO-NOT-LOG-secret");
}

#[tokio::test]
async fn a_nonzero_errcode_becomes_an_api_error_carrying_the_code() {
    let (sender, sink) = harness();
    let router = Arc::clone(&sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        ack_code(&router, &req_id_of(frame), 40001, "invalid secret");
    }));
    let error = sender
        .request(deadline_in(500), CMD_SUBSCRIBE, Value::Null)
        .await
        .unwrap_err();
    let SenderError::Api { cmd, code, message } = error else {
        panic!("expected an api error");
    };
    assert_eq!(cmd, CMD_SUBSCRIBE);
    assert_eq!(code, 40_001);
    assert_eq!(message, "invalid secret");
}

#[tokio::test]
async fn request_times_out_when_no_verdict_ever_comes() {
    let (sender, _sink) = harness();
    let error = sender
        .request(deadline_in(500), CMD_SUBSCRIBE, Value::Null)
        .await
        .unwrap_err();
    assert_eq!(error, SenderError::AckTimeout);
    assert!(
        !error.is_not_attempted(),
        "the frame went out; a lost verdict is not proof of non-delivery"
    );
}

#[tokio::test]
async fn request_with_a_budget_already_spent_writes_nothing() {
    let (sender, sink) = harness();
    let error = sender
        .request(Some(Instant::now()), CMD_SEND_MSG, Value::Null)
        .await
        .unwrap_err();
    assert_eq!(error, SenderError::NotAttempted);
    assert!(error.is_not_attempted(), "nothing was minted or written");
    assert_eq!(sink.written_count(), 0);
}

#[tokio::test]
async fn a_reply_is_only_handed_to_the_req_id_that_asked_for_it() {
    let book = AckBook::new();
    let receiver = book.await_reply("r-1").expect("first registration wins");
    assert!(book.await_reply("r-1").is_none(), "a req_id is spoken for");
    let mut envelope = FrameEnvelope {
        headers: FrameHeaders {
            req_id: "r-2".to_owned(),
        },
        ..FrameEnvelope::default()
    };
    assert!(!book.route_response(&envelope), "nobody waits on r-2");
    envelope.headers.req_id = "r-1".to_owned();
    envelope.body = serde_json::json!({"ok": true});
    assert!(book.route_response(&envelope), "r-1 is claimed");
    let reply = receiver.await.unwrap();
    assert_eq!(reply.body["ok"], true);
    assert!(!book.route_response(&envelope), "a reply is delivered once");
}

#[tokio::test]
async fn route_raw_returns_the_typed_frame_and_routes_the_response() {
    let (sender, _sink) = harness();
    let receiver = sender.book().await_reply("r-7").unwrap();
    let raw = serde_json::to_vec(&serde_json::json!({
        "headers": {"req_id": "r-7"}, "errcode": 0, "body": {"ok": 1}
    }))
    .unwrap();
    let frame = sender.route_raw(&raw).unwrap();
    assert!(matches!(frame, Frame::Response(_)));
    assert_eq!(receiver.await.unwrap().body["ok"], 1);
    assert!(sender.route_raw(b"nope").is_err());
}

// =====================================================================
// 流帧的账本（位置配对）
// =====================================================================

#[tokio::test]
async fn a_verdict_is_matched_to_the_frame_by_position() {
    let book = AckBook::new();
    let waiter = book
        .await_ack(
            "req",
            false,
            false,
            Duration::from_millis(50),
            Duration::from_millis(1),
            None,
        )
        .await
        .unwrap();
    assert!(book.begin_stream_frame("req", "s-1", Some(&waiter), false));
    // 第二个判决（不是第一个）才属于它 ⇒ 第一个必须落空。
    book.deliver_ack("req", 0, "first verdict belongs to an earlier frame");
    tokio::task::yield_now().await;
    book.deliver_ack("req", 0, "second");
    let verdict = waiter.wait(Duration::from_millis(50), None).await.unwrap();
    assert!(verdict.is_ok());
}

#[tokio::test]
async fn an_abandoned_frames_verdict_is_never_handed_to_the_next_one() {
    // 上游逐字：`cancelAck` **故意**留下欠账 —— 把计数补上去会把那一帧的真判决
    // 交给**下一个**写帧的人，而收尾帧是那个付账的。
    let book = AckBook::new();
    let abandoned = book
        .await_ack(
            "req",
            false,
            false,
            Duration::from_millis(20),
            Duration::from_millis(1),
            None,
        )
        .await
        .unwrap();
    assert!(book.begin_stream_frame("req", "s-1", Some(&abandoned), false));
    book.cancel_ack("req", abandoned.id);

    // 服务端还欠着判决 ⇒ 收尾帧不许出去：等到它自己的 ack 预算耗尽，报 StreamBusy
    // （**可证明没发出去**，于是退回普通消息是免费的）。
    let blocked = book
        .await_ack(
            "req",
            true,
            false,
            Duration::from_millis(20),
            Duration::from_millis(1),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(blocked, SenderError::StreamBusy);
    assert!(blocked.is_not_attempted());

    // 那一帧的判决最终到了 —— 没有人等它。它把欠账清掉（`acked` 追上 `sent`），
    // 仅此而已：它**不会**被当成新帧的判决。
    book.deliver_ack("req", 0, "late acceptance of the abandoned frame");
    let next = book
        .await_ack(
            "req",
            true,
            false,
            Duration::from_millis(30),
            Duration::from_millis(1),
            None,
        )
        .await
        .expect("nothing is owed once the late verdict arrived");
    // 真写一帧（`await_ack` 只登记；位置是在写的时点打的）。
    assert!(book.begin_stream_frame("req", "s-2", Some(&next), true));
    // **它自己的**判决（`req_id` 上的第三个）才结算它。
    book.deliver_ack("req", 0, "verdict for the new frame");
    let verdict = next
        .wait(Duration::from_millis(30), None)
        .await
        .expect("a fresh frame waits for its own verdict");
    assert!(verdict.is_ok());
}

#[tokio::test]
async fn an_abandoned_debt_keeps_blocking_until_a_verdict_settles_it() {
    // 上面那条用例的另一半：欠账没清完之前收尾帧一帧都不许出去
    // （上游 `awaitAck` 的第二个条件 —— 光看"有没有人在等"是不够的）。
    let book = AckBook::new();
    let abandoned = book
        .await_ack(
            "req",
            false,
            false,
            Duration::from_millis(10),
            Duration::from_millis(1),
            None,
        )
        .await
        .unwrap();
    assert!(book.begin_stream_frame("req", "s-1", Some(&abandoned), false));
    book.cancel_ack("req", abandoned.id);
    for _ in 0..3 {
        let blocked = book
            .await_ack(
                "req",
                true,
                false,
                Duration::from_millis(10),
                Duration::from_millis(1),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(blocked, SenderError::StreamBusy);
    }
    // 判决到了 ⇒ 放行。
    book.deliver_ack("req", 0, "");
    assert!(book
        .await_ack(
            "req",
            true,
            false,
            Duration::from_millis(10),
            Duration::from_millis(1),
            None
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn a_rewrite_is_allowed_past_an_outstanding_debt() {
    // 上游逐字：`respondStreamRewrite` 是唯一被欠账门放过去的写，因为**同一帧不是第二帧**。
    let book = AckBook::new();
    let abandoned = book
        .await_ack(
            "req",
            false,
            false,
            Duration::from_millis(20),
            Duration::from_millis(1),
            None,
        )
        .await
        .unwrap();
    assert!(book.begin_stream_frame("req", "s-1", Some(&abandoned), true));
    book.cancel_ack("req", abandoned.id);
    let rewrite = book
        .await_ack(
            "req",
            true,
            true,
            Duration::from_millis(20),
            Duration::from_millis(1),
            None,
        )
        .await
        .expect("a rewrite is let past the gate");
    assert!(book.begin_stream_frame("req", "s-1", Some(&rewrite), true));
    book.deliver_ack("req", 0, "");
    book.deliver_ack("req", 0, "");
    assert!(rewrite.wait(Duration::from_millis(30), None).await.is_ok());
}

#[tokio::test]
async fn a_non_final_frame_is_refused_once_the_stream_is_sealed() {
    let (sender, sink) = harness();
    let router = Arc::clone(&sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        ack_ok(&router, &req_id_of(frame));
    }));
    sender
        .respond_stream("req", "s-1", "done", true, deadline_in(500))
        .await
        .expect("the closing frame is accepted");
    let error = sender
        .respond_stream("req", "s-1", "straggler", false, deadline_in(500))
        .await
        .unwrap_err();
    assert_eq!(error, SenderError::StreamSuperseded);
    assert!(
        !error.stream_unusable(),
        "superseded is not a verdict from the server"
    );
    assert_eq!(sink.written_count(), 1, "the straggler never went out");
}

#[tokio::test]
async fn respond_stream_reports_a_server_refusal_as_unusable() {
    let (sender, sink) = harness();
    let router = Arc::clone(&sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        ack_code(
            &router,
            &req_id_of(frame),
            ERRCODE_STREAM_EXPIRED,
            "expired",
        );
    }));
    let error = sender
        .respond_stream("req", "s-1", "late", true, deadline_in(500))
        .await
        .unwrap_err();
    assert!(error.stream_unusable(), "{error:?}");
    let SenderError::Stream(stream) = &error else {
        panic!("expected a stream error");
    };
    assert_eq!(stream.code, ERRCODE_STREAM_EXPIRED);
}

#[tokio::test]
async fn respond_stream_times_out_and_that_timeout_is_not_unusable() {
    // 上游逐字：写失败、ack 没来，都不说这条流的事 ⇒ 都不算 unusable。
    let (sender, _sink) = harness();
    let error = sender
        .respond_stream("req", "s-1", "x", true, deadline_in(500))
        .await
        .unwrap_err();
    assert_eq!(error, SenderError::StreamAckTimeout);
    assert!(!error.stream_unusable());
}

#[tokio::test]
async fn respond_stream_requires_the_callbacks_req_id() {
    let (sender, sink) = harness();
    let error = sender
        .respond_stream("", "s-1", "x", false, deadline_in(500))
        .await
        .unwrap_err();
    assert_eq!(error, SenderError::MissingCallbackReqId);
    assert_eq!(sink.written_count(), 0);
}
