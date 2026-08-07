//! M3 真实验证：local_disk put/get/stream/list/delete/presign + SHA-256。
//!
//! 与 Node `server/src/storage/local-disk-provider.ts` 行为对齐：
//! - 写入成功返回 SHA-256 元数据
//! - get 返回字节
//! - list_prefix 仅返回该前缀
//! - delete 幂等（不存在不报错）
//! - presign_get 返回 HMAC 签名 URL（仅本地磁盘自实现签名）
//! - bucket 含 `/` 或 `..` 拒绝
//! - key 含 `..` 拒绝（防 path traversal）

use bytes::Bytes;
use pc_storage::types::{ObjectKey, StorageClass, StorageLocation, WriteTarget};
use pc_storage::{LocalDiskStorage, StorageProvider};
use std::time::Duration;
use tempfile::TempDir;

fn loc(bucket: &str, key: &str) -> StorageLocation {
    StorageLocation {
        bucket: bucket.into(),
        key: ObjectKey::new(key),
    }
}

fn target(bucket: &str, key: &str) -> WriteTarget {
    WriteTarget {
        location: loc(bucket, key),
        class: StorageClass::Hot,
    }
}

#[tokio::test]
async fn put_get_roundtrip() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    let payload = Bytes::from_static(b"hello paperclip storage");
    let meta = store
        .put_object(
            &target("attachments", "a.txt").location,
            payload.clone(),
            Some("text/plain"),
        )
        .await
        .expect("put");
    assert_eq!(meta.size, payload.len() as u64);
    assert!(meta.content_sha256.is_some(), "sha256 metadata present");
    // 第二个独立断言 — 实际值由 put_get_sha256_known_value 覆盖
    let read = store
        .get_object(&loc("attachments", "a.txt"))
        .await
        .expect("get");
    assert_eq!(read, payload);
}

#[tokio::test]
async fn put_get_sha256_known_value() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    // "abc" 的 SHA-256 已知常量
    let payload = Bytes::from_static(b"abc");
    let meta = store
        .put_object(&target("b", "k").location, payload.clone(), None)
        .await
        .expect("put");
    assert_eq!(
        meta.content_sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
}

#[tokio::test]
async fn get_missing_object_returns_error() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    let err = store.get_object(&loc("b", "nope")).await.unwrap_err();
    assert!(matches!(err, pc_storage::StorageError::NotFound(_)));
}

#[tokio::test]
async fn delete_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    store
        .put_object(&target("b", "k").location, Bytes::from_static(b"x"), None)
        .await
        .expect("put");
    store
        .delete_object(&loc("b", "k"))
        .await
        .expect("first delete");
    store
        .delete_object(&loc("b", "k"))
        .await
        .expect("second delete idempotent");
}

#[tokio::test]
async fn list_prefix_filters_correctly() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    store
        .put_object(
            &target("bucket1", "a/1.txt").location,
            Bytes::from_static(b"1"),
            None,
        )
        .await
        .unwrap();
    store
        .put_object(
            &target("bucket1", "a/2.txt").location,
            Bytes::from_static(b"2"),
            None,
        )
        .await
        .unwrap();
    store
        .put_object(
            &target("bucket1", "b/3.txt").location,
            Bytes::from_static(b"3"),
            None,
        )
        .await
        .unwrap();
    let keys = store.list_prefix("bucket1", "a/").await.expect("list");
    assert_eq!(keys.len(), 2);
    let names: Vec<String> = keys.iter().map(|k| k.as_str().to_string()).collect();
    assert!(names.contains(&"a/1.txt".to_string()));
    assert!(names.contains(&"a/2.txt".to_string()));
}

#[tokio::test]
async fn bucket_with_slash_rejected() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    let err = store
        .put_object(
            &target("../etc", "passwd").location,
            Bytes::from_static(b"x"),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, pc_storage::StorageError::Invalid(_)));
}

#[tokio::test]
async fn key_path_traversal_rejected() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    let err = store
        .put_object(
            &target("b", "../escape.txt").location,
            Bytes::from_static(b"x"),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, pc_storage::StorageError::Invalid(_)));
}

#[tokio::test]
async fn presign_get_returns_url() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    store
        .put_object(
            &target("b", "k.txt").location,
            Bytes::from_static(b"data"),
            None,
        )
        .await
        .unwrap();
    let url = store
        .presign_get(&loc("b", "k.txt"), Duration::from_secs(60))
        .await
        .expect("presign");
    assert!(url.url.starts_with("file://") || url.url.contains("k.txt"));
    assert!(url.expires_at > chrono::Utc::now());
}

#[tokio::test]
async fn stream_object_emits_bytes() {
    use futures::StreamExt;
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    let payload = Bytes::from_static(b"stream-test-bytes");
    store
        .put_object(&target("b", "k").location, payload.clone(), None)
        .await
        .unwrap();
    let mut stream = store.stream_object(&loc("b", "k")).await.expect("stream");
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(buf, payload.to_vec());
}

#[tokio::test]
async fn health_is_ok() {
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStorage::new(dir.path().to_path_buf());
    store.health().await.expect("healthy");
}
