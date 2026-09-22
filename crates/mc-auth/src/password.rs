//! 密码哈希（PBKDF2-HMAC-SHA256 + per-password salt）。

use base64::Engine;
use rand::RngCore;
use sha2::Sha256;

const PBKDF2_ITERATIONS: u32 = 100_000;
const PBKDF2_KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;

/// 编码后：`pbkdf2$<iter>$<salt_b64>$<hash_b64>`。
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);

    let hash = pbkdf2_sha256(
        password.as_bytes(),
        &salt,
        PBKDF2_ITERATIONS,
        PBKDF2_KEY_LEN,
    );
    format!(
        "pbkdf2${}${}${}",
        PBKDF2_ITERATIONS,
        base64::engine::general_purpose::STANDARD.encode(salt),
        base64::engine::general_purpose::STANDARD.encode(hash)
    )
}

/// 常量时间比较。
pub fn verify_password(password: &str, encoded: &str) -> bool {
    let parts: Vec<&str> = encoded.split('$').collect();
    if parts.len() != 4 || parts[0] != "pbkdf2" {
        return false;
    }
    let Ok(iterations) = parts[1].parse::<u32>() else {
        return false;
    };
    let Ok(salt) = base64::engine::general_purpose::STANDARD.decode(parts[2]) else {
        return false;
    };
    let Ok(expected) = base64::engine::general_purpose::STANDARD.decode(parts[3]) else {
        return false;
    };
    let actual = pbkdf2_sha256(password.as_bytes(), &salt, iterations, expected.len());
    constant_time_eq(&actual, &expected)
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iter: u32, len: usize) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(password).unwrap();
    mac.update(salt);
    mac.update(&(1u32).to_be_bytes());
    let mut u = mac.finalize().into_bytes().to_vec();
    let mut result = u.clone();
    for _ in 1..iter {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(password).unwrap();
        mac.update(&u);
        u = mac.finalize().into_bytes().to_vec();
        for (a, b) in result.iter_mut().zip(u.iter()) {
            *a ^= *b;
        }
    }
    result.truncate(len);
    result
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_round_trip() {
        let pwd = "correct horse battery staple";
        let hashed = hash_password(pwd);
        assert!(verify_password(pwd, &hashed));
        assert!(!verify_password("wrong", &hashed));
    }

    #[test]
    fn verify_rejects_malformed() {
        assert!(!verify_password("x", "not-a-hash"));
        assert!(!verify_password("x", "pbkdf2$abc$def"));
    }

    #[test]
    fn hash_format_is_stable() {
        let h = hash_password("pwd");
        assert!(h.starts_with("pbkdf2$"));
        let parts: Vec<&str> = h.split('$').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "pbkdf2");
        assert_eq!(parts[1], "100000");
    }
}
