//! `StorageProvider` trait 抽象。

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};

use crate::error::{StorageError, StorageResult};
use crate::types::{ObjectKey, PresignedUrl, StorageClass, StorageLocation, WriteTarget};

/// 对象的元数据。
#[derive(Debug, Clone)]
pub struct ObjectMetadata {
    pub key: ObjectKey,
    pub size: u64,
    pub content_type: Option<String>,
    pub content_sha256: Option<String>,
    pub last_modified: DateTime<Utc>,
    pub class: StorageClass,
}

/// 内容流（本地磁盘 provider 直接是字节；S3 可走 multipart）。
pub type ObjectStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, StorageError>> + Send + Sync>>;

#[async_trait]
pub trait StorageProvider: Send + Sync + std::fmt::Debug {
    /// provider 名称（用于 registry）。
    fn name(&self) -> &'static str;

    /// 健康检查：不抛错即 OK。
    async fn health(&self) -> StorageResult<()> {
        Ok(())
    }

    /// 写一个对象（覆盖语义）。
    async fn put_object(
        &self,
        target: &StorageLocation,
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<ObjectMetadata>;

    /// 读一个完整对象（小文件适用）。
    async fn get_object(&self, location: &StorageLocation) -> StorageResult<Bytes>;

    /// 流式读取。
    async fn stream_object(&self, location: &StorageLocation) -> StorageResult<ObjectStream>;

    /// 删除对象。幂等（不存在不报错）。
    async fn delete_object(&self, location: &StorageLocation) -> StorageResult<()>;

    /// 列出 bucket 下前缀（用于回归测试 / 清理）。
    async fn list_prefix(&self, bucket: &str, prefix: &str) -> StorageResult<Vec<ObjectKey>>;

    /// 生成临时下载 URL（如不支持 → `StorageError::NotImplemented`）。
    async fn presign_get(
        &self,
        location: &StorageLocation,
        ttl: std::time::Duration,
    ) -> StorageResult<PresignedUrl> {
        let _ = (location, ttl);
        Err(StorageError::NotImplemented(
            "presign_get not implemented for this provider".into(),
        ))
    }

    /// 默认写入目标（host 调用时简化传参）。
    fn default_write_target(&self, bucket: &str, key: &str) -> WriteTarget {
        WriteTarget::new(bucket, key)
    }
}
