//! `media_ingest` 的用例：解码接缝的三个边界、对象 key 的派生、描述与命名的取舍、
//! 失败分类、通知的措辞与去重，以及**一次完整的摄入**（下载 → 解密 → 上传 → `MediaRef`）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::engine::resolvers::{EngineError, EngineResult, RecordPendingMediaObjectParams};
use crate::wecom::media_guard::{
    addr_policy, new_media_http_client, unmap, AddrPolicy, MediaGuard,
};

mod support;

use support::{spawn_media_stub, MediaScript};

// =====================================================================
// 解码接缝（交接 H1）
// =====================================================================

/// 上游 `ws_frame.go` 的那几个 JSON tag 逐字：解不出来就**失败关闭**。
#[test]
fn the_envelope_decodes_the_documented_tags_and_fails_closed() {
    let raw = json!({
        "bot_id": "bot-1",
        "msg_id": "msg-1",
        "msg_type": "image",
        "chat_type": "group",
        "chat_id": "chat-1",
        "sender_user_id": "user-1",
        "content": "[图片]",
        "media": [
            { "kind": "image", "url": "https://cos.example.cn/a", "aeskey": "key-a" },
            { "kind": "file", "url": "https://cos.example.cn/b", "aeskey": "key-b" }
        ]
    });
    let inbound = decode_wecom_inbound(&raw).expect("decodes");
    assert_eq!(inbound.bot_id, "bot-1");
    assert_eq!(inbound.msg_id, "msg-1");
    assert_eq!(inbound.chat_type, "group");
    assert_eq!(inbound.chat_id, "chat-1");
    assert_eq!(inbound.sender_user_id, "user-1");
    assert_eq!(inbound.media.len(), 2);
    assert_eq!(inbound.media[0].kind, MessageKind::Image);
    assert_eq!(inbound.media[0].url, "https://cos.example.cn/a");
    assert_eq!(inbound.media[0].aes_key, "key-a");
    assert_eq!(inbound.media[1].kind, MessageKind::File);

    // 不是对象 ⇒ 失败关闭。
    assert_eq!(decode_wecom_inbound(&json!("text")), None);
    assert_eq!(decode_wecom_inbound(&json!({ "media": "nope" })), None);
    // 少一根字符串 ⇒ 失败关闭（不猜那把密钥、也不猜那个地址）。
    assert_eq!(
        decode_wecom_inbound(&json!({ "media": [{ "kind": "image", "url": "u" }] })),
        None
    );
    assert_eq!(
        decode_wecom_inbound(&json!({ "media": [{ "kind": "image", "aeskey": "k" }] })),
        None
    );
    // 认不出的 kind ⇒ `Unknown`（**不**拒整条消息：附件仍然值得取回来）。
    let inbound = decode_wecom_inbound(&json!({
        "media": [{ "kind": "sticker", "url": "u", "aeskey": "k" }]
    }))
    .expect("decodes");
    assert_eq!(inbound.media[0].kind, MessageKind::Unknown);
    // 没有 media 键 ⇒ 空表，不是失败。
    let inbound = decode_wecom_inbound(&json!({ "bot_id": "b" })).expect("decodes");
    assert!(inbound.media.is_empty());
}

// =====================================================================
// 命名与描述
// =====================================================================

/// 对象 key 派生自**聊天**消息与附件位置（两次摄入同一平台消息时不会撞行）。
#[test]
fn the_object_key_is_derived_from_the_chat_message_and_the_position() {
    let installation = ResolvedInstallation {
        id: Id(uuid::Uuid::from_u128(1)),
        workspace_id: Id(uuid::Uuid::from_u128(2)),
        agent_id: Id(uuid::Uuid::from_u128(3)),
        installer_user_id: Id(uuid::Uuid::from_u128(4)),
        active: true,
        kind: mc_core::channel::ChannelKind::WeCom,
        platform: None,
    };
    let chat_message_id = Id(uuid::Uuid::from_u128(9));
    let first = media_object_key(
        &installation,
        chat_message_id,
        "msg-1",
        0,
        MessageKind::Image,
    );
    let second = media_object_key(
        &installation,
        chat_message_id,
        "msg-1",
        1,
        MessageKind::Image,
    );
    assert_ne!(first, second, "位置要进 key");
    assert_eq!(
        first,
        media_object_key(
            &installation,
            chat_message_id,
            "msg-1",
            0,
            MessageKind::Image
        ),
        "同一个输入 ⇒ 同一个 key"
    );
    assert!(first.starts_with("workspaces/00000000-0000-0000-0000-000000000002/wecom/"));
    assert!(first.contains("00000000-0000-0000-0000-000000000001/"));
    // 类型也进 key（同一条消息里的图片与文件不会撞）。
    assert_ne!(
        first,
        media_object_key(
            &installation,
            chat_message_id,
            "msg-1",
            0,
            MessageKind::File
        )
    );
    // 另一条平台消息 ⇒ 另一个 key。
    assert_ne!(
        first,
        media_object_key(
            &installation,
            chat_message_id,
            "msg-2",
            0,
            MessageKind::Image
        )
    );
}

/// 名字与类型：扩展名优先、嗅字节兜底、都没有就给一个唯一的兜底名。
#[test]
fn describing_a_file_prefers_the_extension_then_sniffs_bytes() {
    let inbound = WecomInbound {
        msg_id: "msg-1".to_owned(),
        ..WecomInbound::default()
    };
    let media = InboundMedia {
        kind: MessageKind::File,
        url: "u".to_owned(),
        aes_key: "k".to_owned(),
    };
    // 扩展名赢（`.docx` 真的是 zip 容器，所以扩展名是更好的信号）。
    let (name, content_type) = describe_media(&inbound, 0, &media, "季报.docx", &[0x50, 0x4b]);
    assert_eq!(name, "季报.docx");
    assert_eq!(
        content_type,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    );
    // 没有名字 ⇒ 嗅字节。
    let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (name, content_type) = describe_media(&inbound, 1, &media, "", &png);
    assert_eq!(content_type, "image/png");
    assert_eq!(name, "wecom-file-msg-1-1.png");
    // 都没有 ⇒ 兜底的 octet-stream，而名字里的位置保证同一消息内唯一。
    let (first, _) = describe_media(&inbound, 5, &media, "", b"whatever");
    let (second, _) = describe_media(&inbound, 6, &media, "", b"whatever");
    assert_ne!(first, second);
    // 图片走前缀。
    let image = InboundMedia {
        kind: MessageKind::Image,
        ..media.clone()
    };
    let (name, content_type) = describe_media(&inbound, 0, &image, "", b"nope");
    assert_eq!(content_type, "application/octet-stream");
    assert!(name.starts_with("wecom-image-"), "{name}");
    // 路径穿越的名字被压成一个段（走的是 `clean_media_filename`）。
    let (name, _) = describe_media(&inbound, 0, &media, "../etc/passwd", b"");
    assert_eq!(name, "passwd");
}

/// 两张表：扩展名 → 内容类型、内容类型 → 扩展名；表外一律空串（不猜）。
#[test]
fn the_two_hand_written_tables_agree_and_refuse_the_unknown() {
    assert_eq!(
        content_type_for_extension("xlsx"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    );
    assert_eq!(media_extension("application/pdf"), ".pdf");
    assert_eq!(media_extension("image/jpeg; charset=binary"), ".jpg");
    assert_eq!(media_extension("application/x-not-a-real-type"), "");
    assert_eq!(content_type_for_extension("nope"), "");
    assert_eq!(base_content_type("TEXT/CSV; charset=utf-8"), "text/csv");
    assert_eq!(base_content_type("  image/png  "), "image/png");
    assert_eq!(safe_media_segment(""), "unknown");
    assert_eq!(safe_media_segment("___"), "unknown");
    assert_eq!(safe_media_segment(" msg-1/x "), "msg-1_x");
    // 嗅探：认得出来的几种 + 认不出来 ⇒ 空串。
    assert_eq!(sniff_content_type(b"%PDF-1.7"), "application/pdf");
    assert_eq!(sniff_content_type(b"#!AMR\n"), "audio/amr");
    assert_eq!(sniff_content_type(b"PK\x03\x04rest"), "application/zip");
    assert_eq!(sniff_content_type(b"\x00\x00\x00\x18ftypmp42"), "video/mp4");
    assert_eq!(sniff_content_type(b"just some text"), "");
}

/// 失败分类来自**类型**，而每个类别只留一条。
#[test]
fn failures_are_classified_from_types_and_deduplicated() {
    assert_eq!(
        classify_media_failure(&MediaIngestError::Download(MediaDownloadError::TooLarge)),
        MediaFailure::TooLarge
    );
    assert_eq!(
        classify_media_failure(&MediaIngestError::Download(MediaDownloadError::Guard(
            crate::wecom::media_guard::MediaGuardError::BlockedAddress
        ))),
        MediaFailure::Blocked
    );
    for error in [
        MediaIngestError::Download(MediaDownloadError::Transport),
        MediaIngestError::Storage,
        MediaIngestError::Ledger,
    ] {
        assert_eq!(classify_media_failure(&error), MediaFailure::Unreadable);
    }
    let mut list = Vec::new();
    append_failure(&mut list, MediaFailure::Unreadable);
    append_failure(&mut list, MediaFailure::Unreadable);
    append_failure(&mut list, MediaFailure::TooLarge);
    assert_eq!(list, vec![MediaFailure::Unreadable, MediaFailure::TooLarge]);
}

// =====================================================================
// 一次完整的摄入
// =====================================================================

/// 只用回环的地址策略（被测试的是摄入回路，不是脚手架的地址）。
fn loopback_policy() -> AddrPolicy {
    addr_policy(|address| match unmap(address) {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    })
}

/// 一个记下意图行的账本。
#[derive(Default)]
struct RecordingLedger {
    recorded: Mutex<Vec<RecordPendingMediaObjectParams>>,
    /// 下一次问它要 key 的时候回答"已经归对账器了"。
    owns_key: bool,
}

#[async_trait]
impl MediaIntentLedger for RecordingLedger {
    async fn record_pending_media_object(
        &self,
        params: RecordPendingMediaObjectParams,
    ) -> EngineResult<bool> {
        self.recorded.lock().expect("lock").push(params);
        if self.owns_key {
            return Err(EngineError::infra("ledger unavailable"));
        }
        Ok(true)
    }
}

/// 一个记下上传的对象存储。
#[derive(Default)]
struct RecordingStorage {
    uploaded: Mutex<Vec<(String, String, String, usize)>>,
}

impl MediaStorage for RecordingStorage {
    fn upload(
        &self,
        key: &str,
        data: &[u8],
        content_type: &str,
        filename: &str,
    ) -> Result<String, String> {
        self.uploaded.lock().expect("lock").push((
            key.to_owned(),
            content_type.to_owned(),
            filename.to_owned(),
            data.len(),
        ));
        Ok(format!("https://objects.example/{key}"))
    }

    fn object_url(&self, key: &str) -> String {
        format!("https://objects.example/{key}")
    }
}

/// 一个记下通知的投递口。
#[derive(Default)]
struct RecordingNotifier {
    notices: Mutex<Vec<(Id, String, i32, String)>>,
}

impl MediaNotifier for RecordingNotifier {
    fn notify(&self, installation_id: Id, chat_id: &str, chat_type: i32, text: &str) {
        self.notices.lock().expect("lock").push((
            installation_id,
            chat_id.to_owned(),
            chat_type,
            text.to_owned(),
        ));
    }
}

fn installation() -> ResolvedInstallation {
    ResolvedInstallation {
        id: Id(uuid::Uuid::from_u128(0x11)),
        workspace_id: Id(uuid::Uuid::from_u128(0x22)),
        agent_id: Id(uuid::Uuid::from_u128(0x33)),
        installer_user_id: Id(uuid::Uuid::from_u128(0x44)),
        active: true,
        kind: mc_core::channel::ChannelKind::WeCom,
        platform: None,
    }
}

fn inbound_message(raw: Value) -> InboundMessage {
    InboundMessage {
        event_id: "evt-1".to_owned(),
        message_id: "msg-1".to_owned(),
        source: mc_core::channel::message::Source {
            channel_type: mc_core::channel::ChannelKind::WeCom,
            chat_id: "chat-1".to_owned(),
            chat_type: mc_core::channel::message::ChatType::P2p,
            sender_id: "user-1".to_owned(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Image,
        text: "[图片]".to_owned(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: false,
        force_fresh: false,
        skip_agent_run: false,
        raw,
    }
}

/// 端到端：两个附件，一个取回来、一个被服务端拒 ⇒ 一条 `MediaRef` + 一次通知，
/// 而**意图行在每一次上传之前**就落了。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_ingest_downloads_decrypts_uploads_and_speaks_once() {
    // 一个用真密钥加密的载荷，好让解密真的跑一遍。
    let key = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let plaintext = b"an actual attachment body".to_vec();
    let ciphertext = {
        use base64::Engine as _;
        let mut padded = plaintext.clone();
        let pad = 32 - padded.len() % 32;
        padded.extend(std::iter::repeat_n(
            u8::try_from(pad).expect("pad ≤ 32"),
            pad,
        ));
        // 与 `media_crypt` 的用例同一把 `MediaAesKey`（IV = 密钥前 16 字节）。
        let aes_key = crate::wecom::media_crypt::MediaAesKey::from_bytes(*key);
        let _ = base64::engine::general_purpose::STANDARD.encode(key);
        aes_key.encrypt(&plaintext)
    };
    let port = spawn_media_stub(vec![
        MediaScript::ok(&ciphertext).header(
            "content-disposition",
            r"attachment; filename*=UTF-8''%E5%AD%A3%E6%8A%A5.pdf",
        ),
        MediaScript::status(403),
    ])
    .await;

    let key_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(key)
    };
    let raw = json!({
        "bot_id": "bot-1",
        "msg_id": "msg-1",
        "chat_type": "single",
        "chat_id": "chat-1",
        "sender_user_id": "user-1",
        "media": [
            { "kind": "file", "url": format!("http://localhost:{port}/good"), "aeskey": key_b64 },
            { "kind": "image", "url": format!("http://localhost:{port}/bad"), "aeskey": key_b64 }
        ]
    });

    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::default());
    let notifier = Arc::new(RecordingNotifier::default());
    let client =
        new_media_http_client(MediaGuard::new().with_policy(loopback_policy())).expect("client");
    let resolver = WecomMediaResolver::new(storage.clone(), ledger.clone(), Some(notifier.clone()))
        .expect("resolver")
        .with_client(client);

    let message = inbound_message(raw);
    let identity = ResolvedIdentity {
        user_id: Id(uuid::Uuid::from_u128(0x55)),
    };
    assert!(MediaResolver::has_media(&resolver, &message));

    let after = MediaResolver::resolve_media(
        &resolver,
        &installation(),
        &identity,
        Id(uuid::Uuid::from_u128(0x66)),
        Some(Id(uuid::Uuid::from_u128(0x77))),
        &message,
    );

    // 一个附件落地：名字来自 `Content-Disposition`、类型来自扩展名、字节数是**解密后**的。
    assert_eq!(after.media_refs.len(), 1, "{:?}", after.media_refs);
    let reference = &after.media_refs[0];
    assert_eq!(reference.filename, "季报.pdf");
    assert_eq!(reference.mime_type, "application/pdf");
    assert_eq!(
        reference.size_bytes,
        i64::try_from(plaintext.len()).unwrap()
    );
    assert!(reference.storage_key.contains("/wecom/"));
    assert!(reference.inline_placeholder.is_empty());
    // 上传拿到的是**明文**。
    let uploaded = storage.uploaded.lock().expect("lock").clone();
    assert_eq!(uploaded.len(), 1);
    assert_eq!(uploaded[0].3, plaintext.len());

    // 意图行**两条**，而且都在上传之前（第一条的 key 就是上传用的那个 key）。
    let recorded = ledger.recorded.lock().expect("lock").clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].storage_key, uploaded[0].0);
    assert_eq!(
        recorded[0].chat_message_id,
        Some(Id(uuid::Uuid::from_u128(0x77)))
    );
    assert_eq!(recorded[1].storage_key != recorded[0].storage_key, true);

    // 通知：一条，措辞是"读不出来"那一句，走的是单聊。
    let notices = notifier.notices.lock().expect("lock").clone();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].2, CHAT_TYPE_SINGLE_INT);
    assert_eq!(notices[0].3, MEDIA_UNREADABLE_NOTICE);
    assert_eq!(notices[0].1, "chat-1");
}

/// 账本失败 ⇒ **没有**上传（失败关闭的方向），而且消息原样返回。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ledger_failure_stops_the_upload() {
    let port = spawn_media_stub(vec![MediaScript::ok(b"whatever")]).await;
    let key_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode([b'a'; 32])
    };
    let raw = json!({
        "msg_id": "msg-1",
        "chat_id": "chat-1",
        "chat_type": "single",
        "media": [{ "kind": "file", "url": format!("http://localhost:{port}/good"), "aeskey": key_b64 }]
    });
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger {
        recorded: Mutex::new(Vec::new()),
        owns_key: true,
    });
    let notifier = Arc::new(RecordingNotifier::default());
    let resolver = WecomMediaResolver::new(storage.clone(), ledger, Some(notifier.clone()))
        .expect("resolver")
        .with_client(
            new_media_http_client(MediaGuard::new().with_policy(loopback_policy()))
                .expect("client"),
        );
    let message = inbound_message(raw);
    let after = MediaResolver::resolve_media(
        &resolver,
        &installation(),
        &ResolvedIdentity {
            user_id: Id(uuid::Uuid::from_u128(0x55)),
        },
        Id(uuid::Uuid::from_u128(0x66)),
        Some(Id(uuid::Uuid::from_u128(0x77))),
        &message,
    );
    assert!(after.media_refs.is_empty());
    assert!(storage.uploaded.lock().expect("lock").is_empty());
    assert_eq!(notifier.notices.lock().expect("lock").len(), 1);
}

/// 没有聊天消息行 ⇒ 整个附件面跳过（上游那个 `chatMessageID` 检查）。
#[tokio::test]
async fn without_a_chat_message_row_nothing_is_attempted() {
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::default());
    let resolver =
        WecomMediaResolver::new(storage.clone(), ledger.clone(), None).expect("resolver");
    let message = inbound_message(json!({
        "msg_id": "msg-1",
        "media": [{ "kind": "file", "url": "http://localhost:1/x", "aeskey": "k" }]
    }));
    let after = MediaResolver::resolve_media(
        &resolver,
        &installation(),
        &ResolvedIdentity {
            user_id: Id(uuid::Uuid::from_u128(0x55)),
        },
        Id(uuid::Uuid::from_u128(0x66)),
        None,
        &message,
    );
    assert!(after.media_refs.is_empty());
    assert!(ledger.recorded.lock().expect("lock").is_empty());
}
