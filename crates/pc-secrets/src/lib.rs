#![forbid(unsafe_code)]

pub mod aws;
/// 秘密提供方抽象与本地 AES-256-GCM 加密实现。
///
/// 设计目标：
/// - 与原 paperclip `server/src/secrets/` 完全等价
/// - 支持 `local_encrypted`（AES-256-GCM + 主密钥文件）与 `aws_secrets_manager`（stub）
/// - 提供方注册表 + 健康检查
/// - `SecretProvider` trait 与 `SecretProviderModule` 分离
pub mod local_encrypted;
pub mod provider;
pub mod registry;
pub mod types;

pub use provider::SecretProvider;
pub use registry::SecretProviderRegistry;
pub use types::{
    LocalEncryptedMaterial, PreparedSecretVersion, ProviderHealthCheck, ProviderHealthStatus,
    SecretProviderValidationResult, StoredSecretVersionMaterial,
};

pub fn hmac_sha256(key: &[u8], payload: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(payload);
    let bytes = mac.finalize().into_bytes();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
