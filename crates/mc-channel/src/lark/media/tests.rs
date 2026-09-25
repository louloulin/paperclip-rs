//! [`super`] 的用例：**抽取 / 路径与时机的纯函数 / `has_media` 的契约**。
//!
//! 上游 `media_ingest.go` 那批用例里与"解析"有关的那一半（`media_filename_test.go` /
//! `media_dedup_test.go`）。完整摄入回路（意图行 → 下载 → 上传）在 [`ingest`]，
//! 共用夹具在 [`fixtures`]。

mod fixtures;
mod ingest;

use std::sync::Arc;

use mc_core::channel::message::MessageKind;
use mc_core::id::Id;

use super::super::client::StubApiClient;
use super::super::feishu_channel::Decrypter;
use super::*;
use fixtures::*;
use ingest::{NoopLedger, NoopStorage};
// =====================================================================
// 一、抽取（纯函数）
// =====================================================================

/// 上游 `mediaResourcesFromMessage` 的 `msg_type` 分支逐条。
#[test]
fn extraction_dispatches_per_message_type() {
    // image → image_key / fetch_type=image。
    let resources = media_resources_from_message(&payload("image", r#"{"image_key":"img_x"}"#));
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].key(), "img_x");
    assert_eq!(resources[0].kind(), MessageKind::Image);
    assert_eq!(resources[0].fetch_type(), "image");
    assert_eq!(resources[0].message_id, "om-1");

    // media / video → file_key / fetch_type=file，类别 Video。
    for message_type in ["media", "video"] {
        let resources =
            media_resources_from_message(&payload(message_type, r#"{"file_key":"fk"}"#));
        assert_eq!(resources.len(), 1, "{message_type}");
        assert_eq!(resources[0].kind(), MessageKind::Video, "{message_type}");
        assert_eq!(resources[0].fetch_type(), "file", "{message_type}");
    }

    // file → File；audio → Audio（同一个 file_key）。
    let file = media_resources_from_message(&payload(
        "file",
        r#"{"file_key":"fk","file_name":"a.pdf","mime_type":"application/pdf"}"#,
    ));
    assert_eq!(file[0].kind(), MessageKind::File);
    assert_eq!(file[0].filename, "a.pdf");
    let audio = media_resources_from_message(&payload("audio", r#"{"file_key":"fk"}"#));
    assert_eq!(audio[0].kind(), MessageKind::Audio);

    // 缺键 / 认不出的类型 / 坏 JSON ⇒ 空（不是错误）。
    assert!(media_resources_from_message(&payload("image", "{}")).is_empty());
    assert!(media_resources_from_message(&payload("text", r#"{"text":"hi"}"#)).is_empty());
    assert!(media_resources_from_message(&payload("image", "not json")).is_empty());
    assert!(media_resources_from_message(&payload("image", "")).is_empty());
}

/// `size_bytes` 优先，`size` 是回退；文件名也接受 `name`。
#[test]
fn extraction_falls_back_on_name_and_size_fields() {
    let resources = media_resources_from_message(&payload(
        "file",
        r#"{"file_key":"fk","name":"alt.bin","size":42}"#,
    ));
    assert_eq!(resources[0].filename, "alt.bin");
    assert_eq!(resources[0].size_bytes, 42);

    let resources = media_resources_from_message(&payload(
        "file",
        r#"{"file_key":"fk","size_bytes":7,"size":42}"#,
    ));
    assert_eq!(resources[0].size_bytes, 7, "size_bytes 优先");
}

/// `post` 里的 `img` / `media` span 被抽出来，**同一个键重复出现只留一份**
/// （上游 `mediaResourcesFromPost` 的去重理由逐字：共享对象 key 的两次上传会互相毁掉）。
#[test]
fn post_media_spans_are_deduplicated() {
    let content = r#"{"content":[[
        {"tag":"img","image_key":"img_a"},
        {"tag":"img","image_key":"img_a"},
        {"tag":"img","image_key":"img_b"},
        {"tag":"media","file_key":"fk_a"},
        {"tag":"media","file_key":"fk_a"},
        {"tag":"text","text":"忽略"}
    ] ]}"#;
    let resources = media_resources_from_post(&payload("post", content));
    let keys: Vec<&str> = resources.iter().map(LarkMediaResource::key).collect();
    assert_eq!(keys, vec!["img_a", "img_b", "fk_a"]);
    assert_eq!(resources[0].kind(), MessageKind::Image);
    assert_eq!(resources[2].kind(), MessageKind::Video);
    assert_eq!(resources[2].fetch_type(), "file");

    // 空 span / 坏 JSON / 空正文都返回空。
    assert!(media_resources_from_post(&payload("post", r#"{"content":[[]]}"#)).is_empty());
    assert!(media_resources_from_post(&payload("post", "not json")).is_empty());
    assert!(media_resources_from_post(&payload("post", "")).is_empty());
}

/// `Debug` 只报键的**长度**（平台资源键等价于凭据）。
#[test]
fn resource_debug_reports_the_key_length_only() {
    let resources =
        media_resources_from_message(&payload("image", r#"{"image_key":"img_SECRET"}"#));
    let rendered = format!("{:?}", resources[0]);
    assert!(!rendered.contains("img_SECRET"), "{rendered}");
    assert!(rendered.contains("key_len: 10"), "{rendered}");
}

// =====================================================================
// 二、路径与时机的纯函数
// =====================================================================

/// 对象 key 是 `(chat 消息, 平台消息, 资源类型, 键)` 的函数，且带 workspace / 安装前缀。
#[test]
fn object_key_is_deterministic_and_scoped() {
    let installed = resolved(installation(None));
    let resources = media_resources_from_message(&payload("image", r#"{"image_key":"img_x"}"#));
    let chat_message_id = Id::new();

    let first = media_object_key(&installed, chat_message_id, &resources[0]);
    let second = media_object_key(&installed, chat_message_id, &resources[0]);
    assert_eq!(first, second, "同一个输入必须给出同一个 key");
    assert!(first.starts_with(&format!(
        "workspaces/{}/lark/{}/",
        installed.workspace_id.0, installed.id.0
    )));

    // 换一个 chat 消息 ⇒ 换 key（上游注释：同一条平台消息被摄取两次时彼此独立）。
    assert_ne!(
        first,
        media_object_key(&installed, Id::new(), &resources[0])
    );
    // 换一个资源键 ⇒ 换 key。
    let other = media_resources_from_message(&payload("image", r#"{"image_key":"img_y"}"#));
    assert_ne!(
        first,
        media_object_key(&installed, chat_message_id, &other[0])
    );
}

/// 文件名：优先下载响应的 `Content-Disposition`，其次 payload 里的名字，最后按序号生成。
#[test]
fn filenames_prefer_the_response_then_the_payload_then_a_generated_name() {
    let message = payload("image", r#"{"image_key":"img_x"}"#);
    let resources = media_resources_from_message(&message);

    let fetched = FetchedResource {
        data: vec![1, 2, 3],
        content_type: "image/png".to_string(),
        filename: Some("photo.png".to_string()),
        size_bytes: 3,
    };
    assert_eq!(
        media_filename(&message, &resources[0], &fetched, "image/png", 0),
        "photo.png"
    );

    // 响应没给名字 ⇒ 用 payload 的名字（这里 payload 也没有 ⇒ 生成）。
    let fetched = FetchedResource {
        filename: None,
        ..fetched.clone()
    };
    assert_eq!(
        media_filename(&message, &resources[0], &fetched, "image/png", 0),
        "feishu-image-om-1.png"
    );
    // 第二个资源带序号（上游：三条照片曾经全叫 `feishu-image-<msg>.jpg`）。
    assert_eq!(
        media_filename(&message, &resources[0], &fetched, "image/png", 1),
        "feishu-image-om-1-2.png"
    );

    // payload 带名字时用它。
    let named = media_resources_from_message(&payload(
        "file",
        r#"{"file_key":"fk","file_name":"报表.pdf","mime_type":"application/pdf"}"#,
    ));
    let fetched = FetchedResource {
        filename: None,
        content_type: "application/pdf".to_string(),
        data: vec![1],
        size_bytes: 1,
    };
    assert_eq!(
        media_filename(
            &payload("file", "{}"),
            &named[0],
            &fetched,
            "application/pdf",
            0
        ),
        "报表.pdf"
    );
}

/// `clean_filename`：路径被剥掉；**只有点**的名字退回到生成名（上游原注）。
#[test]
fn clean_filename_rejects_dot_only_names() {
    assert_eq!(clean_filename("  a/b/c.png  "), Some("c.png".to_string()));
    assert_eq!(
        clean_filename("win\\path\\d.txt"),
        Some("d.txt".to_string())
    );
    assert_eq!(clean_filename(""), None);
    assert_eq!(clean_filename("   "), None);
    assert_eq!(clean_filename(".."), None);
    assert_eq!(clean_filename("..."), None);
    assert_eq!(clean_filename("/"), None);
    // 以点开头的**真**文件名照旧保留（上游 `strings.Trim(name, ".")` 的判据）。
    assert_eq!(clean_filename(".hidden"), Some(".hidden".to_string()));
}

/// 内容类型：音频的泛化类型保住协议层格式（飞书音频是 Opus）。
#[test]
fn content_type_preserves_the_audio_protocol_format() {
    let audio = media_resources_from_message(&payload("audio", r#"{"file_key":"fk"}"#));
    let generic = FetchedResource {
        data: Vec::new(),
        content_type: "audio/octet-stream".to_string(),
        filename: None,
        size_bytes: 0,
    };
    assert_eq!(media_content_type(&audio[0], &generic), "audio/opus");

    // payload 自己带了更具体的 mime ⇒ 用它。
    let hinted = media_resources_from_message(&payload(
        "audio",
        r#"{"file_key":"fk","mime_type":"audio/amr"}"#,
    ));
    assert_eq!(media_content_type(&hinted[0], &generic), "audio/amr");

    // 非音频：响应给了就用响应的；都没给 ⇒ octet-stream（上游只对**音频**做协议层兜底）。
    let image = media_resources_from_message(&payload("image", r#"{"image_key":"img_x"}"#));
    let no_type = FetchedResource {
        content_type: String::new(),
        ..generic.clone()
    };
    assert_eq!(
        media_content_type(&image[0], &no_type),
        "application/octet-stream"
    );
    // 非音频时响应给的泛化类型**原样透出**（上游不做协议层改写）。
    assert_eq!(
        media_content_type(&image[0], &generic),
        "audio/octet-stream"
    );
    let png = FetchedResource {
        content_type: "  image/png  ".to_string(),
        ..generic.clone()
    };
    assert_eq!(media_content_type(&image[0], &png), "image/png");
    assert!(is_generic_binary_content_type(
        "AUDIO/OCTET-STREAM; charset=x"
    ));
    assert!(!is_generic_binary_content_type("audio/opus"));
}

/// 扩展名与安全段：常见类型**钉死**（不依赖宿主 mime 表），认不出就空串。
#[test]
fn extensions_and_segments_are_pinned() {
    for (content_type, extension) in [
        ("image/jpeg", ".jpg"),
        ("image/png", ".png"),
        ("image/gif", ".gif"),
        ("image/webp", ".webp"),
        ("video/mp4", ".mp4"),
        ("audio/opus", ".opus"),
        ("audio/ogg", ".ogg"),
        ("audio/amr", ".amr"),
        ("audio/mpeg", ".mp3"),
        ("application/pdf", ".pdf"),
        ("application/vnd.unknown", ""),
        ("image/png; charset=x", ".png"),
    ] {
        assert_eq!(media_extension(content_type), extension, "{content_type}");
    }
    assert_eq!(safe_path_segment("om-abc_123"), "om-abc_123");
    assert_eq!(safe_path_segment("om/abc 123"), "om_abc_123");
    assert_eq!(safe_path_segment(""), "unknown");
    assert_eq!(safe_path_segment("///"), "unknown");
    // 音频缺扩展名时补一个（上游 `ensureAudioFilenameExtension`）。
    assert_eq!(
        ensure_audio_filename_extension("voice", MessageKind::Audio, "audio/opus"),
        "voice.opus"
    );
    assert_eq!(
        ensure_audio_filename_extension("voice.amr", MessageKind::Audio, "audio/opus"),
        "voice.amr",
        "已有扩展名不动"
    );
    assert_eq!(
        ensure_audio_filename_extension("photo", MessageKind::Image, "image/png"),
        "photo"
    );
}

// =====================================================================
// 三、`has_media` 的"纯内存无 I/O"契约
// =====================================================================

/// 纯解码：`raw` 空 / 解不开 / 没资源都返回 `false`，有资源返回 `true`。
#[test]
fn has_media_is_a_pure_memory_check() {
    // 替身客户端对**每一个**传输调用都回 NotConfigured ⇒ 这条用例一旦发起调用就会红。
    let resolver = LarkMediaResolver::new(
        Arc::new(StubApiClient::new()),
        Decrypter::fail_closed(),
        Arc::new(NoopStorage),
        Arc::new(NoopLedger),
    );

    assert!(resolver.has_media(&envelope(&payload("image", r#"{"image_key":"img_x"}"#))));
    assert!(!resolver.has_media(&envelope(&payload("text", r#"{"text":"hi"}"#))));
    assert!(!resolver.has_media(&envelope(&payload("image", "{}"))));

    let mut empty_raw = envelope(&payload("image", r#"{"image_key":"img_x"}"#));
    empty_raw.raw = serde_json::Value::Null;
    assert!(!resolver.has_media(&empty_raw));

    let mut broken_raw = envelope(&payload("image", r#"{"image_key":"img_x"}"#));
    broken_raw.raw = serde_json::json!({"not":"this shape"});
    assert!(!resolver.has_media(&broken_raw));
}

/// 自由函数版的同一判据。
#[test]
fn free_has_media_matches_the_trait_method() {
    assert!(has_media(&payload("image", r#"{"image_key":"img_x"}"#)));
    assert!(!has_media(&payload("text", r#"{"text":"hi"}"#)));
}
