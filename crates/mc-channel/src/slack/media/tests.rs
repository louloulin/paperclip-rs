use super::*;
use crate::engine::resolvers::RecordPendingMediaObjectParams;
use crate::slack::inbound::{RawEvent, RawFile};
use async_trait::async_trait;
use base64::Engine as _;
use std::sync::Mutex;

/// key + 字节 + content type + 文件名。
type UploadRecord = (String, Vec<u8>, String, String);

#[derive(Default)]
struct FakeStorage {
    uploads: Mutex<Vec<UploadRecord>>,
}

impl MediaStorage for FakeStorage {
    fn upload(
        &self,
        key: &str,
        data: &[u8],
        content_type: &str,
        filename: &str,
    ) -> Result<String, String> {
        self.uploads.lock().expect("lock").push((
            key.to_string(),
            data.to_vec(),
            content_type.to_string(),
            filename.to_string(),
        ));
        Ok(format!("https://cdn.example/{key}"))
    }
    fn object_url(&self, key: &str) -> String {
        format!("https://cdn.example/{key}")
    }
}

#[derive(Default)]
struct FakeLedger {
    records: Mutex<Vec<RecordPendingMediaObjectParams>>,
    refuse: bool,
}

#[async_trait]
impl MediaIntentLedger for FakeLedger {
    async fn record_pending_media_object(
        &self,
        params: RecordPendingMediaObjectParams,
    ) -> EngineResult<bool> {
        self.records.lock().expect("lock").push(params);
        Ok(!self.refuse)
    }
}

/// 只认 Slack 域名的取回替身；`files` 是 id → 字节（URL 里含 id）。
#[derive(Default)]
struct FakeFetcher {
    files: std::collections::HashMap<String, (Vec<u8>, String)>,
    last_auth: Mutex<String>,
}

impl MediaFetcher for FakeFetcher {
    fn fetch(&self, request: FetchRequest<'_>) -> Result<Fetched, MediaError> {
        let parsed = Url::parse(request.url).map_err(|_| MediaError::InvalidUrl)?;
        validate_download_url(&parsed, is_slack_file_host)?;
        *self.last_auth.lock().expect("lock") = request.bot_token.to_string();
        let id = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .unwrap_or_default()
            .to_string();
        self.files
            .get(&id)
            .map(|(data, content_type)| Fetched {
                data: data.clone(),
                content_type: content_type.clone(),
            })
            .ok_or(MediaError::Http { status: 404 })
    }
}

fn installation() -> ResolvedInstallation {
    let mut installation = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        crate::slack::inbound::TYPE_SLACK,
        true,
    );
    // 平台值：安装行（含配置 blob）——媒体解析要读它拿 bot token。
    installation.platform = Some(Arc::new(crate::slack::resolvers::InstallationRow {
        id: installation.id,
        workspace_id: installation.workspace_id,
        agent_id: installation.agent_id,
        installer_user_id: installation.installer_user_id,
        status: "active".to_string(),
        config: serde_json::json!({
            "app_id": "A1",
            "bot_token_encrypted": base64::engine::general_purpose::STANDARD.encode("xoxb-test-token"),
        }),
    }));
    installation
}

fn message_with(files: Vec<RawFile>) -> InboundMessage {
    let mut message = InboundMessage {
        event_id: "1.1".to_string(),
        message_id: "1.1".to_string(),
        source: mc_core::channel::message::Source {
            channel_type: crate::slack::inbound::TYPE_SLACK,
            chat_id: "D1".to_string(),
            chat_type: mc_core::channel::message::ChatType::P2p,
            sender_id: "U1".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "here".to_string(),
        command_text: "here".to_string(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::Value::Null,
    };
    message.raw = serde_json::to_value(RawEvent {
        team_id: "T1".to_string(),
        api_app_id: "A1".to_string(),
        event_type: "message".to_string(),
        files,
        ..RawEvent::default()
    })
    .expect("raw");
    message
}

fn resolver(
    storage: Arc<FakeStorage>,
    ledger: Arc<FakeLedger>,
    fetcher: Arc<FakeFetcher>,
) -> SlackMediaResolver {
    SlackMediaResolver::new(Decrypter::plaintext(), storage, ledger).with_fetcher(fetcher)
}

/// 上游 `TestIsSlackFileHost` / `TestIsFetchableSlackFileURL`。
#[test]
fn host_and_url_predicates() {
    for (host, want) in [
        ("files.slack.com", true),
        ("slack.com", true),
        ("FILES.SLACK.COM", true),
        ("sub.files.slack.com", true),
        ("evil.com", false),
        ("slack.com.evil.com", false),
        ("notslack.com", false),
        ("files.slack.com.br", false),
        ("fakeslack.com", false),
    ] {
        assert_eq!(is_slack_file_host(host), want, "{host}");
    }
    for (url, want) in [
        ("https://files.slack.com/files-pri/T1-F1/report.pdf", true),
        ("https://slack.com/files-pri/T1-F1/report.pdf", true),
        ("https://docs.google.com/document/d/abc/edit", false),
        ("https://www.dropbox.com/s/abc/report.pdf", false),
        ("http://files.slack.com/files-pri/T1-F1/x.pdf", false),
        ("https://user:pass@files.slack.com/x.pdf", false),
        ("https://files.slack.com.evil.com/x.pdf", false),
        ("", false),
        ("not a url", false),
    ] {
        assert_eq!(is_fetchable_slack_file_url(url), want, "{url}");
    }
}

/// `has_media` 是纯解码（无文件 ⇒ false）。
#[test]
fn has_media_only_for_fetchable_files() {
    assert!(!has_media(&message_with(Vec::new())));
    assert!(has_media(&message_with(vec![RawFile {
        id: "F1".to_string(),
        download_url: "https://files.slack.com/f".to_string(),
        ..RawFile::default()
    }])));
}

/// 上游 `TestSlackMediaResolver_HappyPath`：一个文件走完 URL → 意图行 → 上传 → `MediaRef`。
#[test]
fn happy_path_records_intent_before_upload_and_returns_a_ref() {
    let storage = Arc::new(FakeStorage::default());
    let ledger = Arc::new(FakeLedger::default());
    let mut fetcher = FakeFetcher::default();
    fetcher.files.insert(
        "f1".to_string(),
        (
            b"%PDF-1.4 fake body".to_vec(),
            "application/pdf".to_string(),
        ),
    );
    let fetcher = Arc::new(fetcher);
    let resolver = resolver(
        Arc::clone(&storage),
        Arc::clone(&ledger),
        Arc::clone(&fetcher),
    );
    let message = message_with(vec![RawFile {
        id: "F1".to_string(),
        name: "report.pdf".to_string(),
        mimetype: "application/pdf".to_string(),
        size: 18,
        download_url: "https://files.slack.com/f1".to_string(),
    }]);
    let out = resolver.resolve_media(
        &installation(),
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert_eq!(out.media_refs.len(), 1);
    let reference = &out.media_refs[0];
    assert_eq!(reference.message_kind, MessageKind::File);
    assert_eq!(reference.filename, "report.pdf");
    assert_eq!(reference.mime_type, "application/pdf");
    assert_eq!(reference.size_bytes, 18);
    assert!(reference.storage_key.starts_with("workspaces/"));
    assert!(reference.storage_key.contains("/slack/"));
    assert_eq!(
        reference.storage_url,
        format!("https://cdn.example/{}", reference.storage_key)
    );
    let records = ledger.records.lock().expect("lock");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].storage_key, reference.storage_key);
    assert_eq!(records[0].storage_url, reference.storage_url);
    drop(records);
    assert_eq!(storage.uploads.lock().expect("lock").len(), 1, "上传过一次");
    assert_eq!(
        fetcher.last_auth.lock().expect("lock").as_str(),
        "xoxb-test-token",
        "用的是本安装解密出来的 bot token"
    );
}

/// 非 Slack 主机 ⇒ 不取、不传（bot token 不出发）。
#[test]
fn blocked_host_never_reaches_the_network() {
    let storage = Arc::new(FakeStorage::default());
    let ledger = Arc::new(FakeLedger::default());
    let fetcher = Arc::new(FakeFetcher::default());
    let resolver = resolver(
        Arc::clone(&storage),
        Arc::clone(&ledger),
        Arc::clone(&fetcher),
    );
    let message = message_with(vec![RawFile {
        id: "F1".to_string(),
        name: "x.bin".to_string(),
        download_url: "https://evil.example/f1".to_string(),
        ..RawFile::default()
    }]);
    let out = resolver.resolve_media(
        &installation(),
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert!(out.media_refs.is_empty());
    assert!(storage.uploads.lock().expect("lock").is_empty());
    assert_eq!(fetcher.last_auth.lock().expect("lock").as_str(), "");
}

/// 上游 `TestSlackMediaResolver_HTMLResponseRequiresExplicitDeclaration`。
#[test]
fn html_login_page_is_not_stored_as_the_file() {
    let html = b"<!DOCTYPE html><html>Sign in to Slack</html>".to_vec();
    for (declared, want) in [("application/pdf", false), ("", false), ("TEXT/HTML", true)] {
        let storage = Arc::new(FakeStorage::default());
        let ledger = Arc::new(FakeLedger::default());
        let mut fetcher = FakeFetcher::default();
        fetcher.files.insert(
            "f1".to_string(),
            (html.clone(), "Text/HTML; charset=utf-8".to_string()),
        );
        let resolver = resolver(Arc::clone(&storage), Arc::clone(&ledger), Arc::new(fetcher));
        let message = message_with(vec![RawFile {
            id: "F1".to_string(),
            name: "report.html".to_string(),
            mimetype: declared.to_string(),
            download_url: "https://files.slack.com/f1".to_string(),
            ..RawFile::default()
        }]);
        let out = resolver.resolve_media(
            &installation(),
            &ResolvedIdentity { user_id: Id::new() },
            Id::new(),
            Some(Id::new()),
            &message,
        );
        assert_eq!(
            out.media_refs.len(),
            usize::from(want),
            "declared={declared}"
        );
        if want {
            assert_eq!(out.media_refs[0].mime_type, "text/html");
        } else {
            assert!(storage.uploads.lock().expect("lock").is_empty());
        }
    }
}

/// 对账器已接管该 key ⇒ 不上传、不复活。
#[test]
fn reconciler_owned_key_is_skipped() {
    let storage = Arc::new(FakeStorage::default());
    let ledger = Arc::new(FakeLedger {
        records: Mutex::new(Vec::new()),
        refuse: true,
    });
    let mut fetcher = FakeFetcher::default();
    fetcher.files.insert(
        "f1".to_string(),
        (b"data".to_vec(), "application/octet-stream".to_string()),
    );
    let resolver = resolver(Arc::clone(&storage), Arc::clone(&ledger), Arc::new(fetcher));
    let message = message_with(vec![RawFile {
        id: "F1".to_string(),
        name: "x.bin".to_string(),
        download_url: "https://files.slack.com/f1".to_string(),
        ..RawFile::default()
    }]);
    let out = resolver.resolve_media(
        &installation(),
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert!(out.media_refs.is_empty());
    assert!(storage.uploads.lock().expect("lock").is_empty());
}

/// 一个文件失败**不**阻断其余（上游 `TestSlackMediaResolver_OneFailureDoesNotStopTheRest`）。
#[test]
fn one_failure_does_not_stop_the_rest() {
    let storage = Arc::new(FakeStorage::default());
    let ledger = Arc::new(FakeLedger::default());
    let mut fetcher = FakeFetcher::default();
    fetcher.files.insert(
        "f2".to_string(),
        (b"second file".to_vec(), "text/plain".to_string()),
    );
    let resolver = resolver(Arc::clone(&storage), Arc::clone(&ledger), Arc::new(fetcher));
    let message = message_with(vec![
        RawFile {
            id: "F1".to_string(),
            name: "missing.bin".to_string(),
            download_url: "https://files.slack.com/f1".to_string(),
            ..RawFile::default()
        },
        RawFile {
            id: "F2".to_string(),
            name: "there.txt".to_string(),
            mimetype: "text/plain".to_string(),
            download_url: "https://files.slack.com/f2".to_string(),
            ..RawFile::default()
        },
    ]);
    let out = resolver.resolve_media(
        &installation(),
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert_eq!(out.media_refs.len(), 1);
    assert_eq!(out.media_refs[0].filename, "there.txt");
}

/// 没有 `chat_message` 行 / 没有平台行 ⇒ 跳过（不猜 key、不丢意图行）。
#[test]
fn missing_chat_message_row_skips_media() {
    let storage = Arc::new(FakeStorage::default());
    let ledger = Arc::new(FakeLedger::default());
    let resolver = resolver(
        Arc::clone(&storage),
        Arc::clone(&ledger),
        Arc::new(FakeFetcher::default()),
    );
    let message = message_with(vec![RawFile {
        id: "F1".to_string(),
        download_url: "https://files.slack.com/f1".to_string(),
        ..RawFile::default()
    }]);
    let out = resolver.resolve_media(
        &installation(),
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        None,
        &message,
    );
    assert!(out.media_refs.is_empty());
    assert!(ledger.records.lock().expect("lock").is_empty());

    let platformless = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        crate::slack::inbound::TYPE_SLACK,
        true,
    );
    let out = resolver.resolve_media(
        &platformless,
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert!(out.media_refs.is_empty());
}

/// 声明就超限 ⇒ 在落意图行与传输**之前**拒掉。
#[test]
fn declared_size_limit_is_refused_before_any_intent_row() {
    let storage = Arc::new(FakeStorage::default());
    let ledger = Arc::new(FakeLedger::default());
    let resolver = resolver(
        Arc::clone(&storage),
        Arc::clone(&ledger),
        Arc::new(FakeFetcher::default()),
    );
    let message = message_with(vec![RawFile {
        id: "F1".to_string(),
        size: i64::try_from(MAX_INBOUND_FILE_BYTES + 1).unwrap_or(i64::MAX),
        download_url: "https://files.slack.com/f1".to_string(),
        ..RawFile::default()
    }]);
    let out = resolver.resolve_media(
        &installation(),
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert!(out.media_refs.is_empty());
    assert!(ledger.records.lock().expect("lock").is_empty());
}

/// 超过 `MAX_FILES_PER_MESSAGE` 的额外文件被截断（前十个仍处理）。
#[test]
fn file_count_is_capped() {
    let storage = Arc::new(FakeStorage::default());
    let ledger = Arc::new(FakeLedger::default());
    let mut fetcher = FakeFetcher::default();
    for index in 0..12 {
        fetcher.files.insert(
            format!("f{index}"),
            (b"data".to_vec(), "application/octet-stream".to_string()),
        );
    }
    let resolver = resolver(Arc::clone(&storage), Arc::clone(&ledger), Arc::new(fetcher));
    let files: Vec<RawFile> = (0..12)
        .map(|index| RawFile {
            id: format!("F{index}"),
            download_url: format!("https://files.slack.com/f{index}"),
            ..RawFile::default()
        })
        .collect();
    let out = resolver.resolve_media(
        &installation(),
        &ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message_with(files),
    );
    assert_eq!(out.media_refs.len(), MAX_FILES_PER_MESSAGE);
}

/// 上游 `TestSlackFileName_Fallbacks`（路径穿越 / 全点 / 空名）。
#[test]
fn file_name_fallbacks() {
    let traversal = RawFile {
        id: "F123".to_string(),
        name: "../../etc/passwd".to_string(),
        ..RawFile::default()
    };
    assert_eq!(slack_file_name(&traversal, 0, "text/plain"), "passwd");
    let dots = RawFile {
        id: "F123".to_string(),
        name: "..".to_string(),
        ..RawFile::default()
    };
    assert_eq!(
        slack_file_name(&dots, 1, "image/png"),
        "slack-file-F123-2.png"
    );
    let empty = RawFile {
        id: "F123".to_string(),
        ..RawFile::default()
    };
    assert_eq!(
        slack_file_name(&empty, 0, "application/pdf"),
        "slack-file-F123-1.pdf"
    );
    assert_eq!(safe_media_segment("  "), "unknown");
    assert_eq!(safe_media_segment("a/b c"), "a_b_c");
}

/// 媒体种类与扩展名映射。
#[test]
fn kind_and_extension_mapping() {
    assert_eq!(slack_media_kind("image/png"), MessageKind::Image);
    assert_eq!(slack_media_kind("video/mp4"), MessageKind::Video);
    assert_eq!(slack_media_kind("audio/ogg"), MessageKind::Audio);
    assert_eq!(slack_media_kind("application/pdf"), MessageKind::File);
    assert_eq!(slack_media_kind(""), MessageKind::File);
    assert_eq!(media_extension("image/jpeg"), ".jpg");
    assert_eq!(media_extension("application/zip"), "");
}

/// content type 的判定顺序：声明优先、其次响应头、最后嗅探。
#[test]
fn content_type_resolution_order() {
    let file = RawFile {
        mimetype: "APPLICATION/PDF".to_string(),
        ..RawFile::default()
    };
    assert_eq!(
        slack_file_content_type(&file, "text/plain", b"x").expect("declared 优先"),
        "application/pdf"
    );
    let undeclared = RawFile::default();
    assert_eq!(
        slack_file_content_type(&undeclared, "image/png; charset=x", b"x").expect("响应头"),
        "image/png"
    );
    assert_eq!(
        slack_file_content_type(&undeclared, "", b"%PDF-1.4").expect("嗅探"),
        "application/pdf"
    );
    assert_eq!(
        slack_file_content_type(&undeclared, "", &[0x89, b'P', b'N', b'G', 0x0d]).expect("png"),
        "image/png"
    );
    assert_eq!(
        slack_file_content_type(&undeclared, "", b"\x00\x01\x02").expect("未知"),
        "application/octet-stream"
    );
    // 没声明是 HTML 却拿到 HTML ⇒ 失败（登录页不是文件）。
    assert!(slack_file_content_type(&undeclared, "text/html", b"<html>").is_err());
}

/// 对象 key 由 chat message id + file id + 序号派生（同一条平台消息两次摄入 ⇒ 不同 key）。
#[test]
fn object_key_is_keyed_by_chat_message() {
    let installation = installation();
    let file = RawFile {
        id: "F1".to_string(),
        ..RawFile::default()
    };
    let chat_message_id = Id::new();
    let first = slack_media_object_key(&installation, chat_message_id, &file, 0);
    let second = slack_media_object_key(&installation, Id::new(), &file, 0);
    assert_ne!(first, second, "不同 chat 消息不得共用 key");
    assert_eq!(
        first,
        slack_media_object_key(&installation, chat_message_id, &file, 0),
        "同一输入 ⇒ 同一 key（纯函数）"
    );
    assert_ne!(
        first,
        slack_media_object_key(&installation, chat_message_id, &file, 1),
        "同一文件的不同序号也不共用 key"
    );
    assert!(first.starts_with("workspaces/"));
    assert!(first.contains("/slack/"));
}
