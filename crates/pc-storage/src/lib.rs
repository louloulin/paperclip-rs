#![forbid(unsafe_code)]

//! 对象存储抽象层：本地磁盘 + `S3` 兼容 provider。
//!
//! 与原 paperclip `server/src/storage/` 等价：
//! - `StorageProvider` trait 统一接口
//! - `LocalDiskStorage`：根目录 + 内容寻址（SHA-256）
//! - `S3Storage`：`S3` 兼容（stub，返回 `NotImplemented` 由 host 决定）
//! - `StorageRegistry`：多 provider 并存

pub mod error;
pub mod local_disk;
pub mod provider;
pub mod registry;
pub mod s3;
pub mod service;
pub mod types;

pub use error::StorageError;
pub use local_disk::LocalDiskStorage;
pub use provider::{ObjectMetadata, StorageProvider};
pub use registry::StorageRegistry;
pub use s3::S3Storage;
pub use service::{
    build_object_key, ensure_company_prefix, hash_buffer, normalize_namespace, sanitize_segment,
    split_filename,
};
pub use service::{
    ObjectBody, ObjectHead, PutFileInput, PutFileResult, ServiceError, StorageService,
};
pub use types::{ObjectKey, PresignedUrl, StorageClass, StorageLocation};
