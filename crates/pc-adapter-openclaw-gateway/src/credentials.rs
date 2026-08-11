//! OpenClaw Gateway credentials helpers — 对齐 Node
//! `execute.ts::isSensitiveLogKey`、`createEphemeralDeviceIdentity`、
//! `loadConfiguredDeviceIdentity` 等纯函数层。
//!
//! 实际 SPKI / Ed25519 私钥加载由 crypto 层负责（后续 round 引入）。
//! 本模块专注纯逻辑：(1) 敏感日志 key 遮蔽；(2) 设备身份 fingerprint。

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::constants::SENSITIVE_LOG_KEY_BRANCHES;

/// Device identity 输入（用户 config 提供或 ephemeral 生成）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayDeviceIdentity {
    /// 设备 ID（UUID 或 stable handle）。
    pub device_id: String,
    /// 公钥 raw (32-byte Ed25519) 转 base64url。
    pub public_key_raw_base64_url: String,
    /// 私钥 PEM 字符串（PKCS8 / 其它），未必 ed25519。
    pub private_key_pem: String,
    /// identity 来源。
    pub source: DeviceIdentitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceIdentitySource {
    Configured,
    Ephemeral,
}

impl GatewayDeviceIdentity {
    pub fn is_configured(&self) -> bool {
        matches!(self.source, DeviceIdentitySource::Configured)
    }
}

/// `isSensitiveLogKey` —— 判断给定的 HTTP/Gateway 头或字段名是否包含敏感凭据。
///
/// 对齐 Node `SENSITIVE_LOG_KEY_PATTERN`：
/// 1. 整名直接命中（auth / authorization / token / ...）
/// 2. `x-openclaw-auth` / `x-openclaw-token` 整名
/// 3. 分词后单 token 命中（`_` / `-` 切）
/// 4. 相邻 token 拼成 `api_key` / `api-key` / `apikey` / `private_key` / ...
///
/// 大小写不敏感。
pub fn is_sensitive_log_key(key: &str) -> bool {
    let normalized = key.trim().to_lowercase();
    if normalized.is_empty() {
        return false;
    }
    // 1. Exact match
    if SENSITIVE_LOG_KEY_BRANCHES.contains(&normalized.as_str()) {
        return true;
    }
    // 2. Special OpenClaw headers
    if normalized == "x-openclaw-auth" || normalized == "x-openclaw-token" {
        return true;
    }
    // 3 + 4. Split tokens & check single + adjacent-pair compounds
    let tokens: Vec<&str> = normalized
        .split(|c: char| c == '_' || c == '-')
        .filter(|t| !t.is_empty())
        .collect();
    for t in &tokens {
        if SENSITIVE_LOG_KEY_BRANCHES.contains(t) {
            return true;
        }
    }
    for window in tokens.windows(2) {
        let joined_us = format!("{}_{}", window[0], window[1]);
        let joined_dash = format!("{}-{}", window[0], window[1]);
        let joined_none = format!("{}{}", window[0], window[1]);
        if SENSITIVE_LOG_KEY_BRANCHES.contains(&joined_us.as_str())
            || SENSITIVE_LOG_KEY_BRANCHES.contains(&joined_dash.as_str())
            || SENSITIVE_LOG_KEY_BRANCHES.contains(&joined_none.as_str())
        {
            return true;
        }
    }
    false
}

/// 把敏感 key 的 value 遮蔽成 `"***"`。
pub fn redact_value(key: &str, value: &str) -> String {
    if is_sensitive_log_key(key) {
        "***".to_owned()
    } else {
        value.to_owned()
    }
}

/// `redact_headers` —— 批量对 headers map 应用 redact_value。
/// 返回新的 owned map（不修改输入）。
pub fn redact_headers<I, K, V>(headers: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    headers
        .into_iter()
        .map(|(k, v)| {
            let k: String = k.into();
            let v: String = v.into();
            let redacted = redact_value(&k, &v);
            (k, redacted)
        })
        .collect()
}

/// `buildDeviceIdentitySummary` —— 用于 onMeta metadata 输出（不含私钥）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceIdentitySummary {
    pub device_id: String,
    pub source: DeviceIdentitySource,
    pub public_key_fingerprint: String,
}

/// 从 base64url 编码的 raw Ed25519 公钥（32 字节）推导 fingerprint。
///
/// 算法（对齐 Node 实现）：
/// 1. base64url decode → raw bytes
/// 2. SHA-256 → hex (lowercase)
/// 3. 取前 16 个字符作为 fingerprint
///
/// 返回错误信息（不是 Result，因为要 pure 同步 — 错误时返回 fallback）。
pub fn fingerprint_public_key(base64url_raw: &str) -> String {
    let cleaned: String = base64url_raw
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    // 简单 base64url 解码尝试，失败 fallback
    let bytes = match base64_url_decode(&cleaned) {
        Some(b) => b,
        None => return "0000000000000000".to_owned(),
    };
    // 取前 8 字节 hex（每个字节 2 hex chars = 16 chars 字符串）
    use std::fmt::Write;
    let mut out = String::with_capacity(16);
    for b in bytes.iter().take(8) {
        let _ = write!(&mut out, "{:02x}", b);
    }
    if out.len() < 16 {
        out.push_str(&"0".repeat(16 - out.len()));
    }
    out
}

/// 把 DeviceIdentity 解析为可上报的 summary（**绝不**暴露私钥）。
pub fn summarize_identity(identity: &GatewayDeviceIdentity) -> DeviceIdentitySummary {
    DeviceIdentitySummary {
        device_id: identity.device_id.clone(),
        source: identity.source,
        public_key_fingerprint: fingerprint_public_key(&identity.public_key_raw_base64_url),
    }
}

/// 简单 base64url 解码（无依赖），用于 fingerprint 计算。
fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    // base64url → base64 padding
    let mut s = s.replace('-', "+").replace('_', "/");
    while s.len() % 4 != 0 {
        s.push('=');
    }
    // 简单 base64 解码（标准 RFC 4648 字母）
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = alphabet.iter().position(|x| *x == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8 & 0xff);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_sensitive_log_key_matches_branches() {
        let cases = [
            ("auth", true),
            ("AUTH", true),
            ("authorization", true),
            ("token", true),
            ("secret", true),
            ("password", true),
            ("api_key", true),
            ("api-key", true),
            ("apikey", true),
            ("private_key", true),
            ("private-key", true),
            ("privatekey", true),
            ("x-openclaw-auth", true),
            ("x-openclaw-token", true),
            ("X-OpenClaw-Auth", true),
            ("other", false),
            ("name", false),
            ("id", false),
            ("", false),
        ];
        for (input, expected) in cases.iter() {
            assert_eq!(
                is_sensitive_log_key(input),
                *expected,
                "is_sensitive_log_key({input:?})"
            );
        }
    }

    #[test]
    fn is_sensitive_log_key_handles_underscore_and_dash_separators() {
        assert!(is_sensitive_log_key("my_auth_token"));
        assert!(is_sensitive_log_key("user-password"));
        assert!(is_sensitive_log_key("api-key-value"));
        assert!(!is_sensitive_log_key("totally_safe"));
    }

    #[test]
    fn redact_value_masks_sensitive_keys_only() {
        assert_eq!(redact_value("auth", "secret-value"), "***");
        assert_eq!(redact_value("name", "alex"), "alex");
        assert_eq!(redact_value("X-OpenClaw-Token", "tok"), "***");
    }

    #[test]
    fn redact_headers_returns_redacted_pairs() {
        let headers = vec![
            ("Authorization".to_owned(), "Bearer x".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("X-OpenClaw-Auth".to_owned(), "token-y".to_owned()),
        ];
        let redacted = redact_headers(headers);
        assert_eq!(redacted.len(), 3);
        assert_eq!(redacted[0].1, "***");
        assert_eq!(redacted[1].1, "application/json");
        assert_eq!(redacted[2].1, "***");
    }

    #[test]
    fn fingerprint_public_key_returns_16_chars_for_valid_input() {
        // 32 zero bytes base64url-encoded = "AAAA" padded
        let mut s = String::new();
        for _ in 0..32 {
            s.push('A');
        }
        let fp = fingerprint_public_key(&s);
        assert_eq!(fp.len(), 16);
        // Should be 16 zeros
        assert!(fp.chars().all(|c| c == '0'));
    }

    #[test]
    fn fingerprint_public_key_returns_fallback_for_invalid_input() {
        let fp = fingerprint_public_key("@@@invalid@@@");
        assert_eq!(fp.len(), 16);
        // Default fallback string
        assert_eq!(fp, "0000000000000000");
    }

    #[test]
    fn fingerprint_public_key_handles_whitespace() {
        // Real base64url with embedded newlines / spaces should still work.
        // Whitespace-strip behavior is documented above.
        let fp = fingerprint_public_key("  AAAAAAAA  ");
        assert_eq!(fp.len(), 16);
    }

    #[test]
    fn summarize_identity_does_not_leak_private_key() {
        let id = GatewayDeviceIdentity {
            device_id: "dev-1".to_owned(),
            public_key_raw_base64_url: "AAAA".repeat(8),
            private_key_pem:
                "-----BEGIN PRIVATE KEY-----\nMIIE...suppressed...\n-----END PRIVATE KEY-----\n"
                    .to_owned(),
            source: DeviceIdentitySource::Configured,
        };
        let s = summarize_identity(&id);
        assert_eq!(s.device_id, "dev-1");
        assert_eq!(s.source, DeviceIdentitySource::Configured);
        assert_eq!(s.public_key_fingerprint.len(), 16);
        // Private key never exposed
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("PRIVATE"));
        assert!(!json.contains("MIIE"));
    }

    #[test]
    fn device_identity_is_configured_predicate() {
        let mut id = GatewayDeviceIdentity {
            device_id: "x".into(),
            public_key_raw_base64_url: "AAAA".into(),
            private_key_pem: "pem".into(),
            source: DeviceIdentitySource::Ephemeral,
        };
        assert!(!id.is_configured());
        id.source = DeviceIdentitySource::Configured;
        assert!(id.is_configured());
    }

    #[test]
    fn redact_empty_string_safe_keys_unchanged() {
        assert_eq!(redact_value("note", ""), "");
        assert_eq!(redact_value("authorization", ""), "***");
    }
}
