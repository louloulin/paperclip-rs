//! M29 — pc-storage::service 真 e2e(集成 LocalDiskStorage 真磁盘 IO)。
//!
//! 覆盖与 Node `server/src/storage/service.ts` 等价的:
//! - put → get → head → delete 闭环
//! - company prefix 隔离(跨 company 拒绝访问)
//! - `..` 路径拒绝
//! - namespace 归一化 + 文件名清洗
//! - SHA256 摘要正确性
//! - 空 body 拒绝

use bytes::Bytes;
use pc_storage::{
    ensure_company_prefix, hash_buffer, normalize_namespace, sanitize_segment, split_filename,
    PutFileInput, ServiceError, StorageService,
};
use std::sync::Arc;

fn new_local_service(tmp: &std::path::Path) -> StorageService {
    StorageService::new(
        Arc::new(pc_storage::LocalDiskStorage::new(tmp.to_path_buf())),
        "default-bucket",
    )
}

#[tokio::test]
async fn e2e_put_get_head_delete_with_local_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = new_local_service(tmp.path());
    let body = Bytes::from_static(b"paperclip storage service e2e");

    let result = svc
        .put_file(PutFileInput {
            company_id: "company-A".to_string(),
            namespace: "documents/invoices".to_string(),
            content_type: "text/plain".to_string(),
            body: body.clone(),
            original_filename: Some("invoice-2026-01.txt".to_string()),
        })
        .await
        .expect("put ok");

    // Object key layout
    let key = result.object_key.as_str();
    assert!(
        key.starts_with("company-A/documents/invoices/"),
        "key={key}"
    );
    let parts: Vec<&str> = key.split('/').collect();
    // company/ns(=2 segments)/yyyy/mm/dd/file
    assert_eq!(parts.len(), 7);
    assert_eq!(parts[3].len(), 4); // year
    assert_eq!(parts[4].len(), 2); // month
    assert_eq!(parts[5].len(), 2); // day
    assert!(parts[6].ends_with("-invoice-2026-01.txt"));

    // SHA256 + size + content-type
    assert_eq!(result.sha256, hash_buffer(&body));
    assert_eq!(result.byte_size, body.len() as u64);
    assert_eq!(result.content_type, "text/plain");

    // get
    let fetched = svc.get_object("company-A", key).await.expect("get");
    assert_eq!(fetched.body, body);

    // head
    let head = svc.head_object("company-A", key).await.expect("head");
    assert_eq!(head.object_key.as_str(), key);

    // delete
    svc.delete_object("company-A", key).await.expect("del");
    let after_delete = svc.get_object("company-A", key).await;
    assert!(matches!(
        after_delete,
        Err(ServiceError::Storage(pc_storage::StorageError::NotFound(_)))
    ));
}

#[tokio::test]
async fn e2e_put_then_other_company_get_is_forbidden() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = new_local_service(tmp.path());
    let result = svc
        .put_file(PutFileInput {
            company_id: "company-A".to_string(),
            namespace: "private".to_string(),
            content_type: "application/json".to_string(),
            body: Bytes::from_static(b"{\"secret\":1}"),
            original_filename: Some("secret.json".to_string()),
        })
        .await
        .expect("put");
    // company-B 试图读 → forbidden
    let stolen = svc
        .get_object("company-B", result.object_key.as_str())
        .await;
    assert!(matches!(stolen, Err(ServiceError::Forbidden(_))));
    let stolen = svc
        .delete_object("company-B", result.object_key.as_str())
        .await;
    assert!(matches!(stolen, Err(ServiceError::Forbidden(_))));
}

#[tokio::test]
async fn e2e_dotdot_key_is_rejected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = new_local_service(tmp.path());
    let evil = svc.get_object("company-A", "company-A/foo/../bar").await;
    assert!(matches!(evil, Err(ServiceError::BadRequest(_))));
    let evil = svc.head_object("company-A", "company-A/foo/../bar").await;
    assert!(matches!(evil, Err(ServiceError::BadRequest(_))));
}

#[tokio::test]
async fn e2e_namespace_normalization_real_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = new_local_service(tmp.path());
    // 原始 namespace `///` 应归一化为 `misc`,object key 仍落盘
    let result = svc
        .put_file(PutFileInput {
            company_id: "company-A".to_string(),
            namespace: "///".to_string(),
            content_type: "text/plain".to_string(),
            body: Bytes::from_static(b"x"),
            original_filename: None,
        })
        .await
        .expect("put");
    assert!(
        result.object_key.as_str().contains("/misc/"),
        "key should contain /misc/: {}",
        result.object_key.as_str()
    );
    // 真实文件应在磁盘上(可读回)
    let fetched = svc
        .get_object("company-A", result.object_key.as_str())
        .await
        .expect("get");
    assert_eq!(fetched.body, Bytes::from_static(b"x"));
}

#[tokio::test]
async fn e2e_original_filename_sanitized_in_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = new_local_service(tmp.path());
    let result = svc
        .put_file(PutFileInput {
            company_id: "company-A".to_string(),
            namespace: "raw".to_string(),
            content_type: "application/octet-stream".to_string(),
            body: Bytes::from_static(b"\x00\x01\x02data"),
            original_filename: Some("evil name!@#$%.bin".to_string()),
        })
        .await
        .expect("put");
    let key = result.object_key.as_str();
    // 清洗后不含非法字符
    assert!(!key.contains(' '), "space must be sanitized: {key}");
    assert!(!key.contains('!'), "exclaim must be sanitized: {key}");
    assert!(!key.contains('@'), "at must be sanitized: {key}");
    // 扩展名 bin 保留
    assert!(key.ends_with(".bin"), "ext preserved: {key}");
}

#[tokio::test]
async fn e2e_empty_body_is_unprocessable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = new_local_service(tmp.path());
    let err = svc
        .put_file(PutFileInput {
            company_id: "company-A".to_string(),
            namespace: "raw".to_string(),
            content_type: "text/plain".to_string(),
            body: Bytes::new(),
            original_filename: None,
        })
        .await;
    assert!(matches!(err, Err(ServiceError::Unprocessable(_))));
}

#[tokio::test]
async fn e2e_put_creates_real_file_on_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = new_local_service(tmp.path());
    let body = b"hello local disk";
    let result = svc
        .put_file(PutFileInput {
            company_id: "company-A".to_string(),
            namespace: "raw".to_string(),
            content_type: "text/plain".to_string(),
            body: Bytes::copy_from_slice(body),
            original_filename: Some("greet.txt".to_string()),
        })
        .await
        .expect("put");
    // 真磁盘上 object key 拼到 bucket + root 下应存在
    // (LocalDiskStorage::resolve 用 bucket + key)
    let on_disk = tmp
        .path()
        .join("default-bucket")
        .join(result.object_key.as_str());
    assert!(
        on_disk.exists(),
        "object file must exist on disk: {on_disk:?}"
    );
    let read = std::fs::read(&on_disk).expect("read");
    assert_eq!(read, body);
    // metadata sidecar (LocalDiskStorage 特性,非 service 责任 — 但作为回归验证存在)
    let meta = on_disk.with_extension("meta");
    assert!(meta.exists(), "sidecar meta must exist: {meta:?}");
    let meta_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
    assert_eq!(meta_json["sha256"].as_str().unwrap(), hash_buffer(body));
}

#[test]
fn unit_sanitize_segment_matches_node_implementation() {
    // 与 Node `sanitizeSegment` 1:1 对齐
    assert_eq!(sanitize_segment("hello world"), "hello_world");
    assert_eq!(sanitize_segment("  hello!!!world  "), "hello_world");
    assert_eq!(sanitize_segment("__hello__world__"), "hello_world");
    assert_eq!(sanitize_segment(""), "file");
    assert_eq!(sanitize_segment("   "), "file");
    assert_eq!(
        sanitize_segment("valid-file_name.ext"),
        "valid-file_name.ext"
    );
    // 折叠多个 _
    assert_eq!(sanitize_segment("a!!!b???c"), "a_b_c");
    assert_eq!(sanitize_segment("a   b"), "a_b");
}

#[test]
fn unit_normalize_namespace_matches_node() {
    assert_eq!(normalize_namespace("a/b/c"), "a/b/c");
    assert_eq!(normalize_namespace("/a//b/"), "a/b");
    assert_eq!(normalize_namespace("///"), "misc");
    assert_eq!(
        normalize_namespace("hello world/foo bar"),
        "hello_world/foo_bar"
    );
    assert_eq!(normalize_namespace(""), "misc");
    assert_eq!(normalize_namespace("   /   "), "misc");
}

#[test]
fn unit_split_filename_matches_node() {
    assert_eq!(
        split_filename(Some("foo.txt")),
        ("foo".to_string(), "txt".to_string())
    );
    assert_eq!(
        split_filename(Some("foo.bar.tar.gz")),
        ("foo.bar.tar".to_string(), "gz".to_string())
    );
    assert_eq!(split_filename(None), ("file".to_string(), String::new()));
    assert_eq!(
        split_filename(Some("")),
        ("file".to_string(), String::new())
    );
    assert_eq!(
        split_filename(Some("IMG.JPG")),
        ("IMG".to_string(), "jpg".to_string())
    );
    assert_eq!(
        split_filename(Some("/path/to/file.md")),
        ("file".to_string(), "md".to_string())
    );
}

#[test]
fn unit_ensure_company_prefix_returns_correct_error_types() {
    assert!(ensure_company_prefix("co1", "co1/foo").is_ok());
    let err = ensure_company_prefix("co1", "co2/foo").unwrap_err();
    assert!(matches!(err, ServiceError::Forbidden(_)));
    let err = ensure_company_prefix("co1", "co1/foo/../bar").unwrap_err();
    assert!(matches!(err, ServiceError::BadRequest(_)));
}
