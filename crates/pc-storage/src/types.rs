//! 存储数据结构。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct ObjectKey(pub String);

impl ObjectKey {
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 对象的逻辑位置（provider-local）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocation {
    pub bucket: String,
    pub key: ObjectKey,
}

/// 存储分级（影响成本/可用性）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageClass {
    Hot,
    Cold,
    Archive,
}

impl StorageClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Cold => "cold",
            Self::Archive => "archive",
        }
    }
}

/// 预签名下载 URL（仅 S3 类 provider 支持）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresignedUrl {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

/// 给上层决定把对象写到哪个 provider。
#[derive(Debug, Clone)]
pub struct WriteTarget {
    pub location: StorageLocation,
    pub class: StorageClass,
}

impl WriteTarget {
    #[must_use]
    pub fn new(bucket: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            location: StorageLocation {
                bucket: bucket.into(),
                key: ObjectKey::new(key),
            },
            class: StorageClass::Hot,
        }
    }
}

/// 仅本地 provider 使用：磁盘根目录。
#[derive(Debug, Clone)]
pub struct LocalRoot(pub PathBuf);

impl LocalRoot {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self(root)
    }
}
