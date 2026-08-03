#![forbid(unsafe_code)]

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
pub mod aws;

pub use provider::SecretProvider;
pub use registry::SecretProviderRegistry;
pub use types::{
    LocalEncryptedMaterial, PreparedSecretVersion, ProviderHealthCheck, ProviderHealthStatus,
    SecretProviderValidationResult, StoredSecretVersionMaterial,
};
