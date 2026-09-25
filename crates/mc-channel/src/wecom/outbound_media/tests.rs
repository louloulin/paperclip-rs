//! `outbound_media` 的用例：种类的降级规则、名字、三态判决、读对象的平台帽，
//! 以及投递那条路上的两个计数器与记账。

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pretty_assertions::assert_eq;

use super::*;
use crate::wecom::metrics::Metrics;

// =====================================================================
// 纯函数
// =====================================================================

/// 种类的判断：内容类型领跑、扩展名兜底、超过某种类帽的**降级成文件**。
#[test]
fn the_kind_demotes_an_oversize_kind_to_a_plain_file() {
    assert_eq!(
        wecom_media_kind("image/png", "a.png", 1024),
        MediaMsgType::Image
    );
    // 图片帽 10 MB：刚好在内。
    assert_eq!(
        wecom_media_kind("image/png", "a.png", MAX_OUTBOUND_IMAGE_BYTES),
        MediaMsgType::Image
    );
    // 超过 1 字节 ⇒ 降级成文件（文件卡用户打得开，被拒的图片不能）。
    assert_eq!(
        wecom_media_kind("image/png", "a.png", MAX_OUTBOUND_IMAGE_BYTES + 1),
        MediaMsgType::File
    );
    assert_eq!(
        wecom_media_kind("video/mp4", "a.mp4", MAX_OUTBOUND_VIDEO_BYTES + 1),
        MediaMsgType::File
    );
    // 语音**只**认 AMR：一条 mp3 作为文件至少点一下就能放。
    assert_eq!(
        wecom_media_kind("audio/amr", "a.amr", 1024),
        MediaMsgType::Voice
    );
    assert_eq!(
        wecom_media_kind("audio/amr", "a.amr", MAX_OUTBOUND_VOICE_BYTES + 1),
        MediaMsgType::File
    );
    assert_eq!(
        wecom_media_kind("audio/mpeg", "a.mp3", 1024),
        MediaMsgType::File
    );
    // 内容类型说不出有用的东西 ⇒ 看扩展名。
    assert_eq!(wecom_media_kind("", "季报.png", 1024), MediaMsgType::Image);
    assert_eq!(
        wecom_media_kind("application/octet-stream", "clip.mp4", 1024),
        MediaMsgType::Video
    );
    // 两个都说不出 ⇒ 文件。
    assert_eq!(
        wecom_media_kind("application/octet-stream", "mystery", 1024),
        MediaMsgType::File
    );
    // 带参数的头上也能认出类型。
    assert_eq!(
        wecom_media_kind("image/jpeg; charset=binary", "a.jpg", 1024),
        MediaMsgType::Image
    );
}

/// 名字压成一个路径段，并在没有扩展名时补一个（`WeCom` 关于格式拿到的唯一提示）。
#[test]
fn the_outbound_name_is_one_segment_with_an_extension() {
    assert_eq!(outbound_media_name("季报.xlsx", ""), "季报.xlsx");
    assert_eq!(
        outbound_media_name("dir/report", "application/pdf"),
        "report.pdf"
    );
    assert_eq!(outbound_media_name("..\\..\\evil", "image/png"), "evil.png");
    // 没有名字 ⇒ `attachment` + 扩展名。
    assert_eq!(outbound_media_name("", "image/png"), "attachment.png");
    // 类型也认不出来 ⇒ 就给 `attachment`（不猜一个扩展名）。
    assert_eq!(
        outbound_media_name("", "application/x-unknown"),
        "attachment"
    );
    // 已经有扩展名 ⇒ 不动它。
    assert_eq!(outbound_media_name("clip.MOV", "video/mp4"), "clip.MOV");
}

/// 三态判决：只有"判决没回来"那一类算未知，而上传阶段的失败全是确定的。
#[test]
fn the_send_outcome_has_exactly_three_states() {
    use crate::wecom::ws_sender::SenderError;
    assert_eq!(send_outcome(None), DeliveryState::Delivered);
    for sender in [
        SenderError::AckTimeout,
        SenderError::WriteAttempted {
            cause: "reset".to_owned(),
        },
        SenderError::AckAbandoned {
            cause: "budget".to_owned(),
        },
    ] {
        assert_eq!(
            send_outcome(Some(&MediaUploadError::Send(sender.clone()))),
            DeliveryState::Unknown,
            "{sender:?}"
        );
    }
    // `NotAttempted` / `ChatBusy` / 服务端拒绝都是**确定**的（上游逐字）。
    for sender in [
        SenderError::NotAttempted,
        SenderError::ChatBusy,
        SenderError::Api {
            cmd: "aibot_send_msg".to_owned(),
            code: 40001,
            message: "no".to_owned(),
        },
    ] {
        assert_eq!(
            send_outcome(Some(&MediaUploadError::Send(sender.clone()))),
            DeliveryState::DefinitelyFailed,
            "{sender:?}"
        );
    }
    // 上传阶段的一切都确定：从未产出 `media_id`。
    assert_eq!(
        send_outcome(Some(&MediaUploadError::UploadTooLarge)),
        DeliveryState::DefinitelyFailed
    );
    assert_eq!(
        send_outcome(Some(&MediaUploadError::NoMediaId)),
        DeliveryState::DefinitelyFailed
    );
    assert_eq!(DeliveryState::Unknown.as_str(), "unknown");
    assert_eq!(DeliveryState::Delivered.as_str(), "delivered");
    assert_eq!(
        DeliveryState::DefinitelyFailed.as_str(),
        "definitely_failed"
    );
}

/// 一个只有字节的存储替身。
struct ByteStore {
    key: String,
    bytes: Vec<u8>,
}

impl MediaObjectStore for ByteStore {
    fn key_from_url(&self, raw_url: &str) -> String {
        if raw_url == "known" {
            self.key.clone()
        } else {
            String::new()
        }
    }

    fn get_reader(&self, _key: &str) -> Result<Box<dyn Read + Send>, String> {
        Ok(Box::new(Cursor::new(self.bytes.clone())))
    }
}

/// 读对象：不是本部署存的对象 ⇒ `Storage`；超过**平台**帽 ⇒ `UploadTooLarge`。
#[test]
fn reading_an_object_enforces_the_platform_cap() {
    let store = ByteStore {
        key: "k".to_owned(),
        bytes: b"bytes".to_vec(),
    };
    assert_eq!(
        read_object(&store, "known").expect("read"),
        b"bytes".to_vec()
    );
    assert_eq!(
        read_object(&store, "elsewhere"),
        Err(MediaUploadError::Storage)
    );

    let oversize = ByteStore {
        key: "k".to_owned(),
        bytes: vec![0u8; MAX_MEDIA_UPLOAD_BYTES + 1],
    };
    assert_eq!(
        read_object(&oversize, "known"),
        Err(MediaUploadError::UploadTooLarge)
    );
    // 恰好在上限上**不**算超。
    let at_cap = ByteStore {
        key: "k".to_owned(),
        bytes: vec![0u8; MAX_MEDIA_UPLOAD_BYTES],
    };
    assert_eq!(
        read_object(&at_cap, "known").expect("read").len(),
        MAX_MEDIA_UPLOAD_BYTES
    );
}

// =====================================================================
// 投递
// =====================================================================

/// 一份把所有调用都数下来的汇（`&'static` 要 `Box::leak`，与 M7-17 的用例同款）。
struct Counting {
    delivered: AtomicUsize,
    dropped: AtomicUsize,
    skipped: AtomicUsize,
    attachments_dropped: AtomicUsize,
    last_reason: Mutex<String>,
}

impl Metrics for Counting {
    fn record_connect_failure(&self) {}
    fn record_auth_failure(&self) {}
    fn record_callback_queued(&self) {}
    fn record_callback_queue_blocked(&self) {}
    fn record_stream_finished(&self) {}
    fn record_stream_fell_back(&self) {}
    fn record_stream_opened(&self) {}
    fn record_outbound_delivered(&self) {
        self.delivered.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_dropped(&self, reason: &str) {
        *self.last_reason.lock().expect("lock") = reason.to_owned();
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_skipped(&self, reason: &str) {
        *self.last_reason.lock().expect("lock") = reason.to_owned();
        self.skipped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_delivered(&self) {}
    fn record_attachment_dropped(&self, reason: &str) {
        *self.last_reason.lock().expect("lock") = reason.to_owned();
        self.attachments_dropped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_delivery_shed(&self) {}
    fn record_outbound_unconfirmed(&self, _reason: &str) {}
    fn record_attachment_unconfirmed(&self, _reason: &str) {}
    fn record_relay_shed(&self, _kind: &str) {}
}

impl Default for Counting {
    fn default() -> Self {
        Self {
            delivered: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            attachments_dropped: AtomicUsize::new(0),
            last_reason: Mutex::new(String::new()),
        }
    }
}

fn counting() -> &'static Counting {
    Box::leak(Box::new(Counting::default()))
}

/// 一个按脚本回答附件的查表口。
struct FakeQueries {
    rows: Vec<AttachmentRow>,
    fails: bool,
}

#[async_trait]
impl AttachmentQueries for FakeQueries {
    async fn list_attachments_by_chat_message(
        &self,
        _chat_message_id: Id,
        _workspace_id: Id,
    ) -> Result<Vec<AttachmentRow>, String> {
        if self.fails {
            return Err("db down".to_owned());
        }
        Ok(self.rows.clone())
    }
}

/// 一个永远没有活 socket 的注册表。
struct NoSenders;

impl MediaSenderLookup for NoSenders {
    fn live_sender(&self, _installation_id: Id) -> Option<Arc<WsSender>> {
        None
    }
}

fn target() -> AttachmentTarget {
    AttachmentTarget {
        installation_id: Id(uuid::Uuid::nil()),
        chat_id: "chat-1".to_owned(),
        chat_type: 1,
        session_id: "session-1".to_owned(),
    }
}

fn delivery(
    rows: Vec<AttachmentRow>,
    fails: bool,
    metrics: &'static dyn Metrics,
    gates: AttachmentGates,
) -> WecomAttachmentDelivery {
    WecomAttachmentDelivery::new(
        Arc::new(ByteStore {
            key: "k".to_owned(),
            bytes: vec![0u8; 8],
        }),
        Arc::new(FakeQueries { rows, fails }),
        Arc::new(NoSenders),
        gates,
        Some(metrics),
    )
}

/// 查表**没找到**文件 ⇒ 记一次 `skipped`（不是丢弃），而且 pending 名额当场还回去。
#[tokio::test]
async fn an_empty_lookup_is_skipped_not_dropped() {
    let metrics = counting();
    let gates = AttachmentGates::default();
    let port = delivery(Vec::new(), false, metrics, gates.clone());
    port.deliver(
        &uuid::Uuid::from_u128(1).to_string(),
        &uuid::Uuid::from_u128(2).to_string(),
        target(),
        true,
    )
    .await;
    assert_eq!(metrics.skipped.load(Ordering::SeqCst), 1);
    assert_eq!(metrics.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(metrics.attachments_dropped.load(Ordering::SeqCst), 0);
    assert_eq!(gates.counts().1, 0, "pending 名额还回去了");
}

/// 查表**失败** ⇒ 一句"我这边没查到" + 回复自己的结局（当文件就是那条回复时）。
#[tokio::test]
async fn a_failed_lookup_tells_the_user_and_settles_the_reply() {
    let metrics = counting();
    let gates = AttachmentGates::default();
    let port = delivery(Vec::new(), true, metrics, gates.clone());
    port.deliver(
        &uuid::Uuid::from_u128(1).to_string(),
        &uuid::Uuid::from_u128(2).to_string(),
        target(),
        true,
    )
    .await;
    assert_eq!(metrics.dropped.load(Ordering::SeqCst), 1, "回复被记成丢弃");
    assert_eq!(
        metrics.last_reason.lock().expect("lock").as_str(),
        "transport_error"
    );
    assert_eq!(gates.counts().1, 0);
    // 不是那条回复时只动回复以外的计数器。
    let metrics = counting();
    let port = delivery(Vec::new(), true, metrics, AttachmentGates::default());
    port.deliver(
        &uuid::Uuid::from_u128(1).to_string(),
        &uuid::Uuid::from_u128(2).to_string(),
        target(),
        false,
    )
    .await;
    assert_eq!(metrics.dropped.load(Ordering::SeqCst), 0);
}

/// 找到了文件、**没有**活 socket ⇒ 逐文件丢弃（`no_live_connection`）+ 回复的结局，pending 还回去。
#[tokio::test]
async fn without_a_live_socket_every_file_is_dropped() {
    let metrics = counting();
    let gates = AttachmentGates::default();
    let rows = vec![
        AttachmentRow {
            id: Id(uuid::Uuid::from_u128(3)),
            url: "known".to_owned(),
            filename: "a.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 8,
        },
        AttachmentRow {
            id: Id(uuid::Uuid::from_u128(4)),
            url: "known".to_owned(),
            filename: "b.png".to_owned(),
            content_type: "image/png".to_owned(),
            size_bytes: 8,
        },
    ];
    let port = delivery(rows, false, metrics, gates.clone());
    port.deliver(
        &uuid::Uuid::from_u128(1).to_string(),
        &uuid::Uuid::from_u128(2).to_string(),
        target(),
        true,
    )
    .await;
    assert_eq!(metrics.attachments_dropped.load(Ordering::SeqCst), 2);
    assert_eq!(metrics.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        metrics.last_reason.lock().expect("lock").as_str(),
        "no_live_connection"
    );
    assert_eq!(gates.counts().1, 0, "pending 名额还回去了");
}

/// pending 上限满了 ⇒ **不查表**就能精确记账（行数是已知的）：逐文件一次 + 回复一次。
#[tokio::test]
async fn a_full_pending_gate_sheds_with_exact_accounting() {
    let metrics = counting();
    let gates = AttachmentGates::new(4, 1);
    assert!(gates.claim_pending(), "先把唯一的名额占住");
    let rows = vec![AttachmentRow {
        id: Id(uuid::Uuid::from_u128(3)),
        url: "known".to_owned(),
        filename: "a.pdf".to_owned(),
        content_type: "application/pdf".to_owned(),
        size_bytes: 8,
    }];
    let port = delivery(rows, false, metrics, gates.clone());
    port.deliver(
        &uuid::Uuid::from_u128(1).to_string(),
        &uuid::Uuid::from_u128(2).to_string(),
        target(),
        true,
    )
    .await;
    assert_eq!(metrics.attachments_dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        metrics.last_reason.lock().expect("lock").as_str(),
        "attachment_not_admitted"
    );
    assert_eq!(metrics.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(gates.counts().1, 1, "削减那条路不还别人的名额");
}

/// 不是一个 uuid 的消息 id ⇒ 什么都不做（一次没有助手消息的轮次）。
#[tokio::test]
async fn a_message_with_no_usable_id_is_ignored() {
    let metrics = counting();
    let port = delivery(Vec::new(), false, metrics, AttachmentGates::default());
    port.deliver("not-a-uuid", "also-not", target(), true).await;
    assert_eq!(metrics.skipped.load(Ordering::SeqCst), 0);
    assert_eq!(metrics.dropped.load(Ordering::SeqCst), 0);
}
