//! 两种验签方案 —— 上游 `vcs/forgejo.go`（HMAC-SHA256）与 `vcs/gitlab.go`（明文 token）的
//! 共用实现（M8-0 anchor 落**契约与常量时间原语**，provider 侧接线归 M8-2）。
//!
//! # 三种签名方案（`docs/61` §1.6 / §2.7 第 3 条）
//!
//! | 方案 | provider | 头 | 比较 |
//! | --- | --- | --- | --- |
//! | HMAC-SHA256 | GitHub（**不在本 crate**） | `X-Hub-Signature-256` | 常量时间 |
//! | HMAC-SHA256 | Forgejo / Gitea | `X-Gitea-Signature` / `X-Forgejo-Signature` | 常量时间 |
//! | 明文 token | GitLab | `X-Gitlab-Token` | **常量时间**（上游用 `==`，本仓刻意收紧，登记 `docs/32` §9.12） |
//!
//! ⚠️ 反例测试是硬要求：签名差 1 位必失败、正确签名必通过（`docs/61` §2.7 / §6.5 的 M8-2 行）。

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// 计算 `HMAC-SHA256(secret, body)` 的**小写十六进制**（无前缀）。
pub fn hmac_sha256_hex(secret: &str, body: &[u8]) -> String {
    // `new_from_slice` 对任意长度密钥都不会失败（HMAC 的键长无约束）。
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// 常量时间比较两个字节串（长度不等立即返回 `false`；相等长度下无提前返回）。
///
/// 这是 GitLab 明文 token 比较与十六进制签名比较的共用原语。**不要**用 `==` 替代：
/// `String`/`[u8]` 的 `PartialEq` 是提前返回的，会泄漏逐字节匹配长度
/// （`docs/61` §2.7 第 3 条）。
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 校验 HMAC-SHA256 签名（十六进制；可带 `sha256=` 前缀，上游 GitHub 带、Forgejo 不带）。
///
/// 用 `hmac` 的 [`Mac::verify_slice`] 而不是先 `hex::encode` 再 `==`：前者内部就是常量时间。
/// 非法十六进制 ⇒ `false`（**不是** panic）。
pub fn verify_hmac_sha256_hex(secret: &str, body: &[u8], signature_hex: &str) -> bool {
    let raw = signature_hex
        .strip_prefix("sha256=")
        .unwrap_or(signature_hex);
    let Ok(expected) = hex::decode(raw) else {
        return false;
    };
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// 校验 GitLab 的 `X-Gitlab-Token` 明文 token（常量时间）。
pub fn verify_plaintext_token(secret: &str, presented: &str) -> bool {
    constant_time_eq(secret.as_bytes(), presented.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_roundtrip_and_one_bit_off() {
        let body = br#"{"action":"opened"}"#;
        let good = hmac_sha256_hex("s3cr3t", body);
        assert!(verify_hmac_sha256_hex("s3cr3t", body, &good));
        // `sha256=` 前缀（GitHub 形态）也要通过。
        assert!(verify_hmac_sha256_hex(
            "s3cr3t",
            body,
            &format!("sha256={good}")
        ));

        // 反例一：签名差 1 位字符。
        let mut flipped = good.clone();
        let last = flipped.pop().unwrap();
        flipped.push(if last == '0' { '1' } else { '0' });
        assert!(!verify_hmac_sha256_hex("s3cr3t", body, &flipped));

        // 反例二：body 差 1 字节。
        let mut other = body.to_vec();
        other[0] = b'{';
        other.push(b' ');
        assert!(!verify_hmac_sha256_hex("s3cr3t", &other, &good));

        // 反例三：非法十六进制不 panic。
        assert!(!verify_hmac_sha256_hex("s3cr3t", body, "zzzz"));
    }

    #[test]
    fn plaintext_token_is_constant_time_and_exact() {
        assert!(verify_plaintext_token("tok-abc", "tok-abc"));
        assert!(!verify_plaintext_token("tok-abc", "tok-abd"));
        assert!(!verify_plaintext_token("tok-abc", "tok-abc-longer"));
        assert!(!verify_plaintext_token("tok-abc", ""));
    }

    #[test]
    fn constant_time_eq_matches_equality_semantics() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
