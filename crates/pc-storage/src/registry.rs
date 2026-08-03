//! 多 provider 注册表：按 bucket 选择 provider。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::error::{StorageError, StorageResult};
use crate::provider::StorageProvider;

/// Provider registry: `bucket_name -> provider_name -> provider`。
/// 一个 provider 可服务多个 bucket。
#[derive(Clone, Default)]
pub struct StorageRegistry {
    inner: Arc<RwLock<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    providers: HashMap<&'static str, Arc<dyn StorageProvider>>,
    bucket_routes: HashMap<String, &'static str>,
}

impl std::fmt::Debug for RegistryInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryInner")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("bucket_routes", &self.bucket_routes)
            .finish()
    }
}

impl std::fmt::Debug for StorageRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read().expect("storage registry poisoned");
        f.debug_struct("StorageRegistry")
            .field("inner", &*inner)
            .finish()
    }
}

impl StorageRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, provider: Arc<dyn StorageProvider>) -> Result<(), StorageError> {
        let name = provider.name();
        let mut inner = self.inner.write().expect("storage registry poisoned");
        if inner.providers.contains_key(name) {
            return Err(StorageError::Invalid(format!(
                "provider {name} already registered"
            )));
        }
        inner.providers.insert(name, provider);
        Ok(())
    }

    pub fn route_bucket(
        &self,
        bucket: impl Into<String>,
        provider_name: &'static str,
    ) -> Result<(), StorageError> {
        let bucket = bucket.into();
        let inner = self.inner.read().expect("storage registry poisoned");
        if !inner.providers.contains_key(provider_name) {
            return Err(StorageError::ProviderUnavailable(format!(
                "cannot route bucket {bucket}: provider {provider_name} not registered"
            )));
        }
        drop(inner);
        let mut inner = self.inner.write().expect("storage registry poisoned");
        inner.bucket_routes.insert(bucket, provider_name);
        Ok(())
    }

    pub fn resolve(&self, bucket: &str) -> StorageResult<Arc<dyn StorageProvider>> {
        let inner = self.inner.read().expect("storage registry poisoned");
        let provider_name = inner
            .bucket_routes
            .get(bucket)
            .copied()
            .or_else(|| inner.providers.keys().next().copied())
            .ok_or_else(|| StorageError::ProviderUnavailable("no providers registered".into()))?;
        let provider = inner.providers.get(provider_name).cloned().ok_or_else(|| {
            StorageError::ProviderUnavailable(format!("provider {provider_name} not found"))
        })?;
        Ok(provider)
    }

    #[must_use]
    pub fn provider_names(&self) -> Vec<&'static str> {
        let inner = self.inner.read().expect("storage registry poisoned");
        let mut names: Vec<&'static str> = inner.providers.keys().copied().collect();
        names.sort_unstable();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use bytes::Bytes;

    use crate::provider::{ObjectMetadata, ObjectStream};
    use crate::types::{ObjectKey, StorageLocation};

    #[derive(Debug)]
    struct StubProvider;

    #[async_trait]
    impl StorageProvider for StubProvider {
        fn name(&self) -> &'static str {
            "stub"
        }
        async fn put_object(
            &self,
            _target: &StorageLocation,
            _bytes: Bytes,
            _ct: Option<&str>,
        ) -> StorageResult<ObjectMetadata> {
            Err(StorageError::NotImplemented("stub".into()))
        }
        async fn get_object(&self, _l: &StorageLocation) -> StorageResult<Bytes> {
            Err(StorageError::NotImplemented("stub".into()))
        }
        async fn stream_object(&self, _l: &StorageLocation) -> StorageResult<ObjectStream> {
            Err(StorageError::NotImplemented("stub".into()))
        }
        async fn delete_object(&self, _l: &StorageLocation) -> StorageResult<()> {
            Err(StorageError::NotImplemented("stub".into()))
        }
        async fn list_prefix(&self, _b: &str, _p: &str) -> StorageResult<Vec<ObjectKey>> {
            Err(StorageError::NotImplemented("stub".into()))
        }
    }

    #[tokio::test]
    async fn register_and_resolve_falls_back_to_first() {
        let r = StorageRegistry::new();
        r.register(Arc::new(StubProvider)).unwrap();
        let resolved = r.resolve("any-bucket").unwrap();
        assert_eq!(resolved.name(), "stub");
    }

    #[tokio::test]
    async fn route_bucket_overrides_fallback() {
        let r = StorageRegistry::new();
        r.register(Arc::new(StubProvider)).unwrap();
        r.route_bucket("cold", "stub").unwrap();
        let resolved = r.resolve("cold").unwrap();
        assert_eq!(resolved.name(), "stub");
    }

    #[tokio::test]
    async fn empty_registry_returns_error() {
        let r = StorageRegistry::new();
        let err = r.resolve("any").unwrap_err();
        assert!(matches!(err, StorageError::ProviderUnavailable(_)));
    }

    #[tokio::test]
    async fn duplicate_provider_name_rejected() {
        let r = StorageRegistry::new();
        r.register(Arc::new(StubProvider)).unwrap();
        let err = r.register(Arc::new(StubProvider)).unwrap_err();
        assert!(matches!(err, StorageError::Invalid(_)));
    }
}
