//! 通用哈希工具。

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HashAlgo {
    Sha256,
    Sha512,
}

/// 内容哈希值（hex 编码）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn sha256(data: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(data);
        Self(hex::encode(h.finalize()))
    }

    pub fn sha512(data: &[u8]) -> Self {
        let mut h = Sha512::new();
        h.update(data);
        Self(hex::encode(h.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_sha256(&self) -> bool {
        self.0.len() == 64 && self.0.chars().all(|c| c.is_ascii_hexdigit())
    }
}

/// HMAC-SHA256 输出。
pub fn hmac_sha256(secret: &[u8], message: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(message);
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

/// HMAC-SHA256 输出（base64）。
pub fn hmac_sha256_b64(secret: &[u8], message: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(message);
    let result = mac.finalize();
    base64::engine::general_purpose::STANDARD.encode(result.into_bytes())
}

/// 验证 HMAC-SHA256 是否匹配（常量时间）。
pub fn hmac_sha256_verify(secret: &[u8], message: &[u8], expected_hex: &str) -> bool {
    let actual = hmac_sha256(secret, message);
    if actual.len() != expected_hex.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in actual.bytes().zip(expected_hex.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        let h = ContentHash::sha256(b"hello");
        // sha256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            h.as_str(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(h.is_sha256());
    }

    #[test]
    fn hmac_round_trip() {
        let secret = b"topsecret";
        let message = b"hello world";
        let mac = hmac_sha256(secret, message);
        assert!(hmac_sha256_verify(secret, message, &mac));
        assert!(!hmac_sha256_verify(b"other", message, &mac));
    }
}