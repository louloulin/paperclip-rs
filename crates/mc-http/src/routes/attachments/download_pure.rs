//! `download.rs` 的**纯函数半边**（无 I/O、无状态 ⇒ 可直接单测）。
//!
//! 拆出来是门 ⑩ 的 800 行硬上限要求（先例 = `routes/onboarding/tests.rs`，M9-3），
//! 切分线是**「纯判定」对「I/O」**这条真的接缝：
//!
//! | 这一半 | 另一半（`download.rs`） |
//! |---|---|
//! | 能力签名 / 验签（HMAC-SHA256、域分隔、fail-closed 六条） | 三个 handler |
//! | `Content-Disposition` 的构造与文件名消毒（RFC 5987） | 两条 workspace 加载路径 |
//! | 文本预览白名单（content-type **加**扩展名 **加**无扩展名构建文件名） | 对象字节读取与流式回写 |
//! | `attachment.url → (bucket, key)` 的切分约定 | 状态码 / 错误体 |
//!
//! `download.rs` 用 `pub use` 把这一半**原样再导出** ⇒ 外面（`read.rs` / `delete.rs` /
//! 本片的用例）仍然写 `download::hmac_sha256(…)` 这样的全路径，**没有一个调用点要改**。
//!
//! 上游对应：`server/internal/handler/attachment_capability.go`（能力）与
//! `server/internal/storage/util.go`（处置与文件名）—— 两边都是**无状态**的，
//! 这正是切分线能落在原处的原因。

use sha2::{Digest, Sha256};

use super::download::{
    CAPABILITY_DOWNLOAD_INTENT, CAPABILITY_KEY_DOMAIN, CAPABILITY_SECRET_ENV,
    CAPABILITY_SECRET_FALLBACK_ENV, CAPABILITY_TTL_SECS, CAPABILITY_VERSION,
};

// ---------------------------------------------------------------------------
// 纯函数（可单测、无库）
// ---------------------------------------------------------------------------

/// 能力消息：`v1|{id}|{exp}`，非空 intent 再追加 `|{intent}`。
///
/// 分隔符 `|` 不会出现在 UUID 或十进制时间戳里 ⇒ 任何两组 `(id, exp)` 都不会被
/// 重新切分成另一组 `(id, exp)` 却得到同一条消息（上游注释逐字）。
#[must_use]
pub fn capability_message(attachment_id: &str, exp: i64, intent: &str) -> String {
    if intent.is_empty() {
        format!("{CAPABILITY_VERSION}|{attachment_id}|{exp}")
    } else {
        format!("{CAPABILITY_VERSION}|{attachment_id}|{exp}|{intent}")
    }
}

/// 签一条能力（十六进制 HMAC-SHA256）。
///
/// `key` = `None`（没配根密钥）⇒ **空串**，铸造侧据此优雅降级、**绝不**发可伪造的链接。
#[must_use]
pub fn sign_capability(
    key: Option<&[u8; 32]>,
    attachment_id: &str,
    exp: i64,
    intent: &str,
) -> String {
    let Some(key) = key else {
        return String::new();
    };
    hex::encode(hmac_sha256(
        key,
        capability_message(attachment_id, exp, intent).as_bytes(),
    ))
}

/// 铸造能力 URL（load 意图；空串 = 配不出根密钥 ⇒ 优雅降级）。
#[must_use]
pub fn capability_path(key: Option<&[u8; 32]>, attachment_id: &str, now_unix: i64) -> String {
    let exp = now_unix + CAPABILITY_TTL_SECS;
    let sig = sign_capability(key, attachment_id, exp, "");
    if sig.is_empty() {
        return String::new();
    }
    format!("/api/attachments/{attachment_id}/signed-download?exp={exp}&sig={sig}")
}

/// 铸造「下载按钮」能力 URL（`dl=1`，另一条签名）。
#[must_use]
pub fn download_capability_path(
    key: Option<&[u8; 32]>,
    attachment_id: &str,
    now_unix: i64,
) -> String {
    let exp = now_unix + CAPABILITY_TTL_SECS;
    let sig = sign_capability(key, attachment_id, exp, CAPABILITY_DOWNLOAD_INTENT);
    if sig.is_empty() {
        return String::new();
    }
    format!("/api/attachments/{attachment_id}/signed-download?exp={exp}&sig={sig}&dl=1")
}

/// 校验能力 —— **每条失败路径都 fail-closed**。
///
/// 缺字段 / `exp` 不可解析 / 已过期 / 签名畸形 / 给别的附件签的 / **没配根密钥** ⇒ 全部 `false`。
/// 签名**覆盖**声明的 `exp` ⇒ 把 `exp` 改长只会作废签名，不会延长能力。
#[must_use]
pub fn verify_capability(
    key: Option<&[u8; 32]>,
    attachment_id: &str,
    raw_exp: &str,
    raw_sig: &str,
    intent: &str,
    now_unix: i64,
) -> bool {
    if key.is_none() || attachment_id.is_empty() || raw_exp.is_empty() || raw_sig.is_empty() {
        return false;
    }
    let Ok(exp) = raw_exp.parse::<i64>() else {
        return false;
    };
    if now_unix > exp {
        return false;
    }
    let Some(want) = decode_hex(&sign_capability(key, attachment_id, exp, intent)) else {
        return false;
    };
    let Some(got) = decode_hex(raw_sig) else {
        return false;
    };
    // 定长常量时间比较（`got == want` 的短路径会早退 ⇒ 不能用）。
    if got.len() != want.len() {
        return false;
    }
    got.iter()
        .zip(want.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// 稳定的下载路径（`util.AttachmentDownloadPath`，逐字）。
#[must_use]
pub fn attachment_download_path(attachment_id: &str) -> String {
    format!("/api/attachments/{attachment_id}/download")
}

/// `ContentDisposition`：媒体类型 inline、其余 attachment（上游 `storage/util.go:79-91`）。
#[must_use]
pub fn content_disposition(content_type: &str, filename: &str) -> String {
    let kind = if is_inline_content_type(content_type) {
        "inline"
    } else {
        "attachment"
    };
    if !needs_rfc5987(filename) {
        return format!("{kind}; filename=\"{}\"", sanitize_filename(filename));
    }
    let ascii_fallback = sanitize_filename(&ascii_only_filename(filename));
    format!(
        "{kind}; filename=\"{ascii_fallback}\"; filename*=UTF-8''{}",
        rfc5987_encode(filename)
    )
}

/// `AttachmentContentDisposition`：恒 `attachment`（上游 `storage/util.go:93-99`）。
#[must_use]
pub fn attachment_content_disposition(filename: &str) -> String {
    if !needs_rfc5987(filename) {
        return format!("attachment; filename=\"{}\"", sanitize_filename(filename));
    }
    let ascii_fallback = sanitize_filename(&ascii_only_filename(filename));
    format!(
        "attachment; filename=\"{ascii_fallback}\"; filename*=UTF-8''{}",
        rfc5987_encode(filename)
    )
}

/// 媒体类型（图片 / 视频 / 音频 / PDF）走 inline，其余走 attachment。
///
/// 🔴 **SVG 被显式排除**（上游 `storage/util.go:102-113` 逐字）：`image/svg+xml` 能带
/// `<script>` / `<foreignObject>` / `onload=`，在文档源里内联渲染就是存储型 XSS。
/// 先归一化（trim / 小写 / 去参数）再匹配 —— RFC 2045 §5.1 的类型匹配是大小写不敏感
/// 且允许带参数，那正是这条安全边界。
#[must_use]
pub fn is_inline_content_type(content_type: &str) -> bool {
    let media = normalize_media_type(content_type);
    if media == "image/svg+xml" {
        return false;
    }
    media.starts_with("image/")
        || media.starts_with("video/")
        || media.starts_with("audio/")
        || media == "application/pdf"
}

fn normalize_media_type(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.split_once(';') {
        Some((head, _)) => head.trim().to_owned(),
        None => lower,
    }
}

/// 内联文本预览的白名单（上游 `file.go:1352-1400` 的 `isTextPreviewable`）。
///
/// 同时看 `content_type` **和**扩展名：`http.DetectContentType` 对 Markdown / 源码
/// 经常返回 `text/plain`，只看类型会把它们 415 掉。
#[must_use]
pub fn is_text_previewable(content_type: &str, filename: &str) -> bool {
    let ct = normalize_media_type(content_type);
    if ct.starts_with("text/") {
        return true;
    }
    if matches!(
        ct.as_str(),
        "application/json"
            | "application/javascript"
            | "application/xml"
            | "application/x-yaml"
            | "application/yaml"
            | "application/toml"
            | "application/x-sh"
            | "application/x-httpd-php"
    ) {
        return true;
    }
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext_is_previewable(&ext) {
        return true;
    }
    // 无扩展名但命中常见构建文件名（上游 `file.go:1400` 起）。
    let base = filename.to_ascii_lowercase();
    PREVIEWABLE_BASENAMES.contains(&base.as_str())
}

/// 文本预览白名单的扩展名表（上游逐字；`cn` 在上面单列过，不在此表）。
pub const PREVIEWABLE_EXTENSIONS: [&str; 45] = [
    "md", "markdown", "txt", "log", "csv", "tsv", "html", "htm", "json", "xml", "yml", "yaml",
    "toml", "ini", "conf", "sh", "bash", "zsh", "py", "rb", "go", "rs", "ts", "tsx", "js", "jsx",
    "mjs", "cjs", "css", "scss", "sass", "less", "sql", "java", "kt", "swift", "c", "cc", "cpp",
    "h", "hpp", "cs", "php", "lua", "vim",
];

fn ext_is_previewable(ext: &str) -> bool {
    PREVIEWABLE_EXTENSIONS.contains(&ext)
}

/// 无扩展名但可预览的常见构建文件名（上游 `file.go:1400` 起）。
pub const PREVIEWABLE_BASENAMES: [&str; 6] = [
    "dockerfile",
    "makefile",
    "gnumakefile",
    "cmakelists.txt",
    ".gitignore",
    ".env",
];

/// 把 `attachment.url` 切成 `(bucket, key)`。
///
/// **约定（已登记为偏离，`docs/32` §50）**：URL 的 path（去掉前导 `/`、去掉 query/fragment）
/// 在**第一个 `/`** 处切开 ⇒ 首段 = bucket、余下 = key。它与
/// `mc_storage::local::LocalDiskStorage::path_for`（`root/bucket/key`）**恰好互逆**。
/// 写侧（`POST /api/upload-file`）属 **M10-B2**，本片是读侧；B2 落地时这个约定要一起复核。
#[must_use]
pub fn split_object_ref(url: &str) -> Option<(String, String)> {
    let without_fragment = url.split_once('#').map_or(url, |(head, _)| head);
    let path = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(head, _)| head);
    // 绝对 URL 只取 path；站点相对 URL 整个就是 path。
    let path = match path
        .strip_prefix("http://")
        .or_else(|| path.strip_prefix("https://"))
    {
        Some(rest) => match rest.split_once('/') {
            Some((_host, p)) => p,
            None => "",
        },
        None => path,
    };
    let path = path.trim_start_matches('/');
    let (bucket, key) = path.split_once('/')?;
    let (bucket, key) = (bucket.trim(), key.trim());
    if bucket.is_empty() || key.is_empty() {
        return None;
    }
    Some((bucket.to_owned(), key.to_owned()))
}

// ---------------------------------------------------------------------------
// HMAC-SHA256（RFC 2104，就地用已有的 `sha2` 依赖实现 —— 不加 `hmac` 依赖边）
// ---------------------------------------------------------------------------

/// HMAC-SHA256。`mc-http` 已有 `sha2`（`sha256_etag` 同源）⇒ **零新依赖**。
#[must_use]
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        padded[..32].copy_from_slice(&digest);
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= padded[i];
        opad[i] ^= padded[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    outer.finalize().into()
}

/// 能力签名密钥 = `SHA-256(域前缀 ‖ 根密钥)`（**纯函数**，不读 env —— 可直接单测）。
#[must_use]
pub fn derive_capability_key(root: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CAPABILITY_KEY_DOMAIN.as_bytes());
    hasher.update(root.as_bytes());
    hasher.finalize().into()
}

/// 从**进程环境**取能力签名密钥（只有 handler 走这条；测试全部走 [`derive_capability_key`]）。
///
/// `None` = 两个 env 都没配（或都为空串）⇒ 铸造侧降级、兑换侧 fail-closed（见模块头第 3 条）。
#[must_use]
pub fn capability_key() -> Option<[u8; 32]> {
    let root = [CAPABILITY_SECRET_ENV, CAPABILITY_SECRET_FALLBACK_ENV]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())?;
    Some(derive_capability_key(&root))
}

// ---------------------------------------------------------------------------
// 文件名消毒（上游 `storage/util.go:11-78`，逐字移植）
// ---------------------------------------------------------------------------

/// 去掉会让 `Content-Disposition` 头注入的字符（控制字符 / 换行 / NUL / 引号 / 分号 / 反斜杠）。
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if (c as u32) < 0x20 || c as u32 == 0x7f || matches!(c, '"' | ';' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn ascii_only_filename(name: &str) -> String {
    name.chars()
        .map(|c| if (c as u32) > 0x7f { '_' } else { c })
        .collect()
}

fn needs_rfc5987(name: &str) -> bool {
    name.chars().any(|c| (c as u32) > 0x7f)
}

fn rfc5987_encode(name: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(name.len() * 3);
    for b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(b) {
            out.push(char::from(*b));
        } else {
            // `write!` 到 String 永不失败（String 的 fmt::Write 实现吞掉 Err）。
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn decode_hex(raw: &str) -> Option<Vec<u8>> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    hex::decode(raw).ok()
}
