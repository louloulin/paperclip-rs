#![forbid(unsafe_code)]

//! Webhook signature verification pure helpers — 1:1 port of
//! paperclip/server/src/services/routines.ts::verifyWebhookSignature (signature 部分).
//!
//! R740: 零 DB 校验 helpers（HMAC-SHA256 + timestamp replay window + hex decode + constant-time compare）。

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Webhook signature header prefix — “t=<unix_ms>,v1=<hex>”.
pub const WEBHOOK_SIG_HEADER: &str = "webhook-signature";

/// 默认 replay window 秒数（与 Node routines 默认 300 秒对齐）。
pub const DEFAULT_REPLAY_WINDOW_SEC: i32 = 300;

/// 计算 HMAC-SHA256(key, payload) → lowercase hex。
pub fn hmac_sha256_hex(key: &[u8], payload: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    mac.update(payload);
    let bytes = mac.finalize().into_bytes();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time 字节数组相等性判定。
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// Hex string → Vec<u8>。奇数长度返回 None。
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

/// 单个 hex 字符 → 4-bit nibble。
pub fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 解析 webhook signature header “t=<ts>,v1=<sig>” → (ts, sig_hex)。
pub fn parse_webhook_signature_header(header: &str) -> Option<(i64, String)> {
    let mut ts: Option<i64> = None;
    let mut sig_hex: Option<String> = None;
    for part in header.split(',') {
        let part = part.trim();
        let part_trimmed = part.trim();
        if let Some(rest) = part_trimmed.strip_prefix("t=") {
            ts = rest.trim().parse().ok();
        } else if let Some(rest) = part_trimmed.strip_prefix("v1=") {
            sig_hex = Some(rest.trim().to_string());
        }
    }
    match (ts, sig_hex) {
        (Some(t), Some(s)) => Some((t, s)),
        _ => None,
    }
}

/// Webhook signature 验证结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookSignatureError {
    MissingField,
    ReplayWindowExceeded,
    HexDecodeFailed,
    SignatureMismatch,
}

impl WebhookSignatureError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingField => "missing_field",
            Self::ReplayWindowExceeded => "replay_window_exceeded",
            Self::HexDecodeFailed => "hex_decode_failed",
            Self::SignatureMismatch => "signature_mismatch",
        }
    }
}

/// 校验 webhook signature（纯逻辑版本，不依赖 DB）。
///
/// payload 由 “t=<ts>.<raw_body>” 拼接。
pub fn verify_webhook_signature_pure(
    signature_header: &str,
    raw_body: &[u8],
    secret_value: &[u8],
    now_unix_ms: i64,
    replay_window_sec: i32,
) -> Result<(), WebhookSignatureError> {
    let (ts, sig_hex) = parse_webhook_signature_header(signature_header)
        .ok_or(WebhookSignatureError::MissingField)?;
    let delta = (now_unix_ms - ts).abs();
    if delta > i64::from(replay_window_sec) * 1000 {
        return Err(WebhookSignatureError::ReplayWindowExceeded);
    }
    let mut payload = Vec::with_capacity(32 + raw_body.len());
    payload.extend_from_slice(ts.to_string().as_bytes());
    payload.push(b'.');
    payload.extend_from_slice(raw_body);
    let expected_hex = hmac_sha256_hex(secret_value, &payload);
    let expected_bytes = hex_decode(&expected_hex).ok_or(WebhookSignatureError::HexDecodeFailed)?;
    let provided = hex_decode(&sig_hex).ok_or(WebhookSignatureError::HexDecodeFailed)?;
    if constant_time_eq(&expected_bytes, &provided) {
        Ok(())
    } else {
        Err(WebhookSignatureError::SignatureMismatch)
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn hmac_sha256_hex_known_vector() {
        // RFC 4231 test case 1 — key = 0x0b * 20, data = "Hi There"
        let key = vec![0x0b; 20];
        let expected = "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7";
        assert_eq!(hmac_sha256_hex(&key, b"Hi There"), expected);
    }

    #[test]
    fn constant_time_eq_same() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn constant_time_eq_different() {
        assert!(!constant_time_eq(b"hello", b"world"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"hello", b"hell")); // length 不同直接不等
    }

    #[test]
    fn hex_decode_even_length() {
        assert_eq!(hex_decode("aabbcc"), Some(vec![0xaa, 0xbb, 0xcc]));
    }

    #[test]
    fn hex_decode_odd_length_none() {
        assert_eq!(hex_decode("aab"), None);
    }

    #[test]
    fn hex_decode_uppercase() {
        assert_eq!(hex_decode("AABB"), Some(vec![0xaa, 0xbb]));
    }

    #[test]
    fn hex_decode_invalid_char() {
        assert_eq!(hex_decode("zz"), None);
    }

    #[test]
    fn hex_nibble_basic() {
        assert_eq!(hex_nibble(b'0'), Some(0));
        assert_eq!(hex_nibble(b'9'), Some(9));
        assert_eq!(hex_nibble(b'a'), Some(10));
        assert_eq!(hex_nibble(b'f'), Some(15));
        assert_eq!(hex_nibble(b'A'), Some(10));
        assert_eq!(hex_nibble(b'g'), None);
        assert_eq!(hex_nibble(b' '), None);
    }

    #[test]
    fn parse_webhook_header_valid() {
        let h = "t=1700000000,v1=aabbcc";
        assert_eq!(parse_webhook_signature_header(h), Some((1700000000, "aabbcc".into())));
    }

    #[test]
    fn parse_webhook_header_with_spaces() {
        let h = "t=1700000000, v1=aabbcc"; // extra space after , is OK (after trim)
        assert_eq!(parse_webhook_signature_header(h), Some((1700000000, "aabbcc".into())));
    }

    #[test]
    fn parse_webhook_header_missing_t() {
        assert_eq!(parse_webhook_signature_header("v1=aabbcc"), None);
    }

    #[test]
    fn parse_webhook_header_missing_v1() {
        assert_eq!(parse_webhook_signature_header("t=1700000000"), None);
    }

    #[test]
    fn verify_webhook_signature_valid() {
        let key = b"secret-key";
        let body = b"{\"foo\":1}";
        let ts: i64 = 1700000000;
        let mut payload = Vec::new();
        payload.extend_from_slice(ts.to_string().as_bytes());
        payload.push(b'.');
        payload.extend_from_slice(body);
        let sig = hmac_sha256_hex(key, &payload);
        let header = format!("t={ts},v1={sig}");
        assert!(verify_webhook_signature_pure(&header, body, key, ts, 300).is_ok());
    }

    #[test]
    fn verify_webhook_signature_replay_window() {
        let key = b"k";
        let body = b"{}";
        let ts: i64 = 1700000000;
        let now = ts + 600_000; // 10 分钟超出默认 5 分钟 window
        let mut payload = Vec::new();
        payload.extend_from_slice(ts.to_string().as_bytes());
        payload.push(b'.');
        payload.extend_from_slice(body);
        let sig = hmac_sha256_hex(key, &payload);
        let header = format!("t={ts},v1={sig}");
        assert_eq!(
            verify_webhook_signature_pure(&header, body, key, now, 300),
            Err(WebhookSignatureError::ReplayWindowExceeded)
        );
    }

    #[test]
    fn verify_webhook_signature_mismatch() {
        let header = "t=1700000000,v1=deadbeef";
        assert_eq!(
            verify_webhook_signature_pure(header, b"body", b"key", 1700000000, 300),
            Err(WebhookSignatureError::SignatureMismatch)
        );
    }

    #[test]
    fn verify_webhook_signature_missing_field() {
        assert_eq!(
            verify_webhook_signature_pure("v1=aabb", b"body", b"k", 0, 300),
            Err(WebhookSignatureError::MissingField)
        );
    }

    #[test]
    fn verify_webhook_signature_hex_decode_failed() {
        let header = "t=1700000000,v1=zz";
        assert_eq!(
            verify_webhook_signature_pure(header, b"body", b"k", 1700000000, 300),
            Err(WebhookSignatureError::HexDecodeFailed)
        );
    }

    #[test]
    fn error_as_str() {
        assert_eq!(WebhookSignatureError::MissingField.as_str(), "missing_field");
        assert_eq!(WebhookSignatureError::SignatureMismatch.as_str(), "signature_mismatch");
    }
}
