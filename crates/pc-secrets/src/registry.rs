use std::collections::HashMap;
use std::sync::Arc;

use crate::provider::SecretProvider;

/// 秘密提供方注册表。
///
/// 等价于原 `server/src/secrets/provider-registry.ts`。
/// 所有注册的提供方通过 `provider_id()` 查找。
#[derive(Default)]
pub struct SecretProviderRegistry {
    providers: HashMap<String, Arc<dyn SecretProvider>>,
}

impl SecretProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个提供方。
    pub fn register(&mut self, provider: Arc<dyn SecretProvider>) {
        let id = provider.provider_id().to_string();
        self.providers.insert(id, provider);
    }

    /// 按 ID 获取提供方。
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn SecretProvider>> {
        self.providers.get(id)
    }

    /// 返回所有已注册的提供方 ID。
    #[must_use]
    pub fn provider_ids(&self) -> Vec<&str> {
        self.providers.keys().map(String::as_str).collect()
    }

    /// 返回已注册的提供方数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_encrypted::LocalEncryptedProvider;
    use crate::provider::{SecretProviderRuntimeContext, SecretProviderWriteContext};
    use uuid::Uuid;

    #[test]
    fn registry_can_register_and_lookup() {
        let mut reg = SecretProviderRegistry::new();
        let test_key = [0x42u8; 32];
        reg.register(Arc::new(LocalEncryptedProvider::from_bytes(test_key)));

        let provider = reg.get("local_encrypted").unwrap();
        assert_eq!(provider.provider_id(), "local_encrypted");
    }

    #[test]
    fn registry_returns_none_for_unknown_provider() {
        let reg = SecretProviderRegistry::new();
        assert!(reg.get("aws_secrets_manager").is_none());
    }

    #[tokio::test]
    async fn registered_provider_works_end_to_end() {
        let mut reg = SecretProviderRegistry::new();
        let test_key = [0x12u8; 32];
        reg.register(Arc::new(LocalEncryptedProvider::from_bytes(test_key)));

        let p = reg.get("local_encrypted").unwrap();
        let ctx = SecretProviderWriteContext {
            company_id: Uuid::nil(),
            secret_key: "K".into(),
            secret_name: "test".into(),
            version: 1,
        };

        let prep = p.create_secret("hello world".into(), &ctx).await.unwrap();
        let runtime_ctx = SecretProviderRuntimeContext {
            company_id: Uuid::nil(),
            secret_id: Uuid::nil(),
            secret_key: "K".into(),
            version: 1,
        };
        let resolved = p
            .resolve_version(prep.material, &runtime_ctx)
            .await
            .unwrap();
        assert_eq!(resolved, "hello world");
    }
}
