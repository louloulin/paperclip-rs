//! Local-disk storage provider。

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::{sha256_etag, validate_key, Object, Result, StorageError, StorageProvider};

pub const PROVIDER_NAME: &str = "local_disk";

pub struct LocalDiskStorage {
    root: PathBuf,
    name: &'static str,
}

impl LocalDiskStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            name: PROVIDER_NAME,
        }
    }

    fn path_for(&self, bucket: &str, key: &str) -> std::result::Result<PathBuf, StorageError> {
        validate_key(key)?;
        Ok(self.root.join(bucket).join(key))
    }
}

#[async_trait]
impl StorageProvider for LocalDiskStorage {
    fn name(&self) -> &str {
        self.name
    }

    async fn put(
        &self,
        bucket: &str,
        key: &str,
        data: ::bytes::Bytes,
        content_type: Option<&str>,
    ) -> Result<Object> {
        let path = self.path_for(bucket, key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::Io(e.to_string()))?;
        }
        tokio::fs::write(&path, &data)
            .await
            .map_err(|e| StorageError::Io(e.to_string()))?;
        if let Some(_ct) = content_type {
            // content-type metadata not persisted; could go to sidecar later.
        }
        let etag = sha256_etag(&data);
        Ok(Object {
            key: key.to_string(),
            size: data.len() as u64,
            content_type: content_type.map(|s| s.to_string()),
            etag: Some(etag),
            provider: self.name.to_string(),
            bucket: bucket.to_string(),
        })
    }

    async fn get(&self, bucket: &str, key: &str) -> Result<::bytes::Bytes> {
        let path = self.path_for(bucket, key)?;
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(::bytes::Bytes::from(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::NotFound(format!("{bucket}/{key}")))
            }
            Err(e) => Err(StorageError::Io(e.to_string())),
        }
    }

    async fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        let path = self.path_for(bucket, key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StorageError::Io(e.to_string())),
        }
    }

    async fn head(&self, bucket: &str, key: &str) -> Result<Object> {
        let path = self.path_for(bucket, key)?;
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(Object {
            key: key.to_string(),
            size: meta.len(),
            content_type: None,
            etag: None,
            provider: self.name.to_string(),
            bucket: bucket.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalDiskStorage::new(tmp.path().to_path_buf());
        let data = ::bytes::Bytes::from_static(b"hello");
        let obj = storage
            .put("bucket", "a/b/file.txt", data.clone(), Some("text/plain"))
            .await
            .unwrap();
        assert_eq!(obj.size, 5);
        let got = storage.get("bucket", "a/b/file.txt").await.unwrap();
        assert_eq!(got, data);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalDiskStorage::new(tmp.path().to_path_buf());
        assert!(storage
            .put("bucket", "a/../b", ::bytes::Bytes::from_static(b"x"), None)
            .await
            .is_err());
    }
}
