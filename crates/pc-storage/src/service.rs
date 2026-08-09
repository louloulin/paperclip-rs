//! `pc-storage::service` —— 上层业务用的存储服务层。
//!
//! 与 Node `server/src/storage/service.ts` 1:1 对齐:
//! - `put_file`: 校验输入 + 归一化 namespace + 拆分文件名(stem/ext) + 生成日期分层 object key + SHA256 摘要
//! - `get_object`/`head_object`/`delete_object`: 强制 company prefix + 拒绝 `..`
//!
//! 高内聚: 不感知具体 provider(provider 通过 `StorageProvider` trait 注入)。
//! 上层 (如 HTTP 路由 / 业务用例) 通过 `StorageService::new(provider)` 拿实例。

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::provider::{ObjectMetadata, StorageProvider};
use crate::types::{ObjectKey, StorageClass, StorageLocation};

/// 单段文件名最大长度(与 Node `MAX_SEGMENT_LENGTH = 120` 等价)。
pub const MAX_SEGMENT_LENGTH: usize = 120;
/// 扩展名最大长度(与 Node `slice(0, 16)` 等价)。
pub const MAX_EXTENSION_LENGTH: usize = 16;

/// 业务层 put 输入(与 Node `PutFileInput` 等价)。
#[derive(Debug, Clone)]
pub struct PutFileInput {
    pub company_id: String,
    pub namespace: String,
    pub content_type: String,
    pub body: Bytes,
    pub original_filename: Option<String>,
}

/// 业务层 put 输出(与 Node `PutFileResult` 等价)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutFileResult {
    pub provider: String,
    pub object_key: ObjectKey,
    pub content_type: String,
    pub byte_size: u64,
    pub sha256: String,
    pub original_filename: Option<String>,
}

/// service 层错误(向上抛出 → HTTP 层映射)。
#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unprocessable: {0}")]
    Unprocessable(String),
    #[error("storage: {0}")]
    Storage(#[from] crate::error::StorageError),
}

/// 业务层对象元数据 head 返回(与 Node `headObject` 等价)。
#[derive(Debug, Clone)]
pub struct ObjectHead {
    pub metadata: ObjectMetadata,
    pub object_key: ObjectKey,
}

/// 业务层对象下载内容(与 Node `getObject` 等价)。
#[derive(Debug, Clone)]
pub struct ObjectBody {
    pub metadata: ObjectMetadata,
    pub body: Bytes,
}

/// 业务层存储服务:封装命名规则 + 校验 + provider 路由。
#[derive(Debug, Clone)]
pub struct StorageService {
    provider: Arc<dyn StorageProvider>,
    bucket: String,
}

impl StorageService {
    /// 构造 service(Node `createStorageService(provider)` 等价)。
    #[must_use]
    pub fn new(provider: Arc<dyn StorageProvider>, bucket: impl Into<String>) -> Self {
        Self {
            provider,
            bucket: bucket.into(),
        }
    }

    /// 暴露 provider id,用于 `PutFileResult.provider`。
    #[must_use]
    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// 默认 bucket 名。
    #[must_use]
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// 上传一个文件(Node `putFile` 等价)。
    pub async fn put_file(&self, input: PutFileInput) -> Result<PutFileResult, ServiceError> {
        assert_put_file_input(&input)?;
        let object_key = build_object_key(
            &input.company_id,
            &input.namespace,
            input.original_filename.as_deref(),
        );
        let location = StorageLocation {
            bucket: self.bucket.clone(),
            key: ObjectKey::new(object_key.clone()),
        };
        let content_type = input.content_type.trim().to_lowercase();
        let byte_size = input.body.len() as u64;
        let _metadata = self
            .provider
            .put_object(&location, input.body.clone(), Some(&content_type))
            .await?;
        let sha256 = hash_buffer(&input.body);
        Ok(PutFileResult {
            provider: self.provider.name().to_string(),
            object_key: ObjectKey::new(object_key),
            content_type,
            byte_size,
            sha256,
            original_filename: input.original_filename,
        })
    }

    /// 读取对象(Node `getObject` 等价)。
    pub async fn get_object(
        &self,
        company_id: &str,
        object_key: &str,
    ) -> Result<ObjectBody, ServiceError> {
        ensure_company_prefix(company_id, object_key)?;
        let location = StorageLocation {
            bucket: self.bucket.clone(),
            key: ObjectKey::new(object_key.to_string()),
        };
        let bytes = self.provider.get_object(&location).await?;
        // 从 provider 视角回填 metadata size/content_type(简化版,无 sha 计算)
        let metadata = ObjectMetadata {
            key: ObjectKey::new(object_key.to_string()),
            size: bytes.len() as u64,
            content_type: None,
            content_sha256: None,
            last_modified: Utc::now(),
            class: StorageClass::Hot,
        };
        Ok(ObjectBody {
            metadata,
            body: bytes,
        })
    }

    /// 检查对象元数据(Node `headObject` 等价)。
    pub async fn head_object(
        &self,
        company_id: &str,
        object_key: &str,
    ) -> Result<ObjectHead, ServiceError> {
        ensure_company_prefix(company_id, object_key)?;
        let location = StorageLocation {
            bucket: self.bucket.clone(),
            key: ObjectKey::new(object_key.to_string()),
        };
        // provider 没有独立 head_object,这里用 stream_object 触发 NotFound
        let _ = self.provider.stream_object(&location).await?;
        let metadata = ObjectMetadata {
            key: ObjectKey::new(object_key.to_string()),
            size: 0,
            content_type: None,
            content_sha256: None,
            last_modified: Utc::now(),
            class: StorageClass::Hot,
        };
        Ok(ObjectHead {
            metadata,
            object_key: ObjectKey::new(object_key.to_string()),
        })
    }

    /// 删除对象(Node `deleteObject` 等价)。
    pub async fn delete_object(
        &self,
        company_id: &str,
        object_key: &str,
    ) -> Result<(), ServiceError> {
        ensure_company_prefix(company_id, object_key)?;
        let location = StorageLocation {
            bucket: self.bucket.clone(),
            key: ObjectKey::new(object_key.to_string()),
        };
        self.provider.delete_object(&location).await?;
        Ok(())
    }
}

// ===== Pure helpers (Node 等价) =====

/// 单段清洗(`[^a-zA-Z0-9._-]+` → `_`,合并 `__`,去首尾 `_`,空时 fallback `"file"`,截 120)。
pub fn sanitize_segment(value: &str) -> String {
    let trimmed = value.trim();
    // Stage 1: 连续非法字符组 → 单个 `_`(精确镜像 Node `replace(/[^a-zA-Z0-9._-]+/g, "_")`)。
    let mut stage1 = String::with_capacity(trimmed.len());
    let mut in_invalid_run = false;
    for c in trimmed.chars() {
        let valid = c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
        if valid {
            stage1.push(c);
            in_invalid_run = false;
        } else if !in_invalid_run {
            stage1.push('_');
            in_invalid_run = true;
        }
    }
    // Stage 2: 折叠 `__+` → `_`(与 Node `replace(/_{2,}/g, "_")` 等价)。
    let mut stage2 = String::with_capacity(stage1.len());
    let mut prev_underscore = false;
    for c in stage1.chars() {
        if c == '_' {
            if !prev_underscore {
                stage2.push('_');
                prev_underscore = true;
            }
        } else {
            stage2.push(c);
            prev_underscore = false;
        }
    }
    // Stage 3: 去首尾 `_`(Node `replace(/^_+|_+$/g, "")`)。
    let final_str = stage2.trim_matches('_');
    let result = if final_str.is_empty() {
        "file".to_string()
    } else {
        final_str.to_string()
    };
    if result.len() > MAX_SEGMENT_LENGTH {
        result[..MAX_SEGMENT_LENGTH].to_string()
    } else {
        result
    }
}

/// namespace 归一化(Node `normalizeNamespace` 等价:split + trim + 过滤空 + sanitize_segment 每段,空则 "misc")。
pub fn normalize_namespace(namespace: &str) -> String {
    // 1. split + trim + 过滤空段(精确镜像 Node 的 `split('/').map(trim).filter(len > 0)`)。
    let parts: Vec<&str> = namespace
        .split('/')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return "misc".to_string();
    }
    // 2. sanitize 每段(此时 trim 后输入一定非空,所以 sanitize_segment 不会回到 "file" fallback)。
    let normalized: Vec<String> = parts.iter().map(|s| sanitize_segment(s)).collect();
    normalized.join("/")
}

/// 拆分文件名 → (stem, ext)。
///
/// Node 等价:`path.basename(filename)` 后 `path.extname` 取扩展名,
/// stem 用 `sanitizeSegment`,ext 转小写 + `[^a-z0-9.]` → "" + 截 16。
pub fn split_filename(filename: Option<&str>) -> (String, String) {
    let Some(name) = filename else {
        return ("file".to_string(), String::new());
    };
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim();
    if base.is_empty() {
        return ("file".to_string(), String::new());
    }
    let ext_raw = std::path::Path::new(base)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let stem_raw = if ext_raw.is_empty() {
        base
    } else {
        &base[..base.len() - ext_raw.len() - 1]
    };
    let stem = sanitize_segment(stem_raw);
    let ext = ext_raw.to_lowercase();
    let ext_cleaned: String = ext
        .chars()
        .filter(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || *c == '.')
        .collect();
    let ext_truncated = if ext_cleaned.len() > MAX_EXTENSION_LENGTH {
        ext_cleaned[..MAX_EXTENSION_LENGTH].to_string()
    } else {
        ext_cleaned
    };
    (stem, ext_truncated)
}

/// 构造 object key: `{company_id}/{ns}/{YYYY}/{MM}/{DD}/{uuid}-{stem}{ext}`。
pub fn build_object_key(
    company_id: &str,
    namespace: &str,
    original_filename: Option<&str>,
) -> String {
    let ns = normalize_namespace(namespace);
    let now = Utc::now();
    let year = now.format("%Y").to_string();
    let month = now.format("%m").to_string();
    let day = now.format("%d").to_string();
    let (stem, ext) = split_filename(original_filename);
    let suffix = Uuid::new_v4();
    let filename = if ext.is_empty() {
        format!("{suffix}-{stem}")
    } else {
        format!("{suffix}-{stem}.{ext}")
    };
    format!("{company_id}/{ns}/{year}/{month}/{day}/{filename}")
}

/// 校验 object_key 以 company_id 开头 + 拒绝 `..`。
pub fn ensure_company_prefix(company_id: &str, object_key: &str) -> Result<(), ServiceError> {
    let expected_prefix = format!("{company_id}/");
    if !object_key.starts_with(&expected_prefix) {
        return Err(ServiceError::Forbidden(
            "Object does not belong to company".to_string(),
        ));
    }
    if object_key.contains("..") {
        return Err(ServiceError::BadRequest("Invalid object key".to_string()));
    }
    Ok(())
}

/// SHA256 摘要(与 Node `hashBuffer` 等价)。
pub fn hash_buffer(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

fn assert_put_file_input(input: &PutFileInput) -> Result<(), ServiceError> {
    if input.company_id.trim().is_empty() {
        return Err(ServiceError::Unprocessable(
            "companyId is required".to_string(),
        ));
    }
    if input.namespace.trim().is_empty() {
        return Err(ServiceError::Unprocessable(
            "namespace is required".to_string(),
        ));
    }
    if input.content_type.trim().is_empty() {
        return Err(ServiceError::Unprocessable(
            "contentType is required".to_string(),
        ));
    }
    if input.body.is_empty() {
        return Err(ServiceError::Unprocessable("File is empty".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{StorageError, StorageResult};

    // --- Pure helpers ---

    #[test]
    fn sanitize_segment_strips_invalid_and_collapses_underscores() {
        assert_eq!(sanitize_segment("hello world"), "hello_world");
        assert_eq!(sanitize_segment("  hello!!!world  "), "hello_world");
        assert_eq!(sanitize_segment("__hello__world__"), "hello_world");
        assert_eq!(sanitize_segment(""), "file");
        assert_eq!(sanitize_segment("   "), "file");
    }

    #[test]
    fn sanitize_segment_truncates_to_max_length() {
        let s = "a".repeat(200);
        let r = sanitize_segment(&s);
        assert_eq!(r.len(), MAX_SEGMENT_LENGTH);
    }

    #[test]
    fn normalize_namespace_joins_segments_with_defaults() {
        assert_eq!(normalize_namespace("a/b/c"), "a/b/c");
        assert_eq!(normalize_namespace("/a//b/"), "a/b");
        assert_eq!(normalize_namespace("///"), "misc");
        assert_eq!(
            normalize_namespace("hello world/foo bar"),
            "hello_world/foo_bar"
        );
    }

    #[test]
    fn split_filename_extracts_stem_and_ext() {
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
        // 大写扩展名 → 小写
        assert_eq!(
            split_filename(Some("IMG.JPG")),
            ("IMG".to_string(), "jpg".to_string())
        );
    }

    #[test]
    fn split_filename_truncates_long_extension() {
        let long_ext = "a".repeat(30);
        let filename = format!("name.{long_ext}");
        let (_stem, ext) = split_filename(Some(&filename));
        assert_eq!(ext.len(), MAX_EXTENSION_LENGTH);
    }

    #[test]
    fn ensure_company_prefix_enforces_prefix_and_no_dotdot() {
        assert!(ensure_company_prefix("co1", "co1/folder/file.txt").is_ok());
        assert!(matches!(
            ensure_company_prefix("co1", "other/folder/file.txt"),
            Err(ServiceError::Forbidden(_))
        ));
        assert!(matches!(
            ensure_company_prefix("co1", "co1/../etc/passwd"),
            Err(ServiceError::BadRequest(_))
        ));
    }

    #[test]
    fn hash_buffer_is_deterministic_and_hex() {
        let h1 = hash_buffer(b"hello");
        let h2 = hash_buffer(b"hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn build_object_key_matches_expected_layout() {
        let key = build_object_key("co1", "docs/invoices", Some("report.pdf"));
        assert!(key.starts_with("co1/docs/invoices/"));
        // namespace 是多段,所以 parts 数 = 1 (co1) + 2 (ns) + 3 (date) + 1 (file) = 7
        let parts: Vec<&str> = key.split('/').collect();
        assert_eq!(parts.len(), 7, "companyId/ns/yyyy/mm/dd/file");
        assert_eq!(parts[3].len(), 4, "year");
        assert_eq!(parts[4].len(), 2, "month");
        assert_eq!(parts[5].len(), 2, "day");
        assert!(parts[6].ends_with("-report.pdf"));
    }

    #[test]
    fn build_object_key_without_ext_omits_dot() {
        let key = build_object_key("co1", "raw", Some("data"));
        assert!(key.ends_with("-data"));
    }

    #[test]
    fn build_object_key_with_namespace_normalization() {
        let key = build_object_key("co1", "///", Some("x.txt"));
        assert!(key.starts_with("co1/misc/"));
    }

    #[test]
    fn assert_put_file_input_validates_required_fields() {
        let mut input = PutFileInput {
            company_id: "co1".to_string(),
            namespace: "docs".to_string(),
            content_type: "text/plain".to_string(),
            body: Bytes::from_static(b"hello"),
            original_filename: None,
        };
        assert!(assert_put_file_input(&input).is_ok());

        input.company_id = " ".to_string();
        assert!(matches!(
            assert_put_file_input(&input),
            Err(ServiceError::Unprocessable(_))
        ));
        input.company_id = "co1".to_string();

        input.namespace = "".to_string();
        assert!(matches!(
            assert_put_file_input(&input),
            Err(ServiceError::Unprocessable(_))
        ));
        input.namespace = "docs".to_string();

        input.content_type = "".to_string();
        assert!(matches!(
            assert_put_file_input(&input),
            Err(ServiceError::Unprocessable(_))
        ));
        input.content_type = "text/plain".to_string();

        input.body = Bytes::new();
        assert!(matches!(
            assert_put_file_input(&input),
            Err(ServiceError::Unprocessable(_))
        ));
    }

    // --- Mock provider integration tests (no real disk) ---

    #[derive(Debug)]
    struct MockProvider {
        store: tokio::sync::Mutex<std::collections::HashMap<String, (Bytes, Option<String>)>>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                store: tokio::sync::Mutex::new(Default::default()),
            }
        }
    }

    #[async_trait::async_trait]
    impl StorageProvider for MockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn put_object(
            &self,
            target: &StorageLocation,
            bytes: Bytes,
            content_type: Option<&str>,
        ) -> StorageResult<ObjectMetadata> {
            self.store.lock().await.insert(
                target.key.as_str().to_string(),
                (bytes, content_type.map(str::to_string)),
            );
            Ok(ObjectMetadata {
                key: target.key.clone(),
                size: target.key.as_str().len() as u64,
                content_type: content_type.map(str::to_string),
                content_sha256: None,
                last_modified: Utc::now(),
                class: StorageClass::Hot,
            })
        }

        async fn get_object(&self, location: &StorageLocation) -> StorageResult<Bytes> {
            self.store
                .lock()
                .await
                .get(location.key.as_str())
                .map(|(b, _)| b.clone())
                .ok_or_else(|| StorageError::NotFound(location.key.as_str().to_string()))
        }

        async fn stream_object(
            &self,
            location: &StorageLocation,
        ) -> StorageResult<crate::provider::ObjectStream> {
            let b = self.get_object(location).await?;
            use futures::stream;
            Ok(Box::pin(stream::once(async move { Ok(b) })))
        }

        async fn delete_object(&self, location: &StorageLocation) -> StorageResult<()> {
            self.store.lock().await.remove(location.key.as_str());
            Ok(())
        }

        async fn list_prefix(&self, _bucket: &str, _prefix: &str) -> StorageResult<Vec<ObjectKey>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn service_put_get_delete_round_trip() {
        let provider = Arc::new(MockProvider::new());
        let svc = StorageService::new(provider, "default-bucket");
        let body = Bytes::from_static(b"hello world");
        let result = svc
            .put_file(PutFileInput {
                company_id: "co1".to_string(),
                namespace: "docs".to_string(),
                content_type: "text/plain".to_string(),
                body: body.clone(),
                original_filename: Some("hello.txt".to_string()),
            })
            .await
            .expect("put");
        assert_eq!(result.provider, "mock");
        assert!(result.object_key.as_str().starts_with("co1/docs/"));
        assert!(result.object_key.as_str().ends_with("-hello.txt"));
        assert_eq!(result.byte_size, body.len() as u64);
        assert_eq!(result.sha256, hash_buffer(&body));
        assert_eq!(result.original_filename.as_deref(), Some("hello.txt"));

        // get_object
        let fetched = svc
            .get_object("co1", result.object_key.as_str())
            .await
            .expect("get");
        assert_eq!(fetched.body, body);

        // head_object
        let head = svc
            .head_object("co1", result.object_key.as_str())
            .await
            .expect("head");
        assert_eq!(head.object_key.as_str(), result.object_key.as_str());

        // delete_object
        svc.delete_object("co1", result.object_key.as_str())
            .await
            .expect("del");

        // get after delete → NotFound (translated to StorageError)
        let after = svc.get_object("co1", result.object_key.as_str()).await;
        assert!(matches!(
            after,
            Err(ServiceError::Storage(StorageError::NotFound(_)))
        ));
    }

    #[tokio::test]
    async fn service_get_object_rejects_other_company_key() {
        let provider = Arc::new(MockProvider::new());
        let svc = StorageService::new(provider, "default-bucket");
        // 在 co1 下 put,然后 co2 试图 get → forbidden
        let result = svc
            .put_file(PutFileInput {
                company_id: "co1".to_string(),
                namespace: "docs".to_string(),
                content_type: "text/plain".to_string(),
                body: Bytes::from_static(b"x"),
                original_filename: None,
            })
            .await
            .expect("put");
        let err = svc.get_object("co2", result.object_key.as_str()).await;
        assert!(matches!(err, Err(ServiceError::Forbidden(_))));
    }

    #[tokio::test]
    async fn service_get_object_rejects_dotdot_key() {
        let provider = Arc::new(MockProvider::new());
        let svc = StorageService::new(provider, "default-bucket");
        let err = svc.get_object("co1", "co1/foo/../bar.txt").await;
        assert!(matches!(err, Err(ServiceError::BadRequest(_))));
    }

    #[tokio::test]
    async fn service_put_file_rejects_empty_body() {
        let provider = Arc::new(MockProvider::new());
        let svc = StorageService::new(provider, "default-bucket");
        let err = svc
            .put_file(PutFileInput {
                company_id: "co1".to_string(),
                namespace: "docs".to_string(),
                content_type: "text/plain".to_string(),
                body: Bytes::new(),
                original_filename: None,
            })
            .await;
        assert!(matches!(err, Err(ServiceError::Unprocessable(_))));
    }

    #[tokio::test]
    async fn service_provider_name_and_bucket_round_trip() {
        let provider = Arc::new(MockProvider::new());
        let svc = StorageService::new(provider, "my-bucket");
        assert_eq!(svc.provider_name(), "mock");
        assert_eq!(svc.bucket(), "my-bucket");
    }
}
