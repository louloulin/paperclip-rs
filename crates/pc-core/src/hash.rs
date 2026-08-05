//! Hash utilities shared across crates.
//!
//! 高内聚：所有 hash 算法集中此处，避免散落实现。
//! 低耦合：仅依赖 sha2 + hex，不依赖任何 IO。

use sha2::{Digest, Sha256};

/// 计算 SHA256 并返回 lowercase hex 字符串。
///
/// 与 `pc_auth::hash_token` 行为一致（hex(sha256(bytes))），
/// 统一在此处以便仓储层无需依赖 `pc-auth`（pc-repos 不能依赖 pc-auth）。
pub fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(digest)
}

/// 常数时间字节比较，防御侧信道泄漏。
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_known_vector() {
        // SHA256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // SHA256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
