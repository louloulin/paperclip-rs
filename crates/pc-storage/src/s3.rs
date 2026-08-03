//! S3 兼容 provider stub。
//!
//! 真正的 S3 客户端集成（`aws-sdk-s3` / `rust-s3`）留给 host 配置 AWS credentials
//! 后开启。本桩在 host 未配置时安全返回 `ProviderUnavailable`，避免误把桩当成真 S3 使用。

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::{StorageError, StorageResult};
use crate::provider::{ObjectMetadata, ObjectStream, StorageProvider};
use crate::types::{ObjectKey, StorageLocation};

#[derive(Debug, Clone)]
#[allow(dead_code)] // region/bucket retained for future AWS metadata
pub struct S3Storage {
    name: &'static str,
    configured: bool,
    region: String,
    bucket: String,
}

impl S3Storage {
    #[must_use]
    pub fn new(region: impl Into<String>, bucket: impl Into<String>) -> Self {
        Self {
            name: "s3",
            configured: false,
            region: region.into(),
            bucket: bucket.into(),
        }
    }

    /// 由 host 在加载 AWS credentials 后调用，启用真实调用。
    pub fn mark_configured(&mut self) {
        self.configured = true;
    }

    fn require_configured(&self) -> StorageResult<()> {
        if self.configured {
            Ok(())
        } else {
            Err(StorageError::ProviderUnavailable(
                "S3 provider not configured (host must supply AWS credentials)".into(),
            ))
        }
    }
}

#[async_trait]
impl StorageProvider for S3Storage {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn health(&self) -> StorageResult<()> {
        self.require_configured()
    }

    async fn put_object(
        &self,
        target: &StorageLocation,
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<ObjectMetadata> {
        self.require_configured()?;
        let _ = (target, bytes.len(), content_type);
        Err(StorageError::NotImplemented(
            "S3 put_object: requires aws-sdk-s3 (not bundled in pc-storage)".into(),
        ))
    }

    async fn get_object(&self, location: &StorageLocation) -> StorageResult<Bytes> {
        self.require_configured()?;
        let _ = location;
        Err(StorageError::NotImplemented(
            "S3 get_object: requires aws-sdk-s3".into(),
        ))
    }

    async fn stream_object(&self, _location: &StorageLocation) -> StorageResult<ObjectStream> {
        self.require_configured()?;
        Err(StorageError::NotImplemented(
            "S3 stream_object: requires aws-sdk-s3".into(),
        ))
    }

    async fn delete_object(&self, location: &StorageLocation) -> StorageResult<()> {
        self.require_configured()?;
        let _ = location;
        Err(StorageError::NotImplemented(
            "S3 delete_object: requires aws-sdk-s3".into(),
        ))
    }

    async fn list_prefix(&self, _bucket: &str, _prefix: &str) -> StorageResult<Vec<ObjectKey>> {
        self.require_configured()?;
        Err(StorageError::NotImplemented(
            "S3 list_prefix: requires aws-sdk-s3".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unconfigured_returns_provider_unavailable() {
        let s3 = S3Storage::new("us-east-1", "paperclip");
        assert!(s3.health().await.is_err());
        let target = StorageLocation {
            bucket: "x".into(),
            key: ObjectKey::new("k"),
        };
        let err = s3
            .put_object(&target, Bytes::from_static(b"hi"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::ProviderUnavailable(_)));
    }

    #[tokio::test]
    async fn configured_still_returns_not_implemented() {
        let mut s3 = S3Storage::new("us-east-1", "paperclip");
        s3.mark_configured();
        // Even when "configured", real implementation must be added by host layer.
        let target = StorageLocation {
            bucket: "x".into(),
            key: ObjectKey::new("k"),
        };
        let err = s3.get_object(&target).await.unwrap_err();
        assert!(matches!(err, StorageError::NotImplemented(_)));
    }
}
