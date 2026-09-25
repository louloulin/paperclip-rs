//! `media_upload` 的用例：三步上传的 **wire 形状**、重发纪律（只有"判决没回来"值得再问一次）、
//! 并发阶梯、发送那一步的**每聊锁**，以及 body 校验。
//!
//! 替身纪律与 `ws_sender::tests` 一致：全跑真的 [`WsSender`] / `AckBook` / [`ChatLocks`]，
//! 只有 socket 是假的。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::wecom::ws_frame::{FrameEnvelope, FrameHeaders};
use crate::wecom::ws_sender::{SinkError, WsSink};

/// 假 socket：记下每一帧，并按一个"按 cmd 回答"的钩子送回判决。
#[derive(Default)]
struct FakeInner {
    written: Mutex<Vec<Value>>,
    inflight: AtomicUsize,
    max_inflight: AtomicUsize,
}

#[derive(Clone, Default)]
struct FakeSink(Arc<FakeInner>);

impl FakeSink {
    fn written(&self) -> Vec<Value> {
        self.0.written.lock().unwrap().clone()
    }

    fn cmds(&self) -> Vec<String> {
        self.written()
            .iter()
            .map(|frame| frame["cmd"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn max_inflight(&self) -> usize {
        self.0.max_inflight.load(Ordering::SeqCst)
    }

    fn frames_for(&self, cmd: &str) -> Vec<Value> {
        self.written()
            .into_iter()
            .filter(|frame| frame["cmd"].as_str() == Some(cmd))
            .collect()
    }
}

#[async_trait::async_trait]
impl WsSink for FakeSink {
    async fn write_text(
        &mut self,
        payload: &[u8],
        _deadline: std::time::Instant,
    ) -> Result<(), SinkError> {
        let now = self.0.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.max_inflight.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
        let frame: Value = serde_json::from_slice(payload).expect("valid JSON frame");
        self.0.written.lock().unwrap().push(frame);
        Ok(())
    }

    async fn close(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
}

/// 一个 sender + 它的假 socket（截止时刻缩到用例尺度）。
fn harness() -> (Arc<WsSender>, FakeSink) {
    let sink = FakeSink::default();
    let sender = WsSender::new(Box::new(sink.clone()))
        .with_ack_timeout(Duration::from_millis(30))
        .with_ack_poll(Duration::from_millis(1));
    (Arc::new(sender), sink)
}

/// 按 `errcode 0` + 一个 body 回答一帧。
fn ack(sender: &WsSender, req_id: &str, body: Value) {
    let _ = sender.route_response(&FrameEnvelope {
        headers: FrameHeaders {
            req_id: req_id.to_owned(),
        },
        body,
        ..FrameEnvelope::default()
    });
}

/// 按 `errcode` 拒绝一帧。
fn refuse(sender: &WsSender, req_id: &str, code: i32, message: &str) {
    let _ = sender.route_response(&FrameEnvelope {
        headers: FrameHeaders {
            req_id: req_id.to_owned(),
        },
        errcode: code,
        error_message: message.to_owned(),
        ..FrameEnvelope::default()
    });
}

/// 一个把三步都答对了的（最小）读循环替身。
///
/// 用了 `on_write` 钩子做不到"写下之后回答"（钩子在写者槽里跑、再 await 会死锁），所以这里用
/// 一个后台任务轮询 socket 上还没被回答的帧 —— 与生产里读循环的位置一致。
#[allow(clippy::needless_pass_by_value)] // 值被**移动**进那个脱离任务里
fn spawn_answerer(sender: Arc<WsSender>, sink: FakeSink, behaviour: AnswerBehaviour) {
    let chunk_frames = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        let mut answered = 0usize;
        loop {
            tokio::time::sleep(Duration::from_millis(2)).await;
            let frames = sink.written();
            while answered < frames.len() {
                let frame = &frames[answered];
                answered += 1;
                let cmd = frame["cmd"].as_str().unwrap_or_default().to_owned();
                let req_id = frame["headers"]["req_id"].as_str().unwrap_or_default();
                if req_id.is_empty() {
                    continue;
                }
                match cmd.as_str() {
                    CMD_UPLOAD_MEDIA_INIT => ack(&sender, req_id, json!({ "upload_id": "up-1" })),
                    CMD_UPLOAD_MEDIA_FINISH => {
                        ack(&sender, req_id, json!({ "media_id": "media-9" }));
                    }
                    CMD_UPLOAD_MEDIA_CHUNK => {
                        let seen = chunk_frames.fetch_add(1, Ordering::SeqCst);
                        if seen >= behaviour.silent_chunks {
                            ack(&sender, req_id, Value::Null);
                        }
                        // 否则**不回答**：模拟判决丢在回来的路上。
                    }
                    _ => ack(&sender, req_id, Value::Null),
                }
            }
        }
    });
}

/// 读循环替身的行为。
struct AnswerBehaviour {
    /// 前几块**不回答**（判决丢在回来的路上）。
    silent_chunks: usize,
}

impl AnswerBehaviour {
    fn answering() -> Self {
        Self { silent_chunks: 0 }
    }
}

fn media(kind: MediaMsgType, filename: &str, size: usize) -> OutboundMedia {
    OutboundMedia {
        kind,
        filename: filename.to_owned(),
        data: vec![0xab; size],
    }
}

// =====================================================================
// 三步上传的 wire 形状
// =====================================================================

/// 三个 cmd **按顺序**出去，而且每一帧的 body 是文档说的那个形状。
#[tokio::test]
async fn the_three_steps_go_out_in_order_with_the_documented_bodies() {
    let (sender, sink) = harness();
    spawn_answerer(
        Arc::clone(&sender),
        sink.clone(),
        AnswerBehaviour::answering(),
    );

    // 两块：1 块 + 1 字节，切出来的两块大小不同。
    let payload = media(MediaMsgType::File, "季报.xlsx", MEDIA_CHUNK_BYTES + 1);
    let media_id = upload_media(&sender, &payload, None).await.expect("upload");
    assert_eq!(media_id, "media-9");

    let cmds = sink.cmds();
    assert_eq!(
        cmds,
        vec![
            CMD_UPLOAD_MEDIA_INIT,
            // 两块**并发**发出 ⇒ 顺序不保证，但集合要齐。
            CMD_UPLOAD_MEDIA_CHUNK,
            CMD_UPLOAD_MEDIA_CHUNK,
            CMD_UPLOAD_MEDIA_FINISH,
        ],
        "{cmds:?}"
    );

    let init = &sink.frames_for(CMD_UPLOAD_MEDIA_INIT)[0];
    let body = &init["body"];
    assert_eq!(body["type"], "file");
    assert_eq!(body["filename"], "季报.xlsx");
    assert_eq!(body["total_size"], MEDIA_CHUNK_BYTES + 1);
    assert_eq!(body["total_chunks"], 2);
    assert!(body.get("md5").is_none(), "md5 刻意不发");

    let chunks = sink.frames_for(CMD_UPLOAD_MEDIA_CHUNK);
    assert_eq!(chunks.len(), 2);
    let mut indices: Vec<u64> = chunks
        .iter()
        .map(|frame| {
            frame["body"]["chunk_index"]
                .as_u64()
                .expect("数字，不是字符串")
        })
        .collect();
    indices.sort_unstable();
    assert_eq!(indices, vec![0, 1]);
    for frame in &chunks {
        assert_eq!(frame["body"]["upload_id"], "up-1");
        let encoded = frame["body"]["base64_data"].as_str().expect("base64");
        let decoded = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("valid base64")
        };
        assert!(decoded.len() <= MEDIA_CHUNK_BYTES);
        assert!(decoded.iter().all(|byte| *byte == 0xab));
    }
    // 两块加起来正好是原始长度。
    let total: usize = chunks
        .iter()
        .map(|frame| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(frame["body"]["base64_data"].as_str().unwrap_or_default())
                .map(|decoded| decoded.len())
                .unwrap_or_default()
        })
        .sum();
    assert_eq!(total, MEDIA_CHUNK_BYTES + 1);

    let finish = &sink.frames_for(CMD_UPLOAD_MEDIA_FINISH)[0];
    assert_eq!(finish["body"]["upload_id"], "up-1");
}

/// 🔴 **重发纪律**：只有"判决没回来"再问一次；一次**拒绝**当场结束。
#[tokio::test]
async fn only_a_missing_verdict_is_worth_asking_again() {
    // ① 第一块不回答 ⇒ 第二次尝试拿到判决 ⇒ 上传成功，而那一块**出现过两次**。
    let (sender, sink) = harness();
    spawn_answerer(
        Arc::clone(&sender),
        sink.clone(),
        AnswerBehaviour { silent_chunks: 1 },
    );
    let payload = media(MediaMsgType::Image, "shot.png", 128);
    let media_id = upload_media(&sender, &payload, None).await.expect("upload");
    assert_eq!(media_id, "media-9");
    assert_eq!(
        sink.frames_for(CMD_UPLOAD_MEDIA_CHUNK).len(),
        2,
        "一块被问过两次"
    );

    // ② 服务端**拒绝** ⇒ 不再问第二次，错误带上块序号。
    let (sender, sink) = harness();
    let refuse_sender = Arc::clone(&sender);
    let refuse_sink = sink.clone();
    tokio::spawn(async move {
        let mut answered = 0usize;
        loop {
            tokio::time::sleep(Duration::from_millis(2)).await;
            let frames = refuse_sink.written();
            while answered < frames.len() {
                let frame = &frames[answered];
                answered += 1;
                let cmd = frame["cmd"].as_str().unwrap_or_default().to_owned();
                let req_id = frame["headers"]["req_id"].as_str().unwrap_or_default();
                if req_id.is_empty() {
                    continue;
                }
                match cmd.as_str() {
                    CMD_UPLOAD_MEDIA_INIT => {
                        ack(&refuse_sender, req_id, json!({ "upload_id": "up-1" }));
                    }
                    CMD_UPLOAD_MEDIA_CHUNK => {
                        refuse(&refuse_sender, req_id, 40001, "invalid media");
                    }
                    _ => ack(&refuse_sender, req_id, json!({ "media_id": "media-9" })),
                }
            }
        }
    });
    let error = upload_media(&sender, &payload, None)
        .await
        .expect_err("refused chunk");
    assert!(matches!(error, MediaUploadError::Chunk { .. }), "{error:?}");
    assert_eq!(
        sink.frames_for(CMD_UPLOAD_MEDIA_CHUNK).len(),
        1,
        "一次拒绝不该被再问一次"
    );
}

/// 并发阶梯逐格对齐 SDK。
#[test]
fn the_parallelism_ladder_matches_the_sdk() {
    assert_eq!(media_chunk_parallelism(0), 1);
    assert_eq!(media_chunk_parallelism(1), 1);
    assert_eq!(media_chunk_parallelism(4), 4);
    assert_eq!(media_chunk_parallelism(5), 3);
    assert_eq!(media_chunk_parallelism(10), 3);
    assert_eq!(media_chunk_parallelism(11), 2);
    assert_eq!(media_chunk_parallelism(100), 2);
}

/// 切块：借用的片、长度、以及两条反例。
#[test]
fn splitting_refuses_empty_and_oversize_and_aliases_the_input() {
    assert_eq!(split_media_chunks(&[]), Err(MediaUploadError::UploadEmpty));
    assert_eq!(
        split_media_chunks(&vec![0u8; MAX_MEDIA_UPLOAD_BYTES + 1]),
        Err(MediaUploadError::UploadTooLarge)
    );
    let data = vec![7u8; MEDIA_CHUNK_BYTES + 5];
    let chunks = split_media_chunks(&data).expect("split");
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), MEDIA_CHUNK_BYTES);
    assert_eq!(chunks[1].len(), 5);
    // 片**借用**输入（指进同一个缓冲区），不是拷贝。
    assert_eq!(chunks[0].as_ptr(), data.as_ptr());
    // 上限恰好是 20 MB ⇒ 100 片（传输能表达 100 片，但 20 MB 只要 40 片）。
    let largest = vec![0u8; MAX_MEDIA_UPLOAD_BYTES];
    let max = split_media_chunks(&largest).expect("split");
    assert_eq!(
        max.len(),
        MAX_MEDIA_UPLOAD_BYTES.div_ceil(MEDIA_CHUNK_BYTES)
    );
    assert!(max.len() <= MAX_MEDIA_CHUNKS);
    assert_eq!(MAX_MEDIA_TRANSPORT_BYTES, 50 << 20);
}

// =====================================================================
// 发送
// =====================================================================

/// 发送写**恰好一帧** `aibot_send_msg`，而且**持有**这个聊的锁。
#[tokio::test]
async fn sending_writes_one_frame_and_holds_the_chat_lock() {
    let (sender, sink) = harness();
    let answer = Arc::clone(&sender);
    let answered_sink = sink.clone();
    tokio::spawn(async move {
        let mut answered = 0usize;
        loop {
            tokio::time::sleep(Duration::from_millis(2)).await;
            let frames = answered_sink.written();
            while answered < frames.len() {
                let req_id = frames[answered]["headers"]["req_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                answered += 1;
                if !req_id.is_empty() {
                    ack(&answer, &req_id, Value::Null);
                }
            }
        }
    });

    // 先把这个聊的锁拿着 ⇒ 发送拿不到锁，报"确定没发出去"。
    let held = sender
        .chat_locks()
        .acquire("chat-1", None)
        .await
        .expect("lock");
    let send = MediaSend {
        kind: MediaMsgType::Image,
        media_id: "media-1".to_owned(),
        title: String::new(),
        description: String::new(),
    };
    let error = send_media(
        &sender,
        "chat-1",
        1,
        &send,
        Some(std::time::Instant::now() + Duration::from_millis(20)),
    )
    .await
    .expect_err("锁在别人手上");
    assert!(
        matches!(error, MediaUploadError::Send(SenderError::ChatBusy)),
        "{error:?}"
    );
    assert!(sink.frames_for(CMD_SEND_MSG).is_empty());

    drop(held);
    send_media(&sender, "chat-1", 1, &send, None)
        .await
        .expect("delivered");
    let frames = sink.frames_for(CMD_SEND_MSG);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["body"]["msgtype"], "image");
    assert_eq!(frames[0]["body"]["image"]["media_id"], "media-1");
    assert_eq!(frames[0]["body"]["chatid"], "chat-1");
    assert_eq!(frames[0]["body"]["chat_type"], 1);
    // 非视频的种类**不带** title / description（带了会被拒）。
    assert!(frames[0]["body"]["image"].get("title").is_none());
    assert_eq!(sink.max_inflight(), 1, "每一帧的写都是串行的");
}

/// 视频带两个字段，而且按**字节**截断在字符边界上。
#[test]
fn video_carries_clipped_title_and_description_only_for_video() {
    let long_title = "标".repeat(40); // 120 字节
    let long_description = "说".repeat(300); // 900 字节
    let send = MediaSend {
        kind: MediaMsgType::Video,
        media_id: "m".to_owned(),
        title: long_title,
        description: long_description,
    };
    let body = media_body_fields(&send).expect("fields");
    assert_eq!(body["msgtype"], "video");
    let title = body["video"]["title"].as_str().expect("title");
    assert!(title.len() <= VIDEO_TITLE_BYTES);
    assert_eq!(title.len() % 3, 0, "切在字符边界上");
    let description = body["video"]["description"].as_str().expect("description");
    assert!(description.len() <= VIDEO_DESCRIPTION_BYTES);
    assert_eq!(description.len() % 3, 0);

    for kind in [MediaMsgType::File, MediaMsgType::Image, MediaMsgType::Voice] {
        let send = MediaSend {
            kind,
            media_id: "m".to_owned(),
            title: "ignored".to_owned(),
            description: "ignored".to_owned(),
        };
        let body = media_body_fields(&send).expect("fields");
        let nested = &body[kind.as_str()];
        assert!(nested.get("title").is_none(), "{}", kind.as_str());
        assert!(nested.get("description").is_none(), "{}", kind.as_str());
    }
}

/// body 校验的三条反例 + 文件名。
#[test]
fn body_and_media_validation_refuses_what_the_server_would() {
    let no_chat = send_msg_media_body(
        "",
        1,
        &MediaSend {
            kind: MediaMsgType::File,
            media_id: "m".to_owned(),
            title: String::new(),
            description: String::new(),
        },
    );
    assert_eq!(no_chat, Err(MediaUploadError::MissingChatId));

    let bad_type = send_msg_media_body(
        "c",
        3,
        &MediaSend {
            kind: MediaMsgType::File,
            media_id: "m".to_owned(),
            title: String::new(),
            description: String::new(),
        },
    );
    assert_eq!(bad_type, Err(MediaUploadError::BadChatType));

    assert_eq!(
        media_body_fields(&MediaSend {
            kind: MediaMsgType::File,
            media_id: String::new(),
            title: String::new(),
            description: String::new(),
        }),
        Err(MediaUploadError::MissingMediaId)
    );

    assert_eq!(
        media(MediaMsgType::File, "   ", 1).validate(),
        Err(MediaUploadError::MissingFilename)
    );
    assert!(media(MediaMsgType::Voice, "note.amr", 1).validate().is_ok());

    assert_eq!(clip_utf8("abc", 10), "abc");
    assert_eq!(clip_utf8("abcdef", 3), "abc");
    assert_eq!(clip_utf8("中文名字", 4), "中");
    assert_eq!(base64_encode(b"hi"), "aGk=");
}
