//! [`super`] 的**纯函数面**：对象 key / 文件名 / 内容类型 / 扩展名 / 安全段。
//!
//! 拆出本文件是**门 ⑩** 的 800 行硬限（`media.rs` 在 `cargo fmt` 之后 807 行 ——
//! 生产文件的上限与用例文件同一条）。切点就是上游自己那一组"命名与定键"的函数
//! （`mediaObjectKey` / `mediaFilename` / `mediaContentType` / `cleanFilename` /
//! `mediaExtension` / `safePathSegment` / `firstNonEmpty`）。
//!
//! **本文件零 I/O、零 `tracing::*`**：全是纯函数（逐条可单测）。

use mc_core::channel::message::MessageKind;
use mc_core::id::Id;

use super::{FetchedResource, LarkMediaResource};
use crate::engine::resolvers::ResolvedInstallation;
use crate::lark::feishu_channel::LarkInboundMessage;

// =====================================================================
// 对象 key / 文件名 / 类型（纯函数，上游同名函数）
// =====================================================================

/// 对象 key 从**这条消息将被挂到的 chat 消息**派生，而不是只看平台消息（上游 `mediaObjectKey`）。
///
/// 上游注释逐字：一条平台消息可能被摄取两次（去重行的所有权 60s 后可再生、24h 后清掉），
/// 共享一个 key 会让第二次摄取撞进第一次的账本行 —— 那一行可能已是墓碑（意图 upsert 拒绝
/// 任何已经离开 `pending` 的对象），于是第二次摄取会**静默丢掉**它的媒体，直到重删排期跑完。
/// 一条 chat 消息一个 key 让两次摄取彼此独立；什么都不会泄，因为每一次的对象都由它自己
/// 那一行账本覆盖。
#[must_use]
pub fn media_object_key(
    installation: &ResolvedInstallation,
    chat_message_id: Id,
    resource: &LarkMediaResource,
) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(chat_message_id.0.to_string().as_bytes());
    hasher.update([0u8]);
    hasher.update(resource.message_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(resource.fetch_type.as_bytes());
    hasher.update([0u8]);
    hasher.update(resource.key.as_bytes());
    let digest = hasher.finalize();
    format!(
        "workspaces/{}/lark/{}/{}",
        installation.workspace_id.0,
        installation.id.0,
        hex::encode(digest)
    )
}

/// 给存储对象挑一个名字（上游 `mediaFilename`）。
///
/// `index` 用来消歧：一条消息上的所有资源共享同一个 `MessageID`，所以三张照片曾经产出三个
/// 都叫 `feishu-image-<msg>.jpg` 的对象。
#[must_use]
pub fn media_filename(
    message: &LarkInboundMessage,
    resource: &LarkMediaResource,
    downloaded: &FetchedResource,
    content_type: &str,
    index: usize,
) -> String {
    for candidate in [
        downloaded.filename.as_deref(),
        Some(resource.filename.as_str()),
    ] {
        if let Some(name) = candidate.and_then(clean_filename) {
            return ensure_audio_filename_extension(&name, resource.kind, content_type);
        }
    }
    let prefix = match resource.kind {
        MessageKind::Image => "feishu-image",
        MessageKind::Video => "feishu-video",
        MessageKind::Audio => "feishu-audio",
        _ => "feishu-file",
    };
    let mut name = format!("{prefix}-{}", safe_path_segment(&message.message_id));
    if index > 0 {
        let _ = std::fmt::Write::write_fmt(&mut name, format_args!("-{}", index + 1));
    }
    name.push_str(media_extension(content_type));
    name
}

/// 存储对象该算什么类型（上游 `mediaContentType`）。
#[must_use]
pub fn media_content_type(resource: &LarkMediaResource, downloaded: &FetchedResource) -> String {
    let content_type = downloaded.content_type.trim().to_string();
    if resource.kind == MessageKind::Audio && is_generic_binary_content_type(&content_type) {
        let hinted = resource.mime_type.trim().to_string();
        if !is_generic_binary_content_type(&hinted) {
            return hinted;
        }
        // 飞书的音频消息是 Opus。它的资源端点可能返回一个没有扩展名的
        // `Content-Disposition` 文件名，外加泛化的 `audio/octet-stream`
        // ⇒ 在这里保住协议层的格式。
        return "audio/opus".to_string();
    }
    if content_type.is_empty() {
        let hinted = resource.mime_type.trim().to_string();
        if !hinted.is_empty() {
            return hinted;
        }
        return "application/octet-stream".to_string();
    }
    content_type
}

/// 是不是"泛化的二进制类型"（上游 `isGenericBinaryContentType`）。
#[must_use]
pub fn is_generic_binary_content_type(content_type: &str) -> bool {
    let base = content_type
        .split_once(';')
        .map_or(content_type, |(head, _)| head);
    matches!(
        base.trim().to_ascii_lowercase().as_str(),
        "" | "application/octet-stream" | "audio/octet-stream"
    )
}

/// 音频文件名没有扩展名时补一个（上游 `ensureAudioFilenameExtension`）。
#[must_use]
pub fn ensure_audio_filename_extension(
    name: &str,
    kind: MessageKind,
    content_type: &str,
) -> String {
    if kind != MessageKind::Audio || std::path::Path::new(name).extension().is_some() {
        return name.to_string();
    }
    format!("{name}{}", media_extension(content_type))
}

/// 清一个候选文件名（上游 `cleanFilename`）。
///
/// `Path::file_name` 会解路径，但它把 `..` 与 `...` 原样还回来。一个**只有点**的名字不是
/// 文件名：放它过去意味着这个对象后面被渲染或另存时显示名是 `..`。遇到就退回到生成名。
#[must_use]
pub fn clean_filename(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or(&normalized);
    if base.is_empty() || base == "/" || base.trim_matches('.').is_empty() {
        return None;
    }
    Some(base.to_string())
}

/// `Content-Type` → 扩展名（上游 `mediaExtension`）。
///
/// 常见类型**钉死**在这里而不是交给宿主的 mime 表：slim 容器镜像不带 `/etc/mime.types`，
/// 上游为此专门把 `application/pdf` 钉住（本仓同样，不依赖任何宿主文件）。
#[must_use]
pub fn media_extension(content_type: &str) -> &'static str {
    let base = content_type
        .split_once(';')
        .map_or(content_type, |(head, _)| head)
        .trim();
    match base {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "video/mp4" => ".mp4",
        "audio/opus" => ".opus",
        "audio/ogg" => ".ogg",
        "audio/amr" => ".amr",
        "audio/mpeg" => ".mp3",
        "application/pdf" => ".pdf",
        _ => "",
    }
}

/// 一个能安全进对象 key 的路径段（上游 `safePathSegment`）。
#[must_use]
pub fn safe_path_segment(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    let mapped: String = trimmed
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    let out = mapped.trim_matches('_');
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out.to_string()
    }
}

/// 第一个非空白值（上游 `firstNonEmpty`）。
pub(super) fn first_non_empty(values: &[&String]) -> String {
    for value in values {
        if !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    String::new()
}
