//! [`super`]（帧信封 + 分片重组）的用例：
//!
//! 1. **黄金字节**：逐字节钉住上游 `ws_frame_test.go` 的五组向量（SDK 兼容性是承重的，见模块文档）；
//! 2. protobuf 的边界（截断 / 空缓冲 / 未知字段 / `Payload` 的 `nil` vs 空）；
//! 3. 分片重组的九条语义（乱序 / 重复 / 滑动 TTL / 惰性 GC / 畸形输入）。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

// =====================================================================
// 一、黄金字节（上游 `TestFrameMarshalIsSDKByteCompatible`）
// =====================================================================

/// 一个黄金向量的形状（抽出别名免得 `clippy::type_complexity` 报警）。
type GoldenFrame = (&'static str, fn() -> Frame, &'static str);

/// 五个向上游逐字抄下来的黄金向量。**不许为了让实现通过而改这些串**
/// （它们钉的是官方 SDK 的 `MarshalToSizedBuffer`；红了说明 wire 兼容性坏了）。
const GOLDEN_FRAMES: &[GoldenFrame] = &[
    (
        "ping_frame",
        || new_ping_frame(7),
        "08001000180720002a0c0a0474797065120470696e6732003a004a00",
    ),
    (
        "pong_frame_service_42",
        || new_pong_frame(42),
        "08001000182a20002a0c0a04747970651204706f6e6732003a004a00",
    ),
    (
        "zero_frame_emits_required_and_opt_strings",
        Frame::default,
        "080010001800200032003a004a00",
    ),
];

#[test]
fn the_three_constructor_goldens_are_byte_exact() {
    for (name, build, expected) in GOLDEN_FRAMES {
        assert_eq!(hex::encode(build().marshal()), *expected, "{name}");
    }
}

#[test]
fn the_ack_golden_is_byte_exact() {
    // 上游 `ack_data_frame`：ACK 复用入站帧的 headers（服务端靠 message_id 配对），
    // 载荷是 SDK `Response` 的 JSON（`headers`/`data` 都是 JSON `null`）。
    let inbound = Frame {
        method: FRAME_METHOD_DATA,
        service: 7,
        headers: vec![
            FrameHeader::new(FRAME_HEADER_TYPE_KEY, FRAME_HEADER_TYPE_EVENT),
            FrameHeader::new(FRAME_HEADER_MESSAGE_ID_KEY, "om-42"),
        ],
        ..Frame::default()
    };
    assert_eq!(
        hex::encode(new_ack_frame(&inbound, true).marshal()),
        "08001000180720012a0d0a047479706512056576656e742a130a0a6d6573736167655f696412056f6d2d343232003a0042277b22636f6465223a3230302c2268656164657273223a6e756c6c2c2264617461223a6e756c6c7d4a00"
    );
}

#[test]
fn the_full_data_frame_golden_is_byte_exact() {
    let frame = Frame {
        seq_id: 42,
        log_id: 99,
        service: 7,
        method: FRAME_METHOD_DATA,
        headers: vec![
            FrameHeader::new(FRAME_HEADER_TYPE_KEY, FRAME_HEADER_TYPE_EVENT),
            FrameHeader::new(FRAME_HEADER_MESSAGE_ID_KEY, "om-1"),
        ],
        payload_encoding: "json".to_string(),
        payload_type: "im.message.receive_v1".to_string(),
        payload: Some(br#"{"schema":"2.0"}"#.to_vec()),
        log_id_new: "log-new".to_string(),
    };
    assert_eq!(
        hex::encode(frame.marshal()),
        "082a1063180720012a0d0a047479706512056576656e742a120a0a6d6573736167655f696412046f6d2d3132046a736f6e3a15696d2e6d6573736167652e726563656976655f763142107b22736368656d61223a22322e30227d4a076c6f672d6e6577"
    );
}

// =====================================================================
// 二、protobuf 边界
// =====================================================================

/// proto2 的 `req` 语义：即使为 0 也要出场（跳过零值曾是"每个 ping 都被 lark 丢掉"的成因）。
#[test]
fn required_zero_fields_are_always_emitted() {
    let raw = Frame::default().marshal();
    assert_eq!(&raw[..8], &[0x08, 0x00, 0x10, 0x00, 0x18, 0x00, 0x20, 0x00]);
}

/// `None`（上游 `nil`）与 `Some(vec![])` 是**两种**字节序列：省略 vs `42 00`。
#[test]
fn a_nil_payload_is_omitted_and_an_empty_one_is_not() {
    let no_payload = Frame::default().marshal();
    assert_eq!(
        hex::encode(&no_payload),
        "080010001800200032003a004a00",
        "nil 载荷必须整个字段省略"
    );

    let empty_payload = Frame {
        payload: Some(Vec::new()),
        ..Frame::default()
    }
    .marshal();
    // 字段 7（`3a 00`）与字段 9（`4a 00`）之间插进 `42 00`。
    assert!(
        empty_payload
            .windows(6)
            .any(|window| window == [0x3a, 0x00, 0x42, 0x00, 0x4a, 0x00]),
        "空载荷要写 tag + 长度 0：{}",
        hex::encode(&empty_payload)
    );
}

#[test]
fn every_field_survives_a_round_trip() {
    let frame = Frame {
        seq_id: 42,
        log_id: 99,
        service: 7,
        method: FRAME_METHOD_DATA,
        headers: vec![
            FrameHeader::new(FRAME_HEADER_TYPE_KEY, FRAME_HEADER_TYPE_EVENT),
            FrameHeader::new(FRAME_HEADER_MESSAGE_ID_KEY, "om-1"),
        ],
        payload_encoding: "json".to_string(),
        payload_type: "im.message.receive_v1".to_string(),
        payload: Some(br#"{"schema":"2.0"}"#.to_vec()),
        log_id_new: "log-new".to_string(),
    };
    assert_eq!(Frame::unmarshal(&frame.marshal()).expect("往返"), frame);
}

#[test]
fn an_empty_or_truncated_buffer_is_an_error() {
    assert_eq!(Frame::unmarshal(&[]), Err(FrameError::Empty));
    assert!(matches!(
        Frame::unmarshal(&[0x08]),
        Err(FrameError::Truncated { .. })
    ));
    // 字段 8 声称 4 字节，实际只有 2 字节。
    assert!(matches!(
        Frame::unmarshal(&[0x42, 0x04, 0xAA, 0xBB]),
        Err(FrameError::Truncated { .. })
    ));
    // 字段号 0 非法。
    assert_eq!(Frame::unmarshal(&[0x00]), Err(FrameError::ZeroField));
    // 字段 1 应当是 varint，给了 bytes。
    assert!(matches!(
        Frame::unmarshal(&[0x0A, 0x00]),
        Err(FrameError::WireType { field: 1, .. })
    ));
}

#[test]
fn unknown_fields_are_skipped() {
    // 字段 3 = 5（我们认），字段 31 = 99（不认识），字段 40 = 长度 2 的串（不认识）。
    let mut buffer = vec![0x18, 0x05];
    buffer.extend_from_slice(&[0xF8, 0x01, 0x63]);
    buffer.extend_from_slice(&[0xC2, 0x02, 0x02, 0xAA, 0xBB]);
    let frame = Frame::unmarshal(&buffer).expect("未知字段应被跳过");
    assert_eq!(frame.service, 5);
}

#[test]
fn header_value_is_first_wins() {
    let frame = Frame {
        headers: vec![
            FrameHeader::new(FRAME_HEADER_TYPE_KEY, "event"),
            FrameHeader::new(FRAME_HEADER_TYPE_KEY, "card"),
            FrameHeader::new(FRAME_HEADER_MESSAGE_ID_KEY, "om-9"),
        ],
        ..Frame::default()
    };
    assert_eq!(frame.header_value(FRAME_HEADER_TYPE_KEY), "event");
    assert_eq!(frame.frame_type(), "event");
    assert_eq!(frame.header_value(FRAME_HEADER_MESSAGE_ID_KEY), "om-9");
    assert!(frame.has_header(FRAME_HEADER_MESSAGE_ID_KEY));
    assert_eq!(frame.header_value("absent"), "");
    assert!(!frame.has_header("absent"));
}

#[test]
fn flags_and_accessors_match_the_wire() {
    let control = new_ping_frame(7);
    assert!(control.is_control());
    assert_eq!(control.payload_bytes(), &[] as &[u8]);
    // ACK 逐字复用入站帧的 `method`（上游逐字）：从控制帧造出来的 ACK 也是控制帧。
    assert!(new_ack_frame(&control, true).is_control());
    assert!(!new_ack_frame(
        &Frame {
            method: FRAME_METHOD_DATA,
            ..Frame::default()
        },
        true
    )
    .is_control());

    let data = Frame {
        method: FRAME_METHOD_DATA,
        payload: Some(b"x".to_vec()),
        ..Frame::default()
    };
    assert!(!data.is_control());
    assert_eq!(data.payload_bytes(), b"x");
}

#[test]
fn nack_carries_code_500_and_echoes_the_headers() {
    let inbound = Frame {
        method: FRAME_METHOD_DATA,
        service: 7,
        headers: vec![FrameHeader::new(FRAME_HEADER_MESSAGE_ID_KEY, "om-42")],
        ..Frame::default()
    };
    let ack = new_ack_frame(&inbound, true);
    assert_eq!(ack.method, inbound.method);
    assert_eq!(ack.service, inbound.service);
    assert_eq!(ack.header_value(FRAME_HEADER_MESSAGE_ID_KEY), "om-42");
    let payload = String::from_utf8(ack.payload.clone().unwrap_or_default()).expect("utf8");
    assert_eq!(payload, r#"{"code":200,"headers":null,"data":null}"#);

    let nack = new_ack_frame(&inbound, false);
    let payload = String::from_utf8(nack.payload.clone().unwrap_or_default()).expect("utf8");
    assert_eq!(payload, r#"{"code":500,"headers":null,"data":null}"#);
}

// =====================================================================
// 三、分片重组
// =====================================================================

/// 可推进的假时钟（`Instant` 不能构造，只能从 `Instant::now()` 起步）。
#[derive(Debug, Clone)]
struct FakeClock(Arc<Mutex<Instant>>);

impl FakeClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn advance(&self, duration: Duration) {
        let mut now = self.0.lock().expect("clock");
        *now += duration;
    }

    fn clock(&self) -> Clock {
        let shared = Arc::clone(&self.0);
        Arc::new(move || *shared.lock().expect("clock"))
    }
}

fn assembler(clock: &FakeClock, ttl: Duration) -> ChunkAssembler {
    ChunkAssembler::new(ttl, clock.clock())
}

#[test]
fn a_single_chunk_message_passes_through() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    assert_eq!(chunks.admit("m-1", 1, 0, b"hello"), Some(b"hello".to_vec()));
    // 凑齐即删条目 ⇒ 不留状态。
    assert_eq!(chunks.pending_count(), 0);
}

#[test]
fn chunks_reassemble_in_sequence_order() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    assert_eq!(chunks.admit("m-1", 3, 0, b"a"), None);
    assert_eq!(chunks.admit("m-1", 3, 1, b"b"), None);
    assert_eq!(chunks.pending_count(), 1);
    assert_eq!(chunks.admit("m-1", 3, 2, b"c"), Some(b"abc".to_vec()));
    assert_eq!(chunks.pending_count(), 0);
}

#[test]
fn chunks_reassemble_out_of_order() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    assert_eq!(chunks.admit("m-1", 2, 1, b"world"), None);
    assert_eq!(
        chunks.admit("m-1", 2, 0, b"hello"),
        Some(b"helloworld".to_vec())
    );
}

/// ⚠️ **不重复投递**的一条证据：同一 `(message_id, seq)` 重发不会多算一片，
/// 也不会第二次交出载荷（条目在凑齐时已被删除）。
#[test]
fn a_duplicate_chunk_is_idempotent() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    assert_eq!(chunks.admit("m-1", 2, 0, b"hello"), None);
    assert_eq!(chunks.admit("m-1", 2, 0, b"hello"), None, "重复片不是完成");
    assert_eq!(chunks.received_count("m-1"), 1, "重复片只算一片");
    assert_eq!(chunks.admit("m-1", 2, 1, b"!"), Some(b"hello!".to_vec()));
    // 条目在凑齐时已被删除 ⇒ 不留残余状态（也不会因为残留而在下一次到达时少算一片）。
    assert_eq!(chunks.received_count("m-1"), 0);
    assert_eq!(chunks.pending_count(), 0);
}

#[test]
fn several_messages_buffer_independently() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    assert_eq!(chunks.admit("m-1", 2, 0, b"a"), None);
    assert_eq!(chunks.admit("m-2", 2, 0, b"x"), None);
    assert_eq!(chunks.pending_count(), 2);
    assert_eq!(chunks.admit("m-2", 2, 1, b"y"), Some(b"xy".to_vec()));
    assert_eq!(chunks.admit("m-1", 2, 1, b"b"), Some(b"ab".to_vec()));
    assert_eq!(chunks.pending_count(), 0);
}

#[test]
fn a_partial_message_expires_after_the_ttl() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    assert_eq!(chunks.admit("m-1", 2, 0, b"a"), None);
    clock.advance(Duration::from_secs(6));
    // 惰性 GC 在 admit 里跑；也可以显式跑一次。
    assert_eq!(chunks.gc_expired(), 1);
    assert_eq!(chunks.pending_count(), 0);
    // 过期之后重来一片 ⇒ 从头开始（不会与旧状态拼出错误的载荷）。
    assert_eq!(chunks.admit("m-1", 2, 1, b"b"), None);
    assert_eq!(chunks.admit("m-1", 2, 0, b"a"), Some(b"ab".to_vec()));
}

#[test]
fn lazy_gc_runs_on_admit() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_millis(100));
    assert_eq!(chunks.admit("m-old", 2, 0, b"a"), None);
    clock.advance(Duration::from_millis(200));
    // 另一条消息的 admit 顺手把过期的清掉（不需要单独的清扫任务）。
    assert_eq!(chunks.admit("m-new", 1, 0, b"z"), Some(b"z".to_vec()));
    assert_eq!(chunks.pending_count(), 0);
}

/// TTL 是**滑动**的：稳定推进的多片事件不会被中途丢弃。
#[test]
fn the_ttl_slides_with_every_chunk() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    // 每片间隔 4s（< TTL），共 3 片 ⇒ 总计 8s > TTL 也必须能拼出来。
    assert_eq!(chunks.admit("m-1", 3, 0, b"a"), None);
    clock.advance(Duration::from_secs(4));
    assert_eq!(chunks.admit("m-1", 3, 1, b"b"), None);
    clock.advance(Duration::from_secs(4));
    assert_eq!(chunks.admit("m-1", 3, 2, b"c"), Some(b"abc".to_vec()));
}

#[test]
fn malformed_chunk_headers_are_ignored() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    let cases = [
        ("", 2, 0, "空 message_id"),
        ("m-1", 0, 0, "sum = 0"),
        ("m-1", -1, 0, "sum < 0"),
        ("m-1", 2, -1, "seq < 0"),
        ("m-1", 2, 2, "seq >= sum"),
    ];
    for (message_id, sum, seq, label) in cases {
        assert_eq!(chunks.admit(message_id, sum, seq, b"x"), None, "{label}");
    }
    assert_eq!(chunks.pending_count(), 0, "畸形输入不得留下状态");

    // 同一个 message_id 被两条 sum 不同的事件复用：越界的片被忽略（失败关闭，不 panic）。
    assert_eq!(chunks.admit("m-2", 2, 0, b"a"), None);
    assert_eq!(chunks.admit("m-2", 4, 3, b"d"), None, "越界片忽略");
    assert_eq!(chunks.received_count("m-2"), 1);
}

#[test]
fn a_non_positive_ttl_falls_back_to_the_sdk_default() {
    let clock = FakeClock::new();
    assert_eq!(
        assembler(&clock, Duration::ZERO).ttl(),
        DEFAULT_CHUNK_TTL,
        "上游：非正 ttl 回落 SDK 的 5 秒"
    );
    assert_eq!(ChunkAssembler::with_defaults().ttl(), DEFAULT_CHUNK_TTL);
}

#[test]
fn parse_chunk_headers_reads_the_three_keys() {
    let frame = Frame {
        headers: vec![
            FrameHeader::new(FRAME_HEADER_SUM_KEY, "3"),
            FrameHeader::new(FRAME_HEADER_SEQ_KEY, "1"),
            FrameHeader::new(FRAME_HEADER_MESSAGE_ID_KEY, "om-7"),
        ],
        ..Frame::default()
    };
    assert_eq!(parse_chunk_headers(&frame), (3, 1, "om-7".to_string()));

    // 缺席 / 解不出整数 ⇒ sum = 0（调用方读作"单帧事件"并绕过重组器）。
    let plain = Frame::default();
    assert_eq!(parse_chunk_headers(&plain), (0, 0, String::new()));
    let broken = Frame {
        headers: vec![
            FrameHeader::new(FRAME_HEADER_SUM_KEY, "not-a-number"),
            FrameHeader::new(FRAME_HEADER_SEQ_KEY, ""),
        ],
        ..Frame::default()
    };
    assert_eq!(parse_chunk_headers(&broken), (0, 0, String::new()));
}

/// 重组器是 `Debug`（诊断），且**不**打印缓冲的字节。
#[test]
fn the_assembler_debug_reports_the_buffer_without_payloads() {
    let clock = FakeClock::new();
    let chunks = assembler(&clock, Duration::from_secs(5));
    assert_eq!(chunks.admit("m-secret", 2, 0, b"TOP-SECRET-BODY"), None);
    let rendered = format!("{chunks:?}");
    assert!(!rendered.contains("TOP-SECRET-BODY"), "{rendered}");
    assert!(rendered.contains("pending: 1"), "{rendered}");
}
