//! `DingTalk` 媒体的**出口守卫与纯判据**：公网地址判定、URL 形状校验、内容类型嗅探
//! （上游 `media.go` 里 `newMediaHTTPClient` / `publicDownloadDialer` / `validateDownloadURL` /
//! `isPublicDownloadAddress` 那几段）。
//!
//! - **写者**：M7-8（`docs/32` §22 的 D1；门 ⑩ 的切分，边界取上游那几段函数的边界）。
//! - **本文件是不可信出口策略的**唯一**实现点**：调用方（`media.rs` 的取回器与重定向策略）
//!   只调 [`guard_download_url`] / [`is_public_download_address`]，不自己拼判据。
//! - **凭据面**：本文件没有任何 URL / 下载码 / 凭据字段，错误变体也不带。

use reqwest::Url;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::dingtalk::media::MAX_INBOUND_IMAGE_BYTES;

// =====================================================================
// 地址判据（纯函数）
// =====================================================================

/// 一个网络前缀（`addr` 的高 `bits` 位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    addr: IpAddr,
    bits: u8,
}

const fn prefix(address: IpAddr, bits: u8) -> Prefix {
    Prefix {
        addr: address,
        bits,
    }
}

const fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

const fn v6(segments: [u16; 8]) -> IpAddr {
    IpAddr::V6(Ipv6Addr::new(
        segments[0],
        segments[1],
        segments[2],
        segments[3],
        segments[4],
        segments[5],
        segments[6],
        segments[7],
    ))
}

/// "绝不允许下载"的前缀表（上游 `nonPublicDownloadPrefixes` **逐条**，加上判定公网单播所需的
/// 几条：私网 / 回环 / 链路本地 / 组播 / 未指定 / 广播）。
pub const NON_PUBLIC_PREFIXES: &[Prefix] = &[
    prefix(v4(0, 0, 0, 0), 8),
    prefix(v4(10, 0, 0, 0), 8),
    prefix(v4(100, 64, 0, 0), 10),
    prefix(v4(127, 0, 0, 0), 8),
    prefix(v4(169, 254, 0, 0), 16),
    prefix(v4(172, 16, 0, 0), 12),
    prefix(v4(192, 0, 0, 0), 24),
    prefix(v4(192, 0, 2, 0), 24),
    prefix(v4(192, 168, 0, 0), 16),
    prefix(v4(198, 18, 0, 0), 15),
    prefix(v4(198, 51, 100, 0), 24),
    prefix(v4(203, 0, 113, 0), 24),
    prefix(v4(224, 0, 0, 0), 4),
    prefix(v4(240, 0, 0, 0), 4),
    prefix(v4(255, 255, 255, 255), 32),
    // 上游的 IPv6 段（按上游顺序逐条）：
    prefix(v6([0, 0, 0, 0, 0, 0, 0, 0]), 96), // ::/96（废弃的 IPv4 兼容）
    prefix(v6([0x64, 0xff9b, 1, 0, 0, 0, 0, 0]), 48), // 64:ff9b:1::/48（本地用 NAT64）
    prefix(v6([0x100, 0, 0, 0, 0, 0, 0, 0]), 64), // 100::/64（discard-only）
    prefix(v6([0x2001, 0, 0, 0, 0, 0, 0, 0]), 32), // 2001::/32（Teredo）
    prefix(v6([0x2001, 2, 0, 0, 0, 0, 0, 0]), 48), // 2001:2::/48（benchmarking）
    prefix(v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0]), 32),
    prefix(v6([0x2001, 0x10, 0, 0, 0, 0, 0, 0]), 28), // 2001:10::/28（废弃 ORCHID）
    prefix(v6([0x2001, 0x20, 0, 0, 0, 0, 0, 0]), 28), // 2001:20::/28（ORCHIDv2）
    prefix(v6([0x2002, 0, 0, 0, 0, 0, 0, 0]), 16),    // 2002::/16（6to4）
    prefix(v6([0x3fff, 0, 0, 0, 0, 0, 0, 0]), 20),    // 3fff::/20（文档）
    // 公网单播判定还要挡住的：
    prefix(v6([0xfe80, 0, 0, 0, 0, 0, 0, 0]), 10), // 链路本地
    prefix(v6([0xfc00, 0, 0, 0, 0, 0, 0, 0]), 7),  // 唯一本地
    prefix(v6([0xff00, 0, 0, 0, 0, 0, 0, 0]), 8),  // 组播
    prefix(v6([0, 0, 0, 0, 0, 0, 0, 1]), 128),     // ::1
];

/// 标准 NAT64 前缀（上游 `wellKnownNAT64Prefix`）。
const WELL_KNOWN_NAT64: Prefix = prefix(v6([0x64, 0xff9b, 0, 0, 0, 0, 0, 0]), 96);

/// 前缀是否包含这个地址。
fn prefix_contains(candidate: Prefix, address: IpAddr) -> bool {
    match (candidate.addr, address) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            let bits = u32::from(network) ^ u32::from(address);
            candidate.bits == 0 || bits.leading_zeros() >= u32::from(candidate.bits)
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => {
            let bits = u128::from(network) ^ u128::from(address);
            candidate.bits == 0 || bits.leading_zeros() >= u32::from(candidate.bits)
        }
        _ => false,
    }
}

/// 这个地址允许作为下载目标吗（上游 `isPublicDownloadAddress`）。
///
/// IPv4-mapped 的 IPv6 先解包；标准 NAT64 前缀里的合成地址按它的低 32 位 IPv4 再判一遍
/// （上游逐字：攻击者控制的 AAAA 记录不能借这个前缀把回环 / 内网塞进来）。
#[must_use]
pub fn is_public_download_address(address: IpAddr) -> bool {
    let address = unmap(address);
    if let IpAddr::V6(v6_address) = address {
        if prefix_contains(WELL_KNOWN_NAT64, address) {
            let segments = v6_address.segments();
            let low = [
                u8::try_from(segments[6] >> 8).unwrap_or(0),
                u8::try_from(segments[6] & 0xff).unwrap_or(0),
                u8::try_from(segments[7] >> 8).unwrap_or(0),
                u8::try_from(segments[7] & 0xff).unwrap_or(0),
            ];
            return is_public_download_address(IpAddr::V4(Ipv4Addr::from(low)));
        }
    }
    !NON_PUBLIC_PREFIXES
        .iter()
        .any(|candidate| prefix_contains(*candidate, address))
}

/// IPv4-mapped 的 IPv6 解包成 IPv4（上游 `Addr.Unmap()`）。
#[must_use]
pub fn unmap(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6_address) => match v6_address.to_ipv4_mapped() {
            Some(v4_address) => IpAddr::V4(v4_address),
            None => IpAddr::V6(v6_address),
        },
        other @ IpAddr::V4(_) => other,
    }
}

/// 同源判定（scheme + host，大小写不敏感；上游 `sameDownloadOrigin`）。
#[must_use]
pub fn same_download_origin(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left
            .host_str()
            .unwrap_or_default()
            .eq_ignore_ascii_case(right.host_str().unwrap_or_default())
        && left.port_or_known_default() == right.port_or_known_default()
}

// =====================================================================
// URL 校验与错误
// =====================================================================

/// 取回 / 校验 / 上传失败（**不带 URL、不带下载码**：两者都是短期凭据）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// URL 形状不合法（无 host / 带 userinfo / 带 fragment / 不是绝对 URL）。
    #[error("dingtalk media: invalid download URL shape")]
    InvalidUrl,
    /// 既不是 `http` 也不是 `https`。
    #[error("dingtalk media: unsupported download URL scheme")]
    UnsupportedScheme,
    /// `https → http` 的降级重定向。
    #[error("dingtalk media: disallowed HTTPS download redirect downgrade")]
    DowngradeRedirect,
    /// 跨源的 `http` 重定向（票据会跟着走）。
    #[error("dingtalk media: disallowed cross-origin HTTP download redirect")]
    CrossOriginRedirect,
    /// 重定向超过 [`MAX_DOWNLOAD_REDIRECTS`] 跳。
    #[error("dingtalk media: too many redirects")]
    TooManyRedirects,
    /// 目标里有非公网地址（DNS 重绑定 / 直连内网）。
    #[error("dingtalk media: blocked non-public download target")]
    BlockedAddress,
    /// 解析不出去。
    #[error("dingtalk media: resolve download target failed")]
    Resolve,
    /// 链路失败（**不带 URL**）。
    #[error("dingtalk media: download failed")]
    Transport,
    /// 读响应体失败。
    #[error("dingtalk media: read failed")]
    Read,
    /// 非 2xx。
    #[error("dingtalk media: http {status}")]
    Http { status: u16 },
    /// 超过 [`MAX_INBOUND_IMAGE_BYTES`]。
    #[error("dingtalk media: image exceeds the {} MiB limit", MAX_INBOUND_IMAGE_BYTES >> 20)]
    TooLarge,
    /// 嗅出来的类型不在白名单里。
    #[error("dingtalk media: disallowed content type {content_type}")]
    DisallowedContentType { content_type: String },
    /// 对象存储失败。
    #[error("dingtalk media: upload failed")]
    Storage,
    /// 意图账本失败。
    #[error("dingtalk media: record media intent failed")]
    Ledger,
    /// 平台没给出可用的下载码。
    #[error("dingtalk media: no usable download code")]
    NoDownloadCode,
}

/// 校验一个下载 URL 的**形状**（上游 `validateDownloadURL`）。
///
/// # Errors
///
/// 见 [`MediaError`]。
pub fn validate_download_url(parsed: &Url) -> Result<(), MediaError> {
    if parsed.host_str().unwrap_or_default().is_empty()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(MediaError::InvalidUrl);
    }
    if !parsed.scheme().eq_ignore_ascii_case("http")
        && !parsed.scheme().eq_ignore_ascii_case("https")
    {
        return Err(MediaError::UnsupportedScheme);
    }
    Ok(())
}

/// 形状 **+ 字面量地址** 校验（下载守卫的入口判据）。
///
/// `reqwest` 的 `dns_resolver` **只对域名**生效（host 已经是 IP 字面量时它根本不解析），
/// 所以字面量这一步必须自己做；域名那一步由 [`PublicOnlyResolver`] 兜住
/// （差异登记 `docs/32` §22 的 D5）。
///
/// # Errors
///
/// 见 [`validate_download_url`]；host 是**非公网** IP 字面量 ⇒ [`MediaError::BlockedAddress`]。
pub fn guard_download_url(parsed: &Url) -> Result<(), MediaError> {
    validate_download_url(parsed)?;
    if is_blocked_literal_host(parsed) {
        return Err(MediaError::BlockedAddress);
    }
    Ok(())
}

/// host 是**非公网** IP 字面量吗（IPv6 字面量在 URL 里带方括号 ⇒ 先剥掉）。
fn is_blocked_literal_host(parsed: &Url) -> bool {
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<IpAddr>()
        .is_ok_and(|address| !is_public_download_address(address))
}

/// 按**魔数**嗅出白名单里的图片类型（上游 `http.DetectContentType` + 白名单收窄）。
///
/// 只认这五种的签名 ⇒ 比 `DetectContentType` 更严（它会把别的类型也认出来，而调用方随后
/// 一律拒掉）。
#[must_use]
pub fn sniff_image_content_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if data.starts_with(b"BM") {
        return Some("image/bmp");
    }
    None
}
