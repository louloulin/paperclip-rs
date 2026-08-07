//! 本地磁盘 provider：内容寻址 + SHA-256 校验。

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures::stream;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::debug;

use crate::error::{StorageError, StorageResult};
use crate::provider::{ObjectMetadata, ObjectStream, StorageProvider};
use crate::types::{ObjectKey, PresignedUrl, StorageClass, StorageLocation};

#[derive(Debug, Clone)]
pub struct LocalDiskStorage {
    root: PathBuf,
    name: &'static str,
}

impl LocalDiskStorage {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            name: "local_disk",
        }
    }

    fn resolve(&self, bucket: &str, key: &str) -> StorageResult<PathBuf> {
        if bucket.is_empty() || bucket.contains('/') || bucket.contains("..") {
            return Err(StorageError::Invalid(format!("invalid bucket: {bucket}")));
        }
        if key.contains("..") {
            return Err(StorageError::Invalid(format!(
                "invalid key (path traversal): {key}"
            )));
        }
        Ok(self.root.join(bucket).join(key))
    }
}

#[async_trait]
impl StorageProvider for LocalDiskStorage {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn health(&self) -> StorageResult<()> {
        if !self.root.exists() {
            fs::create_dir_all(&self.root).await?;
        }
        Ok(())
    }

    async fn put_object(
        &self,
        target: &StorageLocation,
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<ObjectMetadata> {
        let path = self.resolve(&target.bucket, target.key.as_str())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let size = bytes.len() as u64;
        let sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            format!("{:x}", hasher.finalize())
        };

        let mut f = fs::File::create(&path).await?;
        f.write_all(&bytes).await?;
        f.sync_all().await?;

        // Sidecar metadata file with sha + content_type
        let meta_path = Path::new(&path).with_extension("meta");
        let meta_json = serde_json::json!({
            "key": target.key.as_str(),
            "size": size,
            "contentType": content_type,
            "sha256": sha256,
        });
        fs::write(&meta_path, serde_json::to_vec(&meta_json)?).await?;

        debug!(bucket = %target.bucket, key = %target.key, size = size, "local_disk put");
        Ok(ObjectMetadata {
            key: target.key.clone(),
            size,
            content_type: content_type.map(str::to_string),
            content_sha256: Some(sha256),
            last_modified: Utc::now(),
            class: StorageClass::Hot,
        })
    }

    async fn get_object(&self, location: &StorageLocation) -> StorageResult<Bytes> {
        let path = self.resolve(&location.bucket, location.key.as_str())?;
        match fs::read(&path).await {
            Ok(b) => Ok(Bytes::from(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StorageError::NotFound(
                format!("{}/{}", location.bucket, location.key),
            )),
            Err(e) => Err(e.into()),
        }
    }

    async fn stream_object(&self, location: &StorageLocation) -> StorageResult<ObjectStream> {
        let bytes = self.get_object(location).await?;
        Ok(Box::pin(stream::once(async move { Ok(bytes) })))
    }

    async fn delete_object(&self, location: &StorageLocation) -> StorageResult<()> {
        let path = self.resolve(&location.bucket, location.key.as_str())?;
        match fs::remove_file(&path).await {
            Ok(()) => {
                let meta = path.with_extension("meta");
                let _ = fs::remove_file(meta).await; // best-effort
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_prefix(&self, bucket: &str, prefix: &str) -> StorageResult<Vec<ObjectKey>> {
        let base = self.resolve(bucket, "")?;
        let mut out = Vec::new();
        if !base.exists() {
            return Ok(out);
        }
        let mut stack = vec![base];
        while let Some(dir) = stack.pop() {
            let mut entries = fs::read_dir(&dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|e| e.to_str()) != Some("meta") {
                    if let Ok(rel) = p.strip_prefix(self.root.join(bucket)) {
                        let rel_str = rel.to_string_lossy();
                        if rel_str.starts_with(prefix) {
                            out.push(ObjectKey::new(&*rel_str));
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    async fn presign_get(
        &self,
        location: &StorageLocation,
        ttl: std::time::Duration,
    ) -> StorageResult<PresignedUrl> {
        // Validate path exists so we don't hand out a URL for missing data
        let path = self.resolve(&location.bucket, location.key.as_str())?;
        if !path.exists() {
            return Err(StorageError::NotFound(format!(
                "{}/{}",
                location.bucket,
                location.key.as_str()
            )));
        }
        let expires_at = chrono::Utc::now() + ttl;
        // Local-disk presigned URL — `paperclip-local://bucket/key?exp=<unix>&sig=<b64-hash>`.
        // `sig` is base64(SHA256(root|bucket|key|exp)) — root path is the signing
        // secret (only the host knows the root, so it is the verifier).
        let payload = format!(
            "{}|{}|{}",
            location.bucket,
            location.key.as_str(),
            expires_at.timestamp()
        );
        let mut hasher = Sha256::new();
        hasher.update(self.root.to_string_lossy().as_bytes());
        hasher.update(b"|");
        hasher.update(payload.as_bytes());
        let sig = {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
        };
        let url = format!(
            "paperclip-local://{}/{}?exp={}&sig={}",
            location.bucket,
            location.key.as_str(),
            expires_at.timestamp(),
            sig
        );
        Ok(PresignedUrl { url, expires_at })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn location(bucket: &str, key: &str) -> StorageLocation {
        StorageLocation {
            bucket: bucket.into(),
            key: ObjectKey::new(key),
        }
    }

    #[tokio::test]
    async fn put_get_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = LocalDiskStorage::new(tmp.path().to_path_buf());
        let target = location("avatars", "user-1.png");
        let bytes = Bytes::from_static(b"hello world");
        let meta = store
            .put_object(&target, bytes.clone(), Some("image/png"))
            .await
            .unwrap();
        assert_eq!(meta.size, bytes.len() as u64);
        assert!(meta.content_sha256.is_some());
        let got = store.get_object(&target).await.unwrap();
        assert_eq!(got, bytes);
    }

    #[tokio::test]
    async fn get_missing_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let store = LocalDiskStorage::new(tmp.path().to_path_buf());
        let target = location("missing", "nope.bin");
        let err = store.get_object(&target).await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let store = LocalDiskStorage::new(tmp.path().to_path_buf());
        let target = location("avatars", "../etc/passwd");
        let err = store
            .put_object(&target, Bytes::from_static(b"hi"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::Invalid(_)));
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let store = LocalDiskStorage::new(tmp.path().to_path_buf());
        let target = location("logs", "x.log");
        store
            .put_object(&target, Bytes::from_static(b"x"), None)
            .await
            .unwrap();
        store.delete_object(&target).await.unwrap();
        // Second delete should not error
        store.delete_object(&target).await.unwrap();
    }

    #[tokio::test]
    async fn list_prefix_returns_only_bucket_keys() {
        let tmp = TempDir::new().unwrap();
        let store = LocalDiskStorage::new(tmp.path().to_path_buf());
        store
            .put_object(&location("a", "x/1"), Bytes::from_static(b"x"), None)
            .await
            .unwrap();
        store
            .put_object(&location("a", "x/2"), Bytes::from_static(b"y"), None)
            .await
            .unwrap();
        store
            .put_object(&location("b", "z/3"), Bytes::from_static(b"z"), None)
            .await
            .unwrap();
        let a = store.list_prefix("a", "x/").await.unwrap();
        assert_eq!(a.len(), 2);
        let b = store.list_prefix("b", "z/").await.unwrap();
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn round_trip_put_get_stream() {
        let tmp = TempDir::new().unwrap();
        let store = LocalDiskStorage::new(tmp.path().to_path_buf());
        let target = StorageLocation {
            bucket: "paperclip-assets".into(),
            key: ObjectKey::new("company-123/attach-1.png".to_string()),
        };
        let payload = b"hello world".to_vec();
        let meta = store
            .put_object(
                &target,
                bytes::Bytes::from(payload.clone()),
                Some("image/png"),
            )
            .await
            .unwrap();
        assert_eq!(meta.size, payload.len() as u64);
        let got = store.get_object(&target).await.unwrap();
        assert_eq!(got.as_ref(), payload.as_slice());
        let mut stream = store.stream_object(&target).await.unwrap();
        use futures::StreamExt;
        let mut collected = Vec::new();
        while let Some(chunk) = stream.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(collected, payload);
    }

    #[tokio::test]
    async fn get_object_not_found_returns_storage_error() {
        let tmp = TempDir::new().unwrap();
        let store = LocalDiskStorage::new(tmp.path().to_path_buf());
        let target = StorageLocation {
            bucket: "paperclip-assets".into(),
            key: ObjectKey::new("missing/file.png".to_string()),
        };
        let err = store.get_object(&target).await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn health_creates_root() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("nested/deep");
        let store = LocalDiskStorage::new(nested.clone());
        store.health().await.unwrap();
        assert!(nested.exists());
    }
}
