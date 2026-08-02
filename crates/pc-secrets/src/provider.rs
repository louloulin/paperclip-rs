use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{PreparedSecretVersion, ProviderHealthCheck, SecretProviderValidationResult};

/// 提供方运行时上下文。
#[derive(Debug, Clone)]
pub struct SecretProviderRuntimeContext {
    pub company_id: Uuid,
    pub secret_id: Uuid,
    pub secret_key: String,
    pub version: i32,
}

/// 提供方写入上下文。
#[derive(Debug, Clone)]
pub struct SecretProviderWriteContext {
    pub company_id: Uuid,
    pub secret_key: String,
    pub secret_name: String,
    pub version: i32,
}

/// 秘密提供方 trait（与原 `SecretProviderModule` 等价）。
///
/// 实现者只需实现自己支持的方法；默认实现返回 `501` 语义的 Err。
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// 返回此 provider 的标识符（如 `"local_encrypted"`）。
    fn provider_id(&self) -> &'static str;

    /// 校验提供方配置。
    async fn validate_config(
        &self,
        _provider_config: Option<serde_json::Value>,
    ) -> SecretProviderValidationResult {
        SecretProviderValidationResult::valid()
    }

    /// 创建新秘密（返回加密材料 + SHA-256）。
    async fn create_secret(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String>;

    /// 创建新版本（旋转密钥）。
    async fn create_version(
        &self,
        value: String,
        context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String>;

    /// 解析/解密已持久化的材料为明文。
    async fn resolve_version(
        &self,
        material: serde_json::Value,
        context: &SecretProviderRuntimeContext,
    ) -> Result<String, String>;

    /// 健康检查。
    async fn health_check(
        &self,
        _deployment_mode: Option<String>,
        _provider_config: Option<serde_json::Value>,
    ) -> ProviderHealthCheck;

    /// 删除或归档已持久化秘密。
    async fn delete_or_archive(
        &self,
        _material: Option<serde_json::Value>,
        _mode: &str,
        _context: &SecretProviderWriteContext,
    ) -> Result<(), String> {
        Ok(()) // no-op for local encrypted
    }
}
