//! `dingtalk::media` 的用例（写者 M7-8）。
//!
//! 四段：
//!
//! 1. **公网地址判据**：IPv4 / IPv6 / NAT64 / IPv4-mapped 四条路径的真值表（SSRF 的核心）；
//! 2. **URL 校验 / 嗅探 / 对象 key**：纯函数；
//! 3. **解析器**：注入替身取回器 + 记账式存储 + 记账式账本，钉住"意图行先落、
//!    每张图互不牵连、超限整条跳过、账本说不是我们的就不上传"；
//! 4. **真 `reqwest` 的守卫**：`guarded_client` 的解析器把回环 / 内网目标挡在连接之前
//!    （成功路径按定义连不上本地替身 —— 那正是这条守卫的语义 ⇒ 成功路径由第 3 段的
//!    端口替身覆盖；见 `docs/32` §22 的 D5）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::id::Id;
use serde_json::json;

use super::{
    image_extension, is_public_download_address, media_object_key, same_download_origin,
    sniff_image_content_type, unmap, validate_download_url, DingTalkMediaResolver, Fetched,
    MediaError, MediaFetcher, MediaStorage, MAX_IMAGES_PER_MESSAGE, NON_PUBLIC_PREFIXES,
};
use crate::dingtalk::inbound::DingtalkMediaResource;
use crate::dingtalk::outbound::Credentials;
use crate::dingtalk::resolvers::InstallationRow;
use crate::dingtalk::Decrypter;
use crate::engine::resolvers::{
    EngineResult, MediaIntentLedger, MediaResolver as _, RecordPendingMediaObjectParams,
    ResolvedInstallation,
};

// =====================================================================
// 公网地址判据
// =====================================================================

/// 公网 IPv4 放行；上游点名的那 8 段与公网单播所需的私网 / 回环段全部拒。
#[test]
fn the_ipv4_deny_list_matches_upstream() {
    for allowed in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "223.255.255.254"] {
        assert!(
            is_public_download_address(allowed.parse().expect("IPv4")),
            "{allowed} 是公网地址，应当放行"
        );
    }
    for blocked in [
        "0.0.0.0",
        "0.1.2.3",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.1.1",
        "172.16.0.1",
        "172.31.255.255",
        "192.0.0.1",
        "192.0.2.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "239.255.255.255",
        "240.0.0.1",
        "255.255.255.255",
    ] {
        assert!(
            !is_public_download_address(blocked.parse().expect("IPv4")),
            "{blocked} 必须被拒"
        );
    }
    // 172.16/12 的边界：172.15 与 172.32 是公网。
    for allowed in ["172.15.255.255", "172.32.0.0"] {
        assert!(
            is_public_download_address(allowed.parse().expect("IPv4")),
            "{allowed} 在前缀之外，应当放行"
        );
    }
}

/// IPv6：公网放行，上游点名的转换 / 文档段与本地段全拒；NAT64 与 IPv4-mapped 递归判。
#[test]
fn ipv6_and_transition_prefixes_fail_closed() {
    for allowed in ["2606:4700:4700::1111", "2001:4860:4860::8888"] {
        assert!(
            is_public_download_address(allowed.parse().expect("IPv6")),
            "{allowed} 是公网地址"
        );
    }
    for blocked in [
        "::1",
        "::",
        "fe80::1",
        "fc00::1",
        "fd12:3456::1",
        "ff02::1",
        "2001:db8::1",
        "2002::1",
        "100::1",
        "2001::1",
        "2001:2::1",
        "2001:10::1",
        "2001:20::1",
        "3fff::1",
        "64:ff9b:1::1",
    ] {
        assert!(
            !is_public_download_address(blocked.parse().expect("IPv6")),
            "{blocked} 必须被拒"
        );
    }
    // 标准 NAT64 前缀按低 32 位 IPv4 再判一遍。
    assert!(is_public_download_address(
        "64:ff9b::8.8.8.8".parse().expect("IPv6")
    ));
    assert!(!is_public_download_address(
        "64:ff9b::127.0.0.1".parse().expect("IPv6")
    ));
    assert!(!is_public_download_address(
        "64:ff9b::10.0.0.1".parse().expect("IPv6")
    ));
    // IPv4-mapped 解包成 IPv4 再判。
    assert_eq!(
        unmap("::ffff:8.8.8.8".parse().expect("IPv6")).to_string(),
        "8.8.8.8"
    );
    assert!(is_public_download_address(
        "::ffff:8.8.8.8".parse().expect("IPv6")
    ));
    assert!(!is_public_download_address(
        "::ffff:127.0.0.1".parse().expect("IPv6")
    ));
}

/// 前缀表本身有几个条目（改表时这个数会提醒你回头读上游）。
#[test]
fn the_prefix_table_is_populated() {
    assert!(
        NON_PUBLIC_PREFIXES.len() >= 24,
        "上游 19 条 + 公网单播必需的几条"
    );
}

// =====================================================================
// URL / 嗅探 / key
// =====================================================================

/// 形状校验：合法 http/https 放行；缺 host、userinfo、fragment、别的 scheme 全拒。
#[test]
fn url_shape_validation_matches_upstream() {
    for allowed in [
        "http://cdn.example.test/a.png",
        "https://cdn.example.test/a.png?x=1",
    ] {
        assert!(
            validate_download_url(&reqwest::Url::parse(allowed).expect("URL")).is_ok(),
            "{allowed} 应当合法"
        );
    }
    // 缺 host 的那条在本文件里由 `url` crate 自己挡掉（`EmptyHost`）⇒ 这里的实现分支是防御性的。
    for (blocked, want) in [
        (
            "http://user:pw@cdn.example.test/a.png",
            MediaError::InvalidUrl,
        ),
        (
            "https://cdn.example.test/a.png#frag",
            MediaError::InvalidUrl,
        ),
        (
            "ftp://cdn.example.test/a.png",
            MediaError::UnsupportedScheme,
        ),
        // 没有 host 的 file: URL 走的是**第一条**分支（上游的判据顺序也是先看 host）。
        ("file:///etc/passwd", MediaError::InvalidUrl),
    ] {
        let url = reqwest::Url::parse(blocked).expect("URL");
        let error = validate_download_url(&url).expect_err(blocked);
        assert_eq!(error, want, "{blocked}");
    }
}

/// 同源判定看 scheme + host + 端口。
#[test]
fn origin_comparison_covers_scheme_host_and_port() {
    let base = reqwest::Url::parse("https://cdn.example.test/a.png").expect("URL");
    assert!(same_download_origin(
        &base,
        &reqwest::Url::parse("https://CDN.example.test/b.png").expect("URL")
    ));
    assert!(!same_download_origin(
        &base,
        &reqwest::Url::parse("http://cdn.example.test/b.png").expect("URL")
    ));
    assert!(!same_download_origin(
        &base,
        &reqwest::Url::parse("https://other.example.test/b.png").expect("URL")
    ));
    assert!(!same_download_origin(
        &base,
        &reqwest::Url::parse("https://cdn.example.test:8443/b.png").expect("URL")
    ));
}

/// 魔数嗅探只认白名单里的五种。
#[test]
fn content_type_sniffing_only_accepts_the_allowlist() {
    let cases: [(&[u8], Option<&str>); 7] = [
        (b"\x89PNG\r\n\x1a\n", Some("image/png")),
        (b"\xff\xd8\xff\xe0", Some("image/jpeg")),
        (b"GIF89a", Some("image/gif")),
        (b"GIF87a", Some("image/gif")),
        (b"RIFF\x00\x00\x00\x00WEBP", Some("image/webp")),
        (b"BM\x00\x00", Some("image/bmp")),
        (b"<html>", None),
    ];
    for (data, want) in cases {
        assert_eq!(sniff_image_content_type(data), want, "{data:?}");
    }
    assert_eq!(sniff_image_content_type(b""), None);
    assert_eq!(image_extension("image/png"), Some(".png"));
    assert_eq!(image_extension("image/jpeg"), Some(".jpg"));
    assert_eq!(image_extension("text/html"), None);
}

/// 对象 key：是输入（会话行 + 下载码 + 位次）的纯函数，且 workspace / 安装都在路径里。
#[test]
fn the_object_key_is_a_pure_function_of_its_inputs() {
    let installation = installation_with(json!({ "app_id": "app-key", "app_secret": "s" }));
    let message_id = Id::new();
    let resource = DingtalkMediaResource::at("code-1", "alt-1", 2);
    let key = media_object_key(&installation, message_id, &resource, 0);
    assert!(key.starts_with(&format!(
        "workspaces/{}/dingtalk/{}/",
        installation.workspace_id, installation.id
    )));
    assert_eq!(
        key,
        media_object_key(&installation, message_id, &resource, 0),
        "同输入同 key"
    );
    assert_ne!(
        key,
        media_object_key(&installation, message_id, &resource, 1),
        "位次进 key"
    );
    assert_ne!(
        key,
        media_object_key(
            &installation,
            message_id,
            &DingtalkMediaResource::at("code-2", "", 2),
            0
        ),
        "下载码进 key"
    );
    assert_eq!(
        key.rsplit('/').next().map(str::len),
        Some(64),
        "sha256 的十六进制"
    );
}

// =====================================================================
// 替身
// =====================================================================

/// 记账式存储。
#[derive(Default)]
struct RecordingStorage {
    uploads: Mutex<Vec<(String, usize, String, String)>>,
}

impl MediaStorage for RecordingStorage {
    fn upload(
        &self,
        key: &str,
        data: &[u8],
        content_type: &str,
        filename: &str,
    ) -> Result<String, String> {
        self.uploads.lock().expect("lock").push((
            key.to_string(),
            data.len(),
            content_type.to_string(),
            filename.to_string(),
        ));
        Ok(self.object_url(key))
    }

    fn object_url(&self, key: &str) -> String {
        format!("https://objects.example.test/{key}")
    }
}

/// 记账式意图账本；`owned` 决定"这一行还归我们吗"。
struct RecordingLedger {
    owned: bool,
    intents: Mutex<Vec<RecordPendingMediaObjectParams>>,
}

impl RecordingLedger {
    fn new(owned: bool) -> Self {
        Self {
            owned,
            intents: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl MediaIntentLedger for RecordingLedger {
    async fn record_pending_media_object(
        &self,
        params: RecordPendingMediaObjectParams,
    ) -> EngineResult<bool> {
        self.intents.lock().expect("lock").push(params);
        Ok(self.owned)
    }
}

/// 替身取回器：按下载码给字节或失败。
struct ScriptedFetcher {
    calls: Mutex<Vec<String>>,
    /// 下载码 → 结果；不在表里 ⇒ [`MediaError::Http`]。
    results: Vec<(&'static str, Result<Vec<u8>, MediaError>)>,
}

impl ScriptedFetcher {
    fn new(results: Vec<(&'static str, Result<Vec<u8>, MediaError>)>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            results,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }
}

impl MediaFetcher for ScriptedFetcher {
    fn fetch_resource(
        &self,
        _credentials: &Credentials,
        resource: &DingtalkMediaResource,
    ) -> Result<Fetched, MediaError> {
        self.calls
            .lock()
            .expect("lock")
            .push(resource.reference.clone());
        for (code, result) in &self.results {
            if *code == resource.reference {
                return result.clone().map(|data| Fetched {
                    content_type: sniff_image_content_type(&data).unwrap_or("image/png"),
                    data,
                });
            }
        }
        Err(MediaError::Http { status: 404 })
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00";

fn installation_with(config: serde_json::Value) -> ResolvedInstallation {
    let mut installation = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        crate::dingtalk::inbound::TYPE_DINGTALK,
        true,
    );
    installation.platform = Some(Arc::new(InstallationRow {
        id: installation.id,
        workspace_id: installation.workspace_id,
        agent_id: installation.agent_id,
        installer_user_id: installation.installer_user_id,
        status: "active".to_string(),
        config,
    }));
    installation
}

fn inbound(media: Vec<DingtalkMediaResource>) -> InboundMessage {
    InboundMessage {
        event_id: "ev".to_string(),
        message_id: "msg".to_string(),
        source: Source {
            channel_type: crate::dingtalk::inbound::TYPE_DINGTALK,
            chat_id: "chat".to_string(),
            chat_type: ChatType::P2p,
            sender_id: "staff".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Image,
        text: "[Image]".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: false,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::to_value(crate::dingtalk::inbound::DingtalkRawEvent {
            app_id: "app-key".to_string(),
            conversation_title: String::new(),
            current_text: String::new(),
            media,
        })
        .expect("raw"),
    }
}

fn resolver(
    fetch: Arc<dyn MediaFetcher>,
    ledger: Arc<RecordingLedger>,
    storage: Arc<RecordingStorage>,
) -> DingTalkMediaResolver {
    DingTalkMediaResolver::new(Decrypter::fail_closed(), storage, ledger).with_fetcher(fetch)
}

// =====================================================================
// 解析器
// =====================================================================

/// `has_media` 是纯解码：有下载码才算有媒体，坏 `raw` 不算。
#[test]
fn has_media_is_a_pure_decode() {
    let resolver = resolver(
        Arc::new(ScriptedFetcher::new(Vec::new())),
        Arc::new(RecordingLedger::new(true)),
        Arc::new(RecordingStorage::default()),
    );
    assert!(!resolver.has_media(&inbound(Vec::new())));
    assert!(resolver.has_media(&inbound(vec![DingtalkMediaResource::at("code-1", "", 0)])));
    let mut broken = inbound(Vec::new());
    broken.raw = json!("not an object");
    assert!(!resolver.has_media(&broken));
}

/// 一张图：意图行先落 → 取回 → 上传 → 回填 `MediaRef`（占位符与位次照上游）。
#[test]
fn one_image_records_intent_then_uploads_and_fills_the_ref() {
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::new(true));
    let fetch = Arc::new(ScriptedFetcher::new(vec![("code-1", Ok(PNG.to_vec()))]));
    let resolver = resolver(
        Arc::clone(&fetch) as Arc<dyn MediaFetcher>,
        Arc::clone(&ledger),
        Arc::clone(&storage),
    );
    let installation = installation_with(json!({ "app_id": "app-key", "app_secret": "s" }));
    let chat_message_id = Id::new();
    let message = inbound(vec![DingtalkMediaResource::at("code-1", "", 3)]);

    let enriched = resolver.resolve_media(
        &installation,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(chat_message_id),
        &message,
    );

    assert_eq!(enriched.media_refs.len(), 1);
    let reference = &enriched.media_refs[0];
    assert_eq!(reference.message_kind, MessageKind::Image);
    assert_eq!(reference.mime_type, "image/png");
    assert_eq!(reference.inline_placeholder, "[Image]");
    assert_eq!(reference.inline_index, 3);
    assert_eq!(reference.filename, "dingtalk-image-1.png");
    assert_eq!(reference.size_bytes, i64::try_from(PNG.len()).expect("len"));
    assert!(reference.storage_url.contains(&reference.storage_key));

    let intents = ledger.intents.lock().expect("lock");
    assert_eq!(intents.len(), 1, "意图行在下载之前就落了");
    assert_eq!(intents[0].storage_key, reference.storage_key);
    assert_eq!(intents[0].chat_message_id, Some(chat_message_id));
    assert_eq!(intents[0].installation_id, Some(installation.id));

    let uploads = storage.uploads.lock().expect("lock");
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].1, PNG.len());
    assert_eq!(uploads[0].2, "image/png");
    assert_eq!(uploads[0].3, "dingtalk-image-1.png");
    assert_eq!(fetch.calls(), vec!["code-1".to_string()]);
}

/// 两张图：一张失败**不影响**另一张；两张的意图行都在。
#[test]
fn a_failed_image_does_not_stop_the_others() {
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::new(true));
    let fetch = Arc::new(ScriptedFetcher::new(vec![
        ("code-1", Err(MediaError::Http { status: 500 })),
        ("code-2", Ok(PNG.to_vec())),
    ]));
    let resolver = resolver(
        Arc::clone(&fetch) as Arc<dyn MediaFetcher>,
        Arc::clone(&ledger),
        Arc::clone(&storage),
    );
    let installation = installation_with(json!({ "app_id": "app-key", "app_secret": "s" }));
    let message = inbound(vec![
        DingtalkMediaResource::at("code-1", "", 0),
        DingtalkMediaResource::at("code-2", "", 1),
    ]);
    let enriched = resolver.resolve_media(
        &installation,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert_eq!(enriched.media_refs.len(), 1, "只回填成功的那个");
    assert_eq!(enriched.media_refs[0].filename, "dingtalk-image-2.png");
    assert_eq!(fetch.calls().len(), 2, "两张都试过");
    assert_eq!(ledger.intents.lock().expect("lock").len(), 2);
    assert_eq!(
        storage.uploads.lock().expect("lock").len(),
        1,
        "失败的不上传"
    );
}

/// 超过 4 张 ⇒ 整条跳过（一张都不落意图，也一张都不取）。
#[test]
fn too_many_images_skip_the_whole_message() {
    let ledger = Arc::new(RecordingLedger::new(true));
    let fetch = Arc::new(ScriptedFetcher::new(Vec::new()));
    let resolver = resolver(
        Arc::clone(&fetch) as Arc<dyn MediaFetcher>,
        Arc::clone(&ledger),
        Arc::new(RecordingStorage::default()),
    );
    let installation = installation_with(json!({ "app_id": "app-key", "app_secret": "s" }));
    let media: Vec<DingtalkMediaResource> = (0..=MAX_IMAGES_PER_MESSAGE)
        .map(|index| DingtalkMediaResource::at(format!("code-{index}"), "", 0))
        .collect();
    let message = inbound(media);
    let enriched = resolver.resolve_media(
        &installation,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &message,
    );
    assert!(enriched.media_refs.is_empty());
    assert!(ledger.intents.lock().expect("lock").is_empty());
    assert!(fetch.calls().is_empty());
}

/// 账本说"这一行已经归对账器" ⇒ **不**上传、**不**回填（别复活那一行）。
#[test]
fn a_key_owned_by_the_reconciler_is_never_uploaded() {
    let storage = Arc::new(RecordingStorage::default());
    let ledger = Arc::new(RecordingLedger::new(false));
    let fetch = Arc::new(ScriptedFetcher::new(vec![("code-1", Ok(PNG.to_vec()))]));
    let resolver = resolver(
        Arc::clone(&fetch) as Arc<dyn MediaFetcher>,
        Arc::clone(&ledger),
        Arc::clone(&storage),
    );
    let installation = installation_with(json!({ "app_id": "app-key", "app_secret": "s" }));
    let enriched = resolver.resolve_media(
        &installation,
        &crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() },
        Id::new(),
        Some(Id::new()),
        &inbound(vec![DingtalkMediaResource::at("code-1", "", 0)]),
    );
    assert!(enriched.media_refs.is_empty());
    assert!(fetch.calls().is_empty(), "拿到意图之前不该下载");
    assert!(storage.uploads.lock().expect("lock").is_empty());
}

/// 缺 `chat_message_id` / 解不出凭据 / 安装行类型不符 / 没有媒体 ⇒ 都不动作。
#[test]
fn the_four_skip_conditions_do_not_touch_any_port() {
    let ledger = Arc::new(RecordingLedger::new(true));
    let fetch = Arc::new(ScriptedFetcher::new(vec![("code-1", Ok(PNG.to_vec()))]));
    let storage = Arc::new(RecordingStorage::default());
    let resolver = resolver(
        Arc::clone(&fetch) as Arc<dyn MediaFetcher>,
        Arc::clone(&ledger),
        Arc::clone(&storage),
    );
    let installation = installation_with(json!({ "app_id": "app-key", "app_secret": "s" }));
    let identity = crate::engine::resolvers::ResolvedIdentity { user_id: Id::new() };
    let message = inbound(vec![DingtalkMediaResource::at("code-1", "", 0)]);

    // ① 没有 chat_message 行。
    let enriched = resolver.resolve_media(&installation, &identity, Id::new(), None, &message);
    assert!(enriched.media_refs.is_empty());
    // ② 没有媒体。
    let enriched = resolver.resolve_media(
        &installation,
        &identity,
        Id::new(),
        Some(Id::new()),
        &inbound(Vec::new()),
    );
    assert!(enriched.media_refs.is_empty());
    // ③ 凭据解不开（失败关闭 + 密文形态）。
    let encrypted = installation_with(json!({
        "app_id": "app-key",
        "app_secret_encrypted": "CIPHERTEXT-B64",
    }));
    let enriched =
        resolver.resolve_media(&encrypted, &identity, Id::new(), Some(Id::new()), &message);
    assert!(enriched.media_refs.is_empty());
    // ④ 安装行类型不符（`Platform` 为空）。
    let bare = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        crate::dingtalk::inbound::TYPE_DINGTALK,
        true,
    );
    let enriched = resolver.resolve_media(&bare, &identity, Id::new(), Some(Id::new()), &message);
    assert!(enriched.media_refs.is_empty());

    assert!(fetch.calls().is_empty(), "四条跳过都不该下载");
    assert!(ledger.intents.lock().expect("lock").is_empty());
    assert!(storage.uploads.lock().expect("lock").is_empty());
}

// =====================================================================
// 真 reqwest 的守卫
// =====================================================================

/// 直连内网 / 回环的下载目标在**连接之前**就被挡（`BlockedAddress`，不是 `Transport`）。
#[tokio::test]
async fn the_guarded_client_blocks_non_public_targets_before_connecting() {
    let client = super::guarded_client().expect("client");
    let timeout = Duration::from_secs(2);
    for url in [
        // IP 字面量：`reqwest` 的 `dns_resolver` 不参与 ⇒ 由 `guard_download_url` 挡。
        "http://127.0.0.1:9/a.png",
        "http://10.0.0.1/a.png",
        "https://192.168.1.1/a.png",
        "http://[::1]:9/a.png",
        "http://[fd00::1]/a.png",
        // 域名：由 `PublicOnlyResolver` 挡（解析出来的地址非公网）。
        "http://localhost:9/a.png",
    ] {
        let error = super::fetch_bytes(&client, url, timeout)
            .await
            .expect_err(url);
        assert_eq!(error, MediaError::BlockedAddress, "{url}");
    }
    // 形状 / scheme 不合法 ⇒ 在解析器之前就被拒。
    let error = super::fetch_bytes(&client, "file:///etc/passwd", timeout)
        .await
        .expect_err("file:");
    assert_eq!(error, MediaError::InvalidUrl);
    let error = super::fetch_bytes(&client, "ftp://cdn.example.test/a.png", timeout)
        .await
        .expect_err("ftp:");
    assert_eq!(error, MediaError::UnsupportedScheme);
}

/// 解析器本身：IP 字面量即时判定，`localhost` 一定被拒（那正是 SSRF 的入口）。
#[tokio::test]
async fn the_resolver_rejects_any_non_public_answer() {
    let public = super::resolve_public("8.8.8.8")
        .await
        .expect("公网 IP 字面量");
    assert_eq!(public.len(), 1);
    assert_eq!(public[0].ip().to_string(), "8.8.8.8");

    for host in ["127.0.0.1", "localhost", "10.0.0.1"] {
        let error = super::resolve_public(host).await.expect_err(host);
        assert!(
            error.downcast_ref::<MediaError>() == Some(&MediaError::BlockedAddress),
            "{host}: {error}"
        );
    }
}

/// 错误文案里**没有** URL / 下载码（两者都是短期凭据）。
#[test]
fn media_errors_never_carry_the_url_or_the_download_code() {
    let samples = [
        MediaError::InvalidUrl,
        MediaError::UnsupportedScheme,
        MediaError::BlockedAddress,
        MediaError::Http { status: 403 },
        MediaError::TooLarge,
        MediaError::DisallowedContentType {
            content_type: "text/html".to_string(),
        },
        MediaError::Storage,
        MediaError::Ledger,
        MediaError::NoDownloadCode,
        MediaError::DowngradeRedirect,
        MediaError::CrossOriginRedirect,
        MediaError::TooManyRedirects,
        MediaError::Resolve,
        MediaError::Transport,
        MediaError::Read,
    ];
    for error in samples {
        let text = format!("{error} {error:?}");
        assert!(!text.contains("downloadCode"), "{text}");
        assert!(!text.contains("https://"), "{text}");
        assert!(!text.contains("ticket="), "{text}");
    }
}
