//! `media_ingest` 的**命名与描述**那一面（上游 `media_ingest.go` 的
//! `mediaObjectKey` / `describeMedia` / `fallbackMediaName` / `mediaExtension` /
//! `safeMediaSegment` / `baseContentType` 与 `http.DetectContentType` 的替代）。
//!
//! - **写者**：M7-18（`LUM-1783`）。本文件是 `media_ingest.rs` 的子模块：拆分依据是
//!   `docs/60-M7-PLAN.md` §6.3 的门 ⑩ 800 行硬限（逐条清单见 `docs/32` §35 的 D1）。
//! - 🔴 **两张手写的表**（登记 `docs/32` §35 的 D8）：`mime.ParseMediaType` /
//!   `mime.TypeByExtension` / `mime.ExtensionsByType` / `http.DetectContentType` 在本仓
//!   **都没有**（`mime` crate 不在 `mc-channel` 的依赖边里，M7-0 冻结）⇒ 表外一律空串（不猜）。

use mc_core::channel::message::MessageKind;
use mc_core::id::Id;
use sha2::{Digest as _, Sha256};

use crate::engine::resolvers::ResolvedInstallation;

use super::{InboundMedia, WecomInbound};
use crate::wecom::media_download::clean_media_filename;

// =====================================================================
// 命名与描述
// =====================================================================

/// 上游 `mediaObjectKey`：给对象起名。
///
/// 它派生自**聊天**消息而不是 `WeCom` 消息，理由与 lark 记下的那条一样：一条平台消息可能被摄入
/// **两次**（入站去重的 claim 过期之后可以被重新取得），而一个共用的 key 会让第二次摄入撞进
/// 第一次那条账本行 —— 那可能是一条意图 upsert 会拒的墓碑，于是媒体被**悄悄**丢掉。
/// 附件在消息里的位置也是 key 的一部分，因为一条图文混排可以带好几个，而 url 稳定不到能拿来
/// 当键的程度。
#[must_use]
pub fn media_object_key(
    installation: &ResolvedInstallation,
    chat_message_id: Id,
    msg_id: &str,
    index: usize,
    kind: MessageKind,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(chat_message_id.to_string().as_bytes());
    hasher.update([0u8]);
    hasher.update(msg_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(kind.as_str().as_bytes());
    hasher.update([0u8]);
    hasher.update(index.to_string().as_bytes());
    let digest = hasher.finalize();
    format!(
        "workspaces/{}/wecom/{}/{}",
        installation.workspace_id,
        installation.id,
        hex::encode(digest)
    )
}

/// 上游 `describeMedia`：想清楚这个文件该叫什么、该说它是什么。
///
/// 回调体两样都没有，所以名字来自下载的 `Content-Disposition`、而类型来自名字的扩展名，
/// 退一步才是嗅解密后的字节 —— 对**真的是 zip 容器**的那些格式（`.docx`、`.xlsx`）扩展名是更好
/// 的信号，而一个名字都没有的时候嗅字节是更好的那一个。
#[must_use]
pub fn describe_media(
    inbound: &WecomInbound,
    index: usize,
    media: &InboundMedia,
    header_name: &str,
    plain: &[u8],
) -> (String, String) {
    let mut filename = clean_media_filename(header_name);
    let mut content_type = String::new();
    if let Some(extension) = std::path::Path::new(&filename)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        content_type_for_extension(extension).clone_into(&mut content_type);
    }
    if content_type.is_empty() {
        content_type = sniff_content_type(plain);
    }
    if content_type.is_empty() {
        "application/octet-stream".clone_into(&mut content_type);
    }
    if filename.is_empty() {
        filename = fallback_media_name(&inbound.msg_id, index, media.kind, &content_type);
    }
    (filename, content_type)
}

/// 上游 `fallbackMediaName`：给一个服务端没起名的附件起名。
///
/// 它在消息内必须**唯一**，所以附件的位置在里面 —— 一条图文混排里的两张照片否则会落成同一个
/// 名字两次。
#[must_use]
pub fn fallback_media_name(
    msg_id: &str,
    index: usize,
    kind: MessageKind,
    content_type: &str,
) -> String {
    let prefix = match kind {
        MessageKind::Image => "wecom-image",
        MessageKind::Video => "wecom-video",
        _ => "wecom-file",
    };
    format!(
        "{prefix}-{}-{index}{}",
        safe_media_segment(msg_id),
        media_extension(content_type)
    )
}

/// 上游 `mediaExtension`：给一个内容类型挑一个扩展名，**偏好熟悉的拼法**而不是 mime 数据库
/// 恰好先列出来的那个（`image/jpeg` 在有些系统上解出 `.jfif`）。
///
/// 🔴 `mime.ExtensionsByType` 在本仓**没有**（`mime` crate 不在依赖边里，M7-0 冻结）⇒ 这里是
/// 一张手写的表（登记 `docs/32` §35 的 D8），表外的一律空串（不猜一个扩展名出来）。
#[must_use]
pub fn media_extension(content_type: &str) -> &'static str {
    let base = base_content_type(content_type);
    match base.as_str() {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/bmp" => ".bmp",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "audio/amr" => ".amr",
        "audio/mpeg" => ".mp3",
        "audio/wav" | "audio/x-wav" => ".wav",
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "text/plain" => ".txt",
        "text/csv" => ".csv",
        "text/markdown" => ".md",
        "text/html" => ".html",
        "application/json" => ".json",
        "application/xml" | "text/xml" => ".xml",
        _ => "",
    }
}

/// 去掉内容类型可能带上的参数，好让 `"text/csv; charset=utf-8"` 与 `"text/csv"` 比得起来。
#[must_use]
pub fn base_content_type(content_type: &str) -> String {
    let lowered = content_type.trim().to_ascii_lowercase();
    match lowered.split_once(';') {
        Some((base, _)) => base.trim().to_owned(),
        None => lowered,
    }
}

/// 扩展名 → 内容类型（上游 `mime.TypeByExtension` 的本地手写表；表外 ⇒ 空串）。
#[must_use]
pub fn content_type_for_extension(extension: &str) -> &'static str {
    let lowered = extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    match lowered.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "amr" => "audio/amr",
        "mp3" => "audio/mpeg",
        "wav" => "audio/x-wav",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "",
    }
}

/// 上游 `http.DetectContentType` 的**收窄版**：只认这张表里那几种的**魔数**。
///
/// 比 `DetectContentType` 更严（它会把别的类型也认出来）；认不出 ⇒ 空串。
/// 表比 Go 那张小 ⇒ 登记为收缩（`docs/32` §35 的 D8）。
#[must_use]
pub fn sniff_content_type(data: &[u8]) -> String {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return "image/png".to_owned();
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg".to_owned();
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return "image/gif".to_owned();
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return "image/webp".to_owned();
    }
    if data.starts_with(b"BM") {
        return "image/bmp".to_owned();
    }
    if data.starts_with(b"%PDF-") {
        return "application/pdf".to_owned();
    }
    if data.starts_with(b"PK\x03\x04") {
        // 一个 zip 容器；`.docx` / `.xlsx` / `.pptx` 都在这一层之下，而**扩展名**才是更好的
        // 信号（上游逐字）⇒ 嗅出来只当兜底。
        return "application/zip".to_owned();
    }
    if data.starts_with(b"ID3") || data.starts_with(&[0xff, 0xfb]) {
        return "audio/mpeg".to_owned();
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WAVE" {
        return "audio/x-wav".to_owned();
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        return "video/mp4".to_owned();
    }
    if data.starts_with(b"#!AMR") {
        return "audio/amr".to_owned();
    }
    String::new()
}

/// 上游 `safeMediaSegment`：把一个 id 压成在文件名里安全的字符。
#[must_use]
pub fn safe_media_segment(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "unknown".to_owned();
    }
    let mut out = String::with_capacity(trimmed.len());
    for character in trimmed.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    let trimmed_out = out.trim_matches('_');
    if trimmed_out.is_empty() {
        return "unknown".to_owned();
    }
    trimmed_out.to_owned()
}
