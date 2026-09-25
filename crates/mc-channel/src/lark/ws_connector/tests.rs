//! [`super`]（会话与帧循环）的用例。
//!
//! 三层：
//!
//! 1. **旋钮 / 停机信号**的纯语义；
//! 2. **帧循环**：脚本化的内存 socket（不睡真觉、不开真 socket）覆盖
//!    ping/pong、分片、解码三分支、ACK/NACK、坏帧跳过、读失败 / 读超时 / 正常关闭 / 停机；
//! 3. **凭据纪律**：拨号失败**不**回显那条一次性地址（它等价于凭据）。
//!
//! ⚠️ **退避重连与租约**那半在 [`supervised`]（真 `engine::Supervisor` + 假端口）。

mod harness;
mod supervised;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness::{
    chunk_frame, credentials, data_frame, receive_payload, server_ping_frame, FixedFetcher,
    Harness, RecordingEmitter, ScriptedDialer, SocketLog,
};

use super::*;
use crate::channel::ChannelError;
use crate::lark::ws_endpoint::EndpointFetcher;
use crate::lark::ws_frame::{new_ping_frame, Frame, FRAME_HEADER_TYPE_PING, FRAME_METHOD_CONTROL};
use crate::lark::ws_frame_decoder::{FrameDecoder, LarkJsonFrameDecoder};
// =====================================================================
// 一、旋钮与停机信号
// =====================================================================

#[test]
fn the_session_knobs_default_to_the_upstream_values() {
    let knobs = SessionKnobs::default();
    assert_eq!(knobs.ping_interval, DEFAULT_PING_INTERVAL);
    assert_eq!(knobs.read_deadline, DEFAULT_READ_DEADLINE);
    assert_eq!(knobs.write_timeout, DEFAULT_WRITE_TIMEOUT);
    assert_eq!(knobs.chunk_ttl, crate::lark::ws_frame::DEFAULT_CHUNK_TTL);
    assert_eq!(knobs, SessionKnobs::default());
}

#[tokio::test]
async fn the_stop_signal_pair_is_idempotent_and_wakes_waiters() {
    let (signal, mut handle) = StopSignal::pair();
    assert!(!signal.is_stopped());
    assert!(!handle.is_stopped());
    let waiter = handle.clone();
    let task = tokio::spawn(async move {
        waiter.clone().wait().await;
    });
    signal.stop();
    assert!(signal.is_stopped());
    assert!(handle.is_stopped());
    task.await.expect("waiter");
    // 已经置位的等待端立刻返回。
    handle.wait().await;
    // 重复置位不是错误。
    signal.stop();
}

#[test]
fn the_connector_requires_no_optional_ports() {
    // 三个端口都是必填（上游 `NewWSLongConnConnector` 的 nil 校验）：构造即装配。
    let harness = Harness::new(Vec::new(), SessionKnobs::default());
    assert_eq!(harness.connector.knobs(), SessionKnobs::default());
    assert!(format!("{:?}", harness.connector).contains("<dyn WsDialer>"));
}

#[test]
fn the_socket_harness_decodes_what_the_connector_writes() {
    // 自检：`Frame::unmarshal` 能解出连接器写出的字节（否则下面的断言会在错的地方红）。
    let frame = Frame::unmarshal(&new_ping_frame(42).marshal()).expect("ping");
    assert_eq!(frame.frame_type(), FRAME_HEADER_TYPE_PING);
    assert_eq!(frame.method, FRAME_METHOD_CONTROL);
}

// =====================================================================
// 二、帧循环
// =====================================================================

/// 两条事件 ⇒ 两条 emit、两条 ACK（`code=200`，`message_id` 原样回声），收尾是"对端正常关闭"。
#[tokio::test]
async fn every_decoded_event_is_emitted_and_acked() {
    let harness = Harness::new(
        vec![
            Ok(WsEvent::Binary(data_frame(
                &receive_payload("om-1"),
                "om-1",
            ))),
            Ok(WsEvent::Binary(data_frame(
                &receive_payload("om-2"),
                "om-2",
            ))),
        ],
        SessionKnobs::default(),
    );
    let outcome = harness.run().await.expect("会话正常结束");

    assert_eq!(outcome, SessionOutcome::Closed);
    assert_eq!(harness.emitter.message_ids(), vec!["om-1", "om-2"]);
    let log = harness.log();
    assert_eq!(log.ack_codes(), vec![200, 200]);
    assert_eq!(
        log.acks()
            .iter()
            .map(|frame| frame.header_value("message_id").to_string())
            .collect::<Vec<_>>(),
        vec!["om-1", "om-2"],
        "ACK 必须回声入站帧的 message_id（服务端靠它配对）"
    );
    assert_eq!(log.closes, 1, "会话结束要关一次链接");
    assert_eq!(harness.fetcher.calls(), 1, "每次会话引导一次");
    assert_eq!(harness.dialer.attempts(), 1);
    // 拨的必须是**引导返回的那一条**端点（地址里带 service_id 与一次性 device_id）。
    let dialed = harness.dialer.endpoints();
    assert_eq!(dialed.len(), 1);
    assert_eq!(dialed[0].service_id, 42);
    assert!(dialed[0].url.contains("device_id=dev-1"));
}

/// 服务端 ping（`Service=0`）⇒ pong 的 `Service` 必须是**引导响应**里的 42。
#[tokio::test]
async fn a_server_ping_is_answered_with_the_bootstrap_service_id() {
    let harness = Harness::new(
        vec![Ok(WsEvent::Binary(server_ping_frame()))],
        SessionKnobs::default(),
    );
    harness.run().await.expect("会话正常结束");

    let log = harness.log();
    let pongs: Vec<&Frame> = log
        .written
        .iter()
        .filter(|frame| frame.frame_type() == "pong")
        .collect();
    assert_eq!(pongs.len(), 1, "一条 ping 回一条 pong");
    assert_eq!(
        pongs[0].service, 42,
        "用引导响应的 service_id，不回声入站帧的 0"
    );
    assert_eq!(pongs[0].method, FRAME_METHOD_CONTROL);
    // pong 不是 ACK：它不该把入站 headers 带过去。
    assert!(!pongs[0].has_header("message_id"));
}

/// 应用层心跳按**服务端下发**的间隔发（20ms ⇒ 70ms 里至少 2 条），并且用引导的 service id。
#[tokio::test]
async fn app_layer_pings_use_the_server_provided_interval() {
    let log = Arc::new(Mutex::new(SocketLog::default()));
    let dialer = Arc::new(ScriptedDialer::with_socket(&log, Vec::new(), false));
    let fetcher = Arc::new(FixedFetcher::new(Duration::from_millis(20)));
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    );
    let emitter = RecordingEmitter::new();
    let (signal, handle) = StopSignal::pair();

    let task = tokio::spawn(async move {
        connector
            .run_session(&credentials(), emitter as Arc<dyn EventEmitter>, handle)
            .await
    });
    tokio::time::sleep(Duration::from_millis(75)).await;
    signal.stop();
    let outcome = task.await.expect("join").expect("会话正常结束");

    assert_eq!(outcome, SessionOutcome::Cancelled, "停机不是错误");
    let log = log.lock().expect("log");
    let pings = log.pings();
    assert!(pings.len() >= 2, "20ms 间隔 75ms 里应当至少两条：{pings:?}");
    assert!(pings.iter().all(|frame| frame.service == 42));
    assert_eq!(log.closes, 1);
}

/// 服务端**没**下发 `PingInterval`（0）时回落静态默认值（用例把它调到 20ms）。
#[tokio::test]
async fn the_static_ping_interval_is_the_fallback() {
    let log = Arc::new(Mutex::new(SocketLog::default()));
    let dialer = Arc::new(ScriptedDialer::with_socket(&log, Vec::new(), false));
    // 服务端给的 ping 间隔是 0（省略）⇒ 连接器必须用旋钮里的它。
    let fetcher = Arc::new(FixedFetcher::new(Duration::ZERO));
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    )
    .with_knobs(SessionKnobs {
        ping_interval: Duration::from_millis(20),
        ..SessionKnobs::default()
    });
    let emitter = RecordingEmitter::new();
    let (signal, handle) = StopSignal::pair();
    let task = tokio::spawn(async move {
        connector
            .run_session(&credentials(), emitter as Arc<dyn EventEmitter>, handle)
            .await
    });
    tokio::time::sleep(Duration::from_millis(75)).await;
    signal.stop();
    task.await.expect("join").expect("会话正常结束");
    let log = log.lock().expect("log");
    assert!(
        log.pings().len() >= 2,
        "零值必须回落旋钮（否则会退化成每 0 秒 ping 一次或永不 ping）"
    );
}

/// 心跳形状 / 订阅外的事件类型 ⇒ 丢弃但**仍然** ACK 200（让服务端别再重投）。
#[tokio::test]
async fn an_ignored_payload_is_acked_and_dropped() {
    let harness = Harness::new(
        vec![
            Ok(WsEvent::Binary(data_frame(r#"{"schema":"2.0"}"#, "om-hb"))),
            Ok(WsEvent::Binary(data_frame("", "om-empty"))),
        ],
        SessionKnobs::default(),
    );
    harness.run().await.expect("会话正常结束");
    assert_eq!(harness.emitter.count(), 0);
    assert_eq!(harness.log().ack_codes(), vec![200, 200]);
}

/// 解不开的载荷 ⇒ 告警 + 丢这一帧 + **仍** ACK 200 + 会话继续（不放大成重连风暴）。
#[tokio::test]
async fn an_undecodable_payload_is_acked_and_the_session_continues() {
    let harness = Harness::new(
        vec![
            Ok(WsEvent::Binary(data_frame("not json", "om-bad"))),
            Ok(WsEvent::Binary(data_frame(
                &receive_payload("om-ok"),
                "om-ok",
            ))),
        ],
        SessionKnobs::default(),
    );
    let outcome = harness.run().await.expect("会话正常结束");
    assert_eq!(outcome, SessionOutcome::Closed);
    assert_eq!(harness.emitter.message_ids(), vec!["om-ok"]);
    assert_eq!(
        harness.log().ack_codes(),
        vec![200, 200],
        "坏载荷也要 200（NACK 会让服务端重投一个我们已证明解不开的载荷）"
    );
}

/// 坏掉的**帧信封**（不是一个合法 protobuf）⇒ 跳过 + 会话继续。
#[tokio::test]
async fn a_malformed_frame_envelope_is_skipped() {
    let harness = Harness::new(
        vec![
            Ok(WsEvent::Binary(vec![0x08])),       // 截断的 varint
            Ok(WsEvent::Binary(vec![0x0A, 0x00])), // 字段 1 给了 bytes（wire 类型不符）
            Ok(WsEvent::Binary(Vec::new())),       // 空缓冲
            Ok(WsEvent::Binary(data_frame(
                &receive_payload("om-ok"),
                "om-ok",
            ))),
        ],
        SessionKnobs::default(),
    );
    let outcome = harness.run().await.expect("会话正常结束");
    assert_eq!(outcome, SessionOutcome::Closed);
    assert_eq!(harness.emitter.message_ids(), vec!["om-ok"]);
    // 三帧坏帧都不得 ACK：只有最后那条事件的 ACK 被写出去。
    let log = harness.log();
    assert_eq!(log.ack_codes(), vec![200]);
    assert_eq!(log.written.len(), 1, "每一帧坏帧都要被跳过");
}

/// 文本帧是 lark 侧的 schema 回归 ⇒ 丢弃 + 会话继续（协议只走二进制）。
#[tokio::test]
async fn a_text_frame_is_dropped_without_tearing_the_link() {
    let harness = Harness::new(
        vec![
            Ok(WsEvent::Text("surprise".to_string())),
            Ok(WsEvent::Binary(data_frame(
                &receive_payload("om-ok"),
                "om-ok",
            ))),
        ],
        SessionKnobs::default(),
    );
    let outcome = harness.run().await.expect("会话正常结束");
    assert_eq!(outcome, SessionOutcome::Closed);
    assert_eq!(harness.emitter.message_ids(), vec!["om-ok"]);
}

/// 基础设施失败 ⇒ NACK 500 + 结束会话（supervisor 退避重连）。事件**已**被送出一次。
#[tokio::test]
async fn an_infra_failure_nacks_and_ends_the_session() {
    let log = Arc::new(Mutex::new(SocketLog::default()));
    let dialer = Arc::new(ScriptedDialer::with_socket(
        &log,
        vec![
            Ok(WsEvent::Binary(data_frame(
                &receive_payload("om-1"),
                "om-1",
            ))),
            Ok(WsEvent::Binary(data_frame(
                &receive_payload("om-2"),
                "om-2",
            ))),
        ],
        true,
    ));
    let fetcher = Arc::new(FixedFetcher::new(Duration::from_secs(120)));
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    );
    let emitter = RecordingEmitter::failing();
    let (_signal, handle) = StopSignal::pair();

    let error = connector
        .run_session(&credentials(), emitter as Arc<dyn EventEmitter>, handle)
        .await
        .expect_err("基础设施失败必须结束会话");

    assert!(error.to_string().contains("dispatch failed"), "{error}");
    let log = log.lock().expect("log");
    assert_eq!(
        log.ack_codes(),
        vec![500],
        "第一条事件要 NACK，第二条根本不该被读"
    );
    assert_eq!(log.closes, 1);
}

/// ACK 写失败是**致命**的（事件已 emit ⇒ 不算丢，但这一次会话必须结束让 supervisor 退避重连）。
#[tokio::test]
async fn a_failed_ack_write_ends_the_session_after_the_event_was_delivered() {
    let log = Arc::new(Mutex::new(SocketLog {
        fail_write_at: Some(1),
        ..SocketLog::default()
    }));
    let dialer = Arc::new(ScriptedDialer::with_socket(
        &log,
        vec![Ok(WsEvent::Binary(data_frame(
            &receive_payload("om-1"),
            "om-1",
        )))],
        true,
    ));
    let fetcher = Arc::new(FixedFetcher::new(Duration::from_secs(120)));
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    );
    let emitter = RecordingEmitter::new();
    let (_signal, handle) = StopSignal::pair();

    let error = connector
        .run_session(
            &credentials(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            handle,
        )
        .await
        .expect_err("ACK 写失败要结束会话");

    assert!(error.to_string().contains("ack write failed"), "{error}");
    assert_eq!(
        emitter.count(),
        1,
        "事件已经交付过，不能因为 ACK 写失败而丢"
    );
    assert!(log.lock().expect("log").written.is_empty());
}

// =====================================================================
// 三、分片（与"不重复投递"直接相关）
// =====================================================================

/// 半截分片：**不 emit、不 ACK**（服务端才好重投整条事件）。
#[tokio::test]
async fn a_partial_chunk_is_neither_emitted_nor_acked() {
    let harness = Harness::new(
        vec![Ok(WsEvent::Binary(chunk_frame(
            "{\"schema\"",
            "om-1",
            2,
            0,
        )))],
        SessionKnobs::default(),
    );
    let outcome = harness.run().await.expect("会话正常结束");
    assert_eq!(outcome, SessionOutcome::Closed);
    assert_eq!(harness.emitter.count(), 0);
    assert!(harness.log().written.is_empty(), "半截分片不得 ACK");
}

/// 分片**乱序**凑齐 ⇒ 只 emit 一次、只 ACK 一次；ACK 用**最后到的那片**的 headers。
#[tokio::test]
async fn chunks_reassemble_then_emit_and_ack_once() {
    let payload = receive_payload("om-1");
    let (head, tail) = payload.split_at(payload.len() / 2);
    let harness = Harness::new(
        vec![
            // 先到后片、再到前片。
            Ok(WsEvent::Binary(chunk_frame(tail, "om-1", 2, 1))),
            Ok(WsEvent::Binary(chunk_frame(head, "om-1", 2, 0))),
        ],
        SessionKnobs::default(),
    );
    let outcome = harness.run().await.expect("会话正常结束");
    assert_eq!(outcome, SessionOutcome::Closed);
    assert_eq!(
        harness.emitter.message_ids(),
        vec!["om-1"],
        "整条事件只投一次"
    );
    assert_eq!(harness.log().ack_codes(), vec![200], "只 ACK 一次");
    assert_eq!(harness.log().acks()[0].header_value("message_id"), "om-1");
}

/// ⚠️ **不重复投递**的核心证据：重复片不产生第二次 emit。
#[tokio::test]
async fn a_duplicate_chunk_does_not_emit_twice() {
    let payload = receive_payload("om-1");
    let (head, tail) = payload.split_at(payload.len() / 2);
    let harness = Harness::new(
        vec![
            Ok(WsEvent::Binary(chunk_frame(head, "om-1", 2, 0))),
            Ok(WsEvent::Binary(chunk_frame(head, "om-1", 2, 0))), // 网络重投
            Ok(WsEvent::Binary(chunk_frame(tail, "om-1", 2, 1))),
        ],
        SessionKnobs::default(),
    );
    harness.run().await.expect("会话正常结束");
    assert_eq!(harness.emitter.message_ids(), vec!["om-1"]);
    assert_eq!(harness.log().ack_codes(), vec![200]);
}

/// 畸形的分片 header（`sum=0` 却带 `seq=1`）被读作**单帧事件**并照常走解码 → ACK。
#[tokio::test]
async fn a_degenerate_chunk_header_is_treated_as_a_single_frame() {
    // `sum=0` ⇒ `parse_chunk_headers` 给出 sum=0 ⇒ 绕过重组器，直接当单帧解。
    let payload = receive_payload("om-1");
    let harness = Harness::new(
        vec![Ok(WsEvent::Binary(chunk_frame(&payload, "om-1", 0, 1)))],
        SessionKnobs::default(),
    );
    harness.run().await.expect("会话正常结束");
    assert_eq!(harness.emitter.message_ids(), vec!["om-1"]);
    assert_eq!(harness.log().ack_codes(), vec![200]);
}

// =====================================================================
// 四、收尾：读失败 / 读超时 / 停机 / 引导
// =====================================================================

/// 读失败 ⇒ `Err`（supervisor 按"这次尝试失败"退避重连）。
#[tokio::test]
async fn a_read_error_ends_the_session_with_an_error() {
    let harness = Harness::new(
        vec![Err(ChannelError::Transport {
            message: "socket read failed".to_string(),
        })],
        SessionKnobs::default(),
    );
    let error = harness.run().await.expect_err("读失败必须上报");
    assert!(error.to_string().contains("socket read failed"), "{error}");
    assert_eq!(harness.log().closes, 1, "出错也要关链接");
}

/// 读超时 ⇒ `Err`（"空闲但健康"靠心跳刷读截止，所以超时意味着链路真的死了）。
#[tokio::test]
async fn a_read_deadline_exceeded_ends_the_session() {
    let log = Arc::new(Mutex::new(SocketLog::default()));
    // 挂住的 socket：不会给任何事件。
    let dialer = Arc::new(ScriptedDialer::with_socket(&log, Vec::new(), false));
    let fetcher = Arc::new(FixedFetcher::new(Duration::from_secs(120)));
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    )
    .with_knobs(SessionKnobs {
        read_deadline: Duration::from_millis(30),
        ..SessionKnobs::default()
    });
    let emitter = RecordingEmitter::new();
    let (_signal, handle) = StopSignal::pair();

    let error = connector
        .run_session(&credentials(), emitter as Arc<dyn EventEmitter>, handle)
        .await
        .expect_err("读超时必须上报");
    assert!(
        error.to_string().contains("read deadline exceeded"),
        "{error}"
    );
}

/// 停机 ⇒ `Ok(Cancelled)`（**不是错误**），且置位之后不再投递。
#[tokio::test]
async fn a_stopped_session_returns_cancelled_and_stops_delivering() {
    let log = Arc::new(Mutex::new(SocketLog::default()));
    let dialer = Arc::new(ScriptedDialer::with_socket(
        &log,
        vec![Ok(WsEvent::Binary(data_frame(
            &receive_payload("om-1"),
            "om-1",
        )))],
        false, // 之后挂住
    ));
    let fetcher = Arc::new(FixedFetcher::new(Duration::from_secs(120)));
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    );
    let emitter = RecordingEmitter::new();
    let (signal, handle) = StopSignal::pair();

    let task = tokio::spawn({
        let emitter = Arc::clone(&emitter);
        async move {
            connector
                .run_session(&credentials(), emitter as Arc<dyn EventEmitter>, handle)
                .await
        }
    });
    // 等第一条事件落地，再置停机。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while emitter.count() == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(emitter.count(), 1, "停机前那条事件应当已经被投递");
    signal.stop();

    let outcome = task.await.expect("join").expect("停机不是错误");
    assert_eq!(outcome, SessionOutcome::Cancelled);
    assert_eq!(emitter.message_ids(), vec!["om-1"], "置位后不再投递");
    assert_eq!(log.lock().expect("log").closes, 1, "停机要收链接");
}

/// 引导失败 ⇒ 原样上报，且**不**拨号。
#[tokio::test]
async fn a_fetch_failure_is_propagated_without_dialing() {
    let log = Arc::new(Mutex::new(SocketLog::default()));
    let dialer = Arc::new(ScriptedDialer::with_socket(&log, Vec::new(), true));
    let mut fetcher = FixedFetcher::new(Duration::from_secs(120));
    fetcher.fail = true;
    let fetcher = Arc::new(fetcher);
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    );
    let emitter = RecordingEmitter::new();
    let (_signal, handle) = StopSignal::pair();

    let error = connector
        .run_session(&credentials(), emitter as Arc<dyn EventEmitter>, handle)
        .await
        .expect_err("引导失败必须上报");
    assert!(error.to_string().contains("status 500"), "{error}");
    assert_eq!(dialer.attempts(), 0, "引导失败不该拨号");
}

/// 生产拨号器可用（用例不开真 socket ⇒ 只断言它实现了端口）。
#[test]
fn the_production_dialer_satisfies_the_port() {
    fn assert_port<T: WsDialer>() {}
    assert_port::<TungsteniteDialer>();
    assert_eq!(
        format!("{:?}", TungsteniteDialer::new()),
        "TungsteniteDialer"
    );
}

// =====================================================================
// 五、凭据纪律
// =====================================================================

/// **错误路径不回显凭据**：拨号失败不得透出那条带 `device_id` 的一次性地址。
#[tokio::test]
async fn a_dial_failure_never_echoes_the_single_use_url() {
    let log = Arc::new(Mutex::new(SocketLog::default()));
    let mut dialer = ScriptedDialer::with_socket(&log, Vec::new(), true);
    dialer.fail = true;
    let dialer = Arc::new(dialer);
    let fetcher = Arc::new(FixedFetcher::new(Duration::from_secs(120)));
    let connector = Connector::new(
        Arc::clone(&fetcher) as Arc<dyn EndpointFetcher>,
        Arc::clone(&dialer) as Arc<dyn WsDialer>,
        Arc::new(LarkJsonFrameDecoder::new()) as Arc<dyn FrameDecoder>,
    );
    let emitter = RecordingEmitter::new();
    let (_signal, handle) = StopSignal::pair();

    let error = connector
        .run_session(&credentials(), emitter as Arc<dyn EventEmitter>, handle)
        .await
        .expect_err("拨号失败必须上报");
    let rendered = error.to_string();
    assert!(!rendered.contains("dev-1"), "回显了一次性地址：{rendered}");
    assert!(!rendered.contains("lark.example"), "{rendered}");
    assert!(!rendered.contains("secret-xyz"), "{rendered}");
    // 真正的原因仍然可读。
    assert!(rendered.contains("handshake failed"), "{rendered}");
}

/// **错误路径不回显凭据**：`InstallationCredentials` / `WsEndpoint` 的 `Debug` 都不含明文。
#[test]
fn the_debug_rendering_of_the_session_inputs_is_redacted() {
    let creds = credentials();
    let rendered = format!("{creds:?}");
    assert!(!rendered.contains("secret-xyz"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(rendered.contains("cli_app_x"), "{rendered}");

    let endpoint = WsEndpoint {
        url: "wss://lark.example/ws?device_id=dev-1&service_id=42".to_string(),
        service_id: 42,
        ..WsEndpoint::default()
    };
    let rendered = format!("{endpoint:?}");
    assert!(!rendered.contains("dev-1"), "{rendered}");
}

/// 会话日志只插值非秘密量：这里断言的是"可达的日志面"里没有 secret
/// （`tracing` 的字段名与取值都由本文件的调用点决定）。
#[test]
fn the_connector_never_formats_the_credentials_into_a_frame() {
    // ACK / pong / ping 三种出站帧都不该带上凭据 —— 这是"帧里没有凭据"的可达检查。
    let outbound = [
        new_ping_frame(42),
        Frame {
            payload: Some(b"{\"code\":200,\"headers\":null,\"data\":null}".to_vec()),
            ..Frame::default()
        },
    ];
    for frame in outbound {
        let bytes = frame.marshal();
        assert!(!bytes.windows(10).any(|window| window == b"secret-xyz"));
    }
}
