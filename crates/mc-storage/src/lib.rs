//! Multica storage providers：local-disk / s3。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use async_trait::async_trait;
use tracing::info;

pub mod local;
pub mod s3;

pub use local::LocalDiskStorage;
#[cfg(feature = "s3")]
pub use s3::S3Storage;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io error: {0}")]
    Io(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid key: {0}")]
    InvalidKey(String),

    #[error("provider error: {0}")]
    Provider(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;

/// 存储对象描述符。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub provider: String,
    pub bucket: String,
}

/// Storage provider trait。
#[async_trait]
pub trait StorageProvider: Send + Sync {
    fn name(&self) -> &str;

    async fn put(
        &self,
        bucket: &str,
        key: &str,
        data: ::bytes::Bytes,
        content_type: Option<&str>,
    ) -> Result<Object>;

    async fn get(&self, bucket: &str, key: &str) -> Result<::bytes::Bytes>;

    async fn delete(&self, bucket: &str, key: &str) -> Result<()>;

    async fn head(&self, bucket: &str, key: &str) -> Result<Object>;
}

/// 顶层 Storage facade：bucket → provider 路由。
#[derive(Default, Clone)]
pub struct Storage {
    pub providers:
        Arc<std::sync::RwLock<std::collections::HashMap<String, Arc<dyn StorageProvider>>>>,
    pub bucket_routes: Arc<std::sync::RwLock<std::collections::HashMap<String, String>>>,
}

impl Storage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, provider: Arc<dyn StorageProvider>) -> Result<()> {
        self.providers
            .write()
            .unwrap()
            .insert(provider.name().to_string(), provider);
        Ok(())
    }

    pub fn route_bucket(&self, bucket: &str, provider_name: &str) -> Result<()> {
        self.bucket_routes
            .write()
            .unwrap()
            .insert(bucket.to_string(), provider_name.to_string());
        Ok(())
    }

    pub fn provider_for(&self, bucket: &str) -> Result<Arc<dyn StorageProvider>> {
        let routes = self.bucket_routes.read().unwrap();
        let provider_name = routes
            .get(bucket)
            .ok_or_else(|| StorageError::NotFound(format!("bucket not routed: {bucket}")))?;
        let providers = self.providers.read().unwrap();
        providers.get(provider_name).cloned().ok_or_else(|| {
            StorageError::NotFound(format!("provider not registered: {provider_name}"))
        })
    }

    pub async fn put(
        &self,
        bucket: &str,
        key: &str,
        data: ::bytes::Bytes,
        content_type: Option<&str>,
    ) -> Result<Object> {
        let provider = self.provider_for(bucket)?;
        provider.put(bucket, key, data, content_type).await
    }

    pub async fn get(&self, bucket: &str, key: &str) -> Result<::bytes::Bytes> {
        let provider = self.provider_for(bucket)?;
        provider.get(bucket, key).await
    }

    pub async fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        let provider = self.provider_for(bucket)?;
        provider.delete(bucket, key).await
    }

    pub async fn head(&self, bucket: &str, key: &str) -> Result<Object> {
        let provider = self.provider_for(bucket)?;
        provider.head(bucket, key).await
    }
}

/// 验证 key 合法（不允许 `..` 或空段）。
pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.contains("..") || key.starts_with('/') {
        return Err(StorageError::InvalidKey(key.to_string()));
    }
    Ok(())
}

/// 默认 multipart 上传帮助。
pub async fn write_file(path: &std::path::PathBuf, data: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| StorageError::Io(e.to_string()))?;
    }
    let mut f = tokio::fs::File::create(path)
        .await
        .map_err(|e| StorageError::Io(e.to_string()))?;
    f.write_all(data)
        .await
        .map_err(|e| StorageError::Io(e.to_string()))?;
    f.flush()
        .await
        .map_err(|e| StorageError::Io(e.to_string()))?;
    info!(path = %path.display(), size = data.len(), "wrote file");
    Ok(())
}

/// Compute SHA-256 etag.
pub fn sha256_etag(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Re-export bytes-like abstraction。
pub use ::bytes;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_key_rejects_traversal() {
        assert!(validate_key("a/b/c").is_ok());
        assert!(validate_key("..").is_err());
        assert!(validate_key("a/../b").is_err());
        assert!(validate_key("/abs").is_err());
        assert!(validate_key("").is_err());
    }

    #[test]
    fn sha256_etag_known() {
        let etag = sha256_etag(b"hello");
        assert_eq!(
            etag,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn storage_routes_to_provider() {
        let storage = Storage::new();
        storage
            .register(Arc::new(local::LocalDiskStorage::new("/tmp/mc-test")))
            .unwrap();
        storage.route_bucket("mc-assets", "local_disk").unwrap();
        let provider = storage.provider_for("mc-assets").unwrap();
        assert_eq!(provider.name(), "local_disk");
    }
}
