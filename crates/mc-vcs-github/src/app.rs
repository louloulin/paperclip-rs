//! GitHub App 的 RS256 JWT 签发（上游 `handler/github.go` 的 `signGitHubAppJWT` +
//! `ghsnapshot/client.go` 的 `signAppJWT`）。
//!
//! # 本片**确实实现**的唯一一处运行时原语（`docs/61` §2.4 / §6.5 的 M8-0 专属 `DoD`）
//!
//! anchor 其余文件都是桩，但 RS256 的**依赖边选定与往返实测**是本片的验收项：
//! 本仓没有 `jsonwebtoken`，而 `ring 0.17.14` 与 `rsa 0.9.10` 都已在 `Cargo.lock` 里
//! （传递依赖）⇒ 裁定用 **`ring`**（`RSA_PKCS1_SHA256` 对应上游 `golang-jwt` 的
//! PKCS#1 v1.5），**不新增外部包**。选择理由与往返实测落 `docs/32` §9.12。
//!
//! # 凭据纪律（`docs/61` §2.4 的第 1 条）
//!
//! [`AppJwtSigner`] **不派生 `Debug`**（手写脱敏）：私钥字节与签发出的 JWT **绝不**进日志。
//! `AppJwtError` 也**不携带**任何密钥材料（`ring` 的错误只有原因名）。
//!
//! # 与 M8-1 的分工
//!
//! 本文件的 `sign_app_jwt` / `sign_rs256` 是**完整实现**；M8-1 只需在其上接
//! installation token 交换与缓存（`token_cache.rs`），**不得**另起一份 JWT 签名。

use base64::engine::general_purpose::{STANDARD as B64_STD, URL_SAFE_NO_PAD as B64_URL};
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{self, KeyPair as _, RsaKeyPair};
use serde_json::json;

/// App JWT 签发/校验的错误（**不含**任何密钥材料）。
#[derive(Debug, thiserror::Error)]
pub enum AppJwtError {
    #[error("GITHUB_APP_PRIVATE_KEY is not valid PEM")]
    InvalidPem,
    #[error("GITHUB_APP_PRIVATE_KEY is not a PKCS#8 RSA private key: {0}")]
    InvalidKey(String),
    #[error("RS256 signing failed: {0}")]
    Sign(String),
}

/// 从 PEM 文本解出 PKCS#8 DER。
///
/// 只接受 `BEGIN PRIVATE KEY`（PKCS#8）—— `BEGIN RSA PRIVATE KEY`（PKCS#1）需要
/// `rsa` crate 才能转 PKCS#8，而本片刻意只接一条 `ring` 直连边（`docs/61` §2.4）。
/// 上游 `jwt.ParseRSAPrivateKeyFromPEM` 两种都收；这里是**登记过的收缩**
/// （`docs/32` §9.12）。
pub fn pkcs8_der_from_pem(pem: &str) -> Result<Vec<u8>, AppJwtError> {
    let mut body = String::new();
    for line in pem.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN ") || line.starts_with("-----END ") {
            continue;
        }
        body.push_str(line);
    }
    if body.is_empty() {
        return Err(AppJwtError::InvalidPem);
    }
    B64_STD.decode(body).map_err(|_| AppJwtError::InvalidPem)
}

/// GitHub App 私钥的 RS256 签发器。
///
/// 手写 `Debug`（不派生）：只暴露 `app_id` 与公钥长度，**绝不**暴露私钥。
pub struct AppJwtSigner {
    app_id: String,
    key_pair: RsaKeyPair,
}

impl std::fmt::Debug for AppJwtSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppJwtSigner")
            .field("app_id", &self.app_id)
            .field("private_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl AppJwtSigner {
    /// 从 PEM 私钥构造。
    ///
    /// # Errors
    ///
    /// PEM 非法、不是 PKCS#8、或 `ring` 拒绝该密钥（如密钥过小）时返回 [`AppJwtError`]。
    pub fn from_pem(app_id: impl Into<String>, pem: &str) -> Result<Self, AppJwtError> {
        let der = pkcs8_der_from_pem(pem)?;
        let key_pair =
            RsaKeyPair::from_pkcs8(&der).map_err(|e| AppJwtError::InvalidKey(e.to_string()))?;
        Ok(Self {
            app_id: app_id.into(),
            key_pair,
        })
    }

    /// App id（JWT 的 `iss`）。
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// 私钥对应的公钥（DER 形态，可直接交给 [`verify_rs256`]）。
    pub fn public_key(&self) -> &[u8] {
        self.key_pair.public_key().as_ref()
    }

    /// 对任意消息做 RS256（PKCS#1 v1.5）签名。
    ///
    /// # Errors
    ///
    /// `ring` 的签名失败（应当只在系统 RNG 不可用时发生）。
    pub fn sign_rs256(&self, message: &[u8]) -> Result<Vec<u8>, AppJwtError> {
        let rng = SystemRandom::new();
        let mut signature = vec![0u8; self.key_pair.public().modulus_len()];
        self.key_pair
            .sign(&signature::RSA_PKCS1_SHA256, &rng, message, &mut signature)
            .map_err(|e| AppJwtError::Sign(e.to_string()))?;
        Ok(signature)
    }

    /// 签发 GitHub App JWT（上游 `signAppJWT` 的口径）。
    ///
    /// `iat` 回拨 60 秒吸收时钟偏移，`exp` 上限 9 分钟（GitHub 的天花板是 10 分钟）。
    /// `now_unix_secs` 显式传入（**不读系统时钟**）⇒ 过期/时钟偏移可注入测试。
    ///
    /// # Errors
    ///
    /// 同 [`AppJwtSigner::sign_rs256`]。
    pub fn sign_app_jwt(&self, now_unix_secs: i64) -> Result<String, AppJwtError> {
        let header = json!({"alg": "RS256", "typ": "JWT"});
        let payload = json!({
            "iat": now_unix_secs - 60,
            "exp": now_unix_secs + 9 * 60,
            "iss": self.app_id,
        });
        let header_b64 = B64_URL.encode(serde_json::to_vec(&header).unwrap_or_default());
        let payload_b64 = B64_URL.encode(serde_json::to_vec(&payload).unwrap_or_default());
        let signing_input = format!("{header_b64}.{payload_b64}");
        let signature = self.sign_rs256(signing_input.as_bytes())?;
        Ok(format!("{signing_input}.{}", B64_URL.encode(signature)))
    }
}

/// 校验 RS256（PKCS#1 v1.5）签名 —— 只用于**测试**与本地替身，生产不做验签
/// （GitHub 的 JWT 由 GitHub 自己验）。
pub fn verify_rs256(public_key_der: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let key =
        signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, public_key_der);
    key.verify(message, signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用私钥（**不是**生产密钥；只为往返自证存在，且 `docs/61` §6.5 要求
    /// 「实测签发→验证往返」）。它在仓库里没有第二个消费者。
    const TEST_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQC8Lko4B+10Dj7o
dZedrDE3ZbHRBAsSKACHGOsI00EzEEUh8di1MVB1O/dYC2ZwlsteuwkZFF+aQDuT
3gZTzQVkq5menpNmJ5BVKfQr6jy7mQAhhgUEG69Afu2QNC3hFe3S+lcjdt9agM3r
rbuK35OrPrhaeW33bO+1ip0r5gnnw0j6IaW+Tnndzusv1H7tBJj9R6IA7E7tPGIr
UbdWADlP2XDPC/g5R37XhY5H5sg9B3dJptrVBD532eHG0nK7UTQybq7QMHOygYn5
KfbgZZLnYwpV40SE56DhR9jzxcIdo8okGMeoIlbwebC09spzLXwi66bPW6jE7eKY
cYwfvixlAgMBAAECggEACpr7QNQljERfRD+YV2EEdxBKqLJ3I0NQ4ExFtr4dLxEM
LGESawfH9ot2Iaam09KTzJdy6FBvIOTc1rUNGzzzQFyxcDCUsw2owzv1kGIHoTT6
vmjssHIU+ugMYHOoYEaZnCnSrmN9K/8VW+JzLtzx2BVVU3gDfA3OJqeUuwwgY8jT
bSrNe8LKiS6sj5IF2hIpBIXFTuE6SKU/64T3kiKG2AbnLLtF34LNl1sbw5TTjEAs
hGnDvNJqHgCtDqGDXaAiNbTrPWeWMJW4RhfLDzF1fpZXSw3f5z+zVMzgg6JSBZCj
Pf5FAsU4db9cGW+yLjb5NqNrBX/c24OxZ2yNcPRSkQKBgQDsraU02Mf2uFj22T0r
HBp3q11IcpuA7YU2lBP6o4aKlc6cCyj8U75yS7hu/kUoEtAUnxyyCXOpM7lhGbmz
dtIqM6r74FlIr8mPi2TLMloUGmtY42sUNtciQgd+Ds4RjWNC2/1FRe5XMFm1bP/f
6jMZEr0k0d0Cz7au5sZg4Jwg6QKBgQDLixe4xIOvQ7wvKpKngKbStW0MIAoYnvSB
hIj3xxasXc6V/qp9ANTnG28yk9tsCajUMQSfEX0uIx1YSAB0MbaIrAhsqvUaknDx
vkUtaeBWE8e0H3U+9KVnVWLZoPNHFIVeGhWMXJKpbQiZiDreELaAeltgIzU5JfAP
VRJ8eoOiHQKBgQCopqQOoFr9aCec3vhDe+cwVyBFu8UrfhVq6uHBvDznDBEKCLnP
9CzFbUejb/T/tUgpKahdBXcxnvX+R0KYq5bfE6pHiXqV3Q2YCBBu6xZdNOZBlOx8
nwd2Fe8Y2JvmzgVpYzF653YLEx0Ztu4uNMjsmPnG/vSqSDE5OKEr72HR4QKBgQDF
jWKgulr1KNDlFnTwjjVcHSqRsicabmzxqCkoE9s1wHZZrqraWIxLIp1ygX9eBKIQ
EONjYB4XQY2huYB3Rijbzdz/W445FBj7CKkrwq8x3FDfygiJ6fj/qigfAdAdFRW8
l6SCbvcJ6gGGwmogTihT2m4FiSaHKQMuXmtq1Z4dIQKBgQCBdv0m4+OX6Rxjx3cd
cpr1nFZBZqPqOdDLakWXRko+K0eNFOKCF4Smf3OUVZz5yoAAGG9HcXEwUO09ZzCD
6FZCMLwGRY4IXmRObxEfD5k/ZlzL9+/rNIKtQjObwppciqPW0NoPt5qJvMJXq7Dw
C2CZCWlMaEGLpAGiVraQnRQlPw==
-----END PRIVATE KEY-----
";

    #[test]
    fn rs256_sign_verify_roundtrip() {
        let signer = AppJwtSigner::from_pem("123456", TEST_KEY_PEM).expect("test key must parse");
        let message = b"header.payload";
        let signature = signer.sign_rs256(message).expect("sign");
        assert!(verify_rs256(signer.public_key(), message, &signature));

        // 反例一：消息差 1 字节。
        assert!(!verify_rs256(
            signer.public_key(),
            b"header.payloae",
            &signature
        ));
        // 反例二：签名差 1 位。
        let mut tampered = signature.clone();
        tampered[0] ^= 0x01;
        assert!(!verify_rs256(signer.public_key(), message, &tampered));
    }

    #[test]
    fn app_jwt_has_three_segments_and_expected_claims() {
        let signer = AppJwtSigner::from_pem("123456", TEST_KEY_PEM).expect("test key must parse");
        let now = 1_700_000_000_i64;
        let token = signer.sign_app_jwt(now).expect("sign jwt");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must be header.payload.signature");

        let payload: serde_json::Value =
            serde_json::from_slice(&B64_URL.decode(parts[1]).expect("payload is base64url"))
                .expect("payload is JSON");
        assert_eq!(payload["iss"], "123456");
        assert_eq!(payload["iat"], now - 60);
        assert_eq!(payload["exp"], now + 9 * 60);

        // 签名是对 `header.payload` 逐字节的 RS256。
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let signature = B64_URL.decode(parts[2]).expect("signature is base64url");
        assert!(verify_rs256(
            signer.public_key(),
            signing_input.as_bytes(),
            &signature
        ));
    }

    #[test]
    fn non_pkcs8_or_garbage_pem_is_rejected_without_leaking() {
        assert!(matches!(
            AppJwtSigner::from_pem("1", "not a pem"),
            Err(AppJwtError::InvalidPem)
        ));
        // `BEGIN RSA PRIVATE KEY`（PKCS#1）是登记过的收缩：解析器只认 PKCS#8。
        let pkcs1 =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK\n-----END RSA PRIVATE KEY-----";
        assert!(AppJwtSigner::from_pem("1", pkcs1).is_err());
    }
}
