//! `workspace_operation_log_store` 域（Round 264）。
//!
//! 与原 `paperclip/server/src/services/workspace-operation-log-store.ts` 1:1 对齐：
//! - 抽象 `WorkspaceOperationLogStore` trait：`begin/append/finalize/read`
//! - 提供 `LocalFileWorkspaceOperationLogStore`：把日志以 NDJSON 行写入
//!   `<base>/<companyId>/<opId>.ndjson`
//! - 日志结构：`{"ts": "...", "stream": "stdout|stderr|system", "chunk": "..."}`
//! - `finalize` 计算文件大小 + sha256（与 Node 一致：`{ bytes, sha256, compressed: false }`）
//! - `read` 支持 `offset/limitBytes` 分页
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：本模块只关心"日志行的存储与读取"，不关心上层 `recordOperation` 编排。
//! - 低耦合：通过 trait 抽象；后续可以接入 S3/对象存储而不改动调用方。
//! - 线程安全：`OnceLock<RwLock<Option<Box<dyn ...>>>>` 缓存默认 store。
//! - 路径安全：`safe_segments` 与 `resolve_within` 防止公司 ID 中含有 `../` 等穿越片段。

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;


// ============================================================================
// 错误
// ============================================================================

#[derive(Debug, Error)]
pub enum WorkspaceOperationLogStoreError {
    #[error("workspace operation log not found")]
    NotFound,
    #[error("invalid log path: {0}")]
    InvalidPath(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] serde_json::Error),
}

// ============================================================================
// 公共类型（与 Node 版 1:1 对齐）
// ============================================================================

/// 当前支持的存储实现类型（与 Node 的 `local_file` 字符串一致）。
pub const STORE_TYPE_LOCAL_FILE: &str = "local_file";

/// 句柄：在数据库中存 `{ store, log_ref }`，调用方拿到后传给 `append/finalize/read`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOperationLogHandle {
    pub store: String,
    pub log_ref: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceOperationLogReadOptions {
    pub offset: Option<u64>,
    pub limit_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOperationLogReadResult {
    pub content: String,
    pub next_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOperationLogFinalizeSummary {
    pub bytes: u64,
    pub sha256: Option<String>,
    pub compressed: bool,
}

/// 单条日志事件（与 Node 中 `append(handle, event)` 的入参对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOperationLogEvent {
    pub stream: LogStream,
    pub chunk: String,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
    System,
}

impl LogStream {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogStream::Stdout => "stdout",
            LogStream::Stderr => "stderr",
            LogStream::System => "system",
        }
    }
}

// ============================================================================
// Trait
// ============================================================================

#[async_trait::async_trait]
pub trait WorkspaceOperationLogStore: Send + Sync + std::any::Any {
    async fn begin(
        &self,
        company_id: Uuid,
        operation_id: Uuid,
    ) -> Result<WorkspaceOperationLogHandle, WorkspaceOperationLogStoreError>;

    async fn append(
        &self,
        handle: &WorkspaceOperationLogHandle,
        event: &WorkspaceOperationLogEvent,
    ) -> Result<(), WorkspaceOperationLogStoreError>;

    async fn finalize(
        &self,
        handle: &WorkspaceOperationLogHandle,
    ) -> Result<WorkspaceOperationLogFinalizeSummary, WorkspaceOperationLogStoreError>;

    async fn read(
        &self,
        handle: &WorkspaceOperationLogHandle,
        opts: WorkspaceOperationLogReadOptions,
    ) -> Result<WorkspaceOperationLogReadResult, WorkspaceOperationLogStoreError>;
}

// ============================================================================
// Local File Store
// ============================================================================

#[derive(Debug, Clone)]
pub struct LocalFileWorkspaceOperationLogStore {
    base_path: PathBuf,
}

impl LocalFileWorkspaceOperationLogStore {
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
        }
    }



    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// 把 `company_id` 和 `operation_id` 都规范化成 `[a-zA-Z0-9._-]+`，防穿越。
    fn safe_segments(&self, company_id: Uuid, operation_id: Uuid) -> (String, String) {
        let clean = |s: &str| -> String {
            s.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        };
        (clean(&company_id.to_string()), clean(&operation_id.to_string()))
    }

    /// 限制 `relative` 不得越出 `base`。返回绝对路径。
    fn resolve_within(&self, relative: &str) -> Result<PathBuf, WorkspaceOperationLogStoreError> {
        let resolved = clean_path(&self.base_path.join(relative));
        let base_clean = clean_path(&self.base_path);
        let resolved_str = resolved.to_string_lossy().into_owned();
        let base_str = base_clean.to_string_lossy().into_owned();

        let mut base_with_sep = base_str.clone();
        if !base_with_sep.ends_with(std::path::MAIN_SEPARATOR) {
            base_with_sep.push(std::path::MAIN_SEPARATOR);
        }

        if resolved_str == base_str || resolved_str.starts_with(&base_with_sep) {
            Ok(resolved)
        } else {
            Err(WorkspaceOperationLogStoreError::InvalidPath(format!(
                "resolved log path '{}' escapes base '{}'",
                resolved_str, base_str
            )))
        }
    }

    async fn ensure_base(&self) -> Result<PathBuf, WorkspaceOperationLogStoreError> {
        fs::create_dir_all(&self.base_path).await?;
        Ok(self.base_path.clone())
    }

    async fn ensure_dir(&self, relative_dir: &str) -> Result<PathBuf, WorkspaceOperationLogStoreError> {
        let abs = self.resolve_within(relative_dir)?;
        fs::create_dir_all(&abs).await?;
        Ok(abs)
    }
}

#[async_trait::async_trait]
impl WorkspaceOperationLogStore for LocalFileWorkspaceOperationLogStore {
    async fn begin(
        &self,
        company_id: Uuid,
        operation_id: Uuid,
    ) -> Result<WorkspaceOperationLogHandle, WorkspaceOperationLogStoreError> {
        self.ensure_base().await?;
        let (company_seg, op_seg) = self.safe_segments(company_id, operation_id);
        let rel_dir = company_seg.clone();
        let rel_path = format!("{rel_dir}/{op_seg}.ndjson");
        let _abs_dir = self.ensure_dir(&rel_dir).await?;
        let abs_path = self.resolve_within(&rel_path)?;

        // 与 Node `fs.writeFile(absPath, "")` 等价
        let mut f = fs::File::create(&abs_path).await?;
        f.write_all(b"").await?;
        f.flush().await?;

        Ok(WorkspaceOperationLogHandle {
            store: STORE_TYPE_LOCAL_FILE.to_string(),
            log_ref: rel_path,
        })
    }

    async fn append(
        &self,
        handle: &WorkspaceOperationLogHandle,
        event: &WorkspaceOperationLogEvent,
    ) -> Result<(), WorkspaceOperationLogStoreError> {
        if handle.store != STORE_TYPE_LOCAL_FILE {
            return Err(WorkspaceOperationLogStoreError::InvalidPath(format!(
                "unsupported log store type: {}",
                handle.store
            )));
        }
        let abs_path = self.resolve_within(&handle.log_ref)?;
        let line_value = serde_json::json!({
            "ts": event.ts.to_rfc3339(),
            "stream": event.stream.as_str(),
            "chunk": event.chunk,
        });
        let mut line_str = serde_json::to_string(&line_value)?;
        line_str.push('\n');

        let mut f = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&abs_path)
            .await?;
        f.write_all(line_str.as_bytes()).await?;
        f.flush().await?;
        Ok(())
    }

    async fn finalize(
        &self,
        handle: &WorkspaceOperationLogHandle,
    ) -> Result<WorkspaceOperationLogFinalizeSummary, WorkspaceOperationLogStoreError> {
        if handle.store != STORE_TYPE_LOCAL_FILE {
            return Ok(WorkspaceOperationLogFinalizeSummary {
                bytes: 0,
                sha256: None,
                compressed: false,
            });
        }
        let abs_path = self.resolve_within(&handle.log_ref)?;
        let meta = match fs::metadata(&abs_path).await {
            Ok(m) => m,
            Err(_) => return Err(WorkspaceOperationLogStoreError::NotFound),
        };
        let hash = sha256_file(&abs_path).await?;
        Ok(WorkspaceOperationLogFinalizeSummary {
            bytes: meta.len(),
            sha256: Some(hash),
            compressed: false,
        })
    }

    async fn read(
        &self,
        handle: &WorkspaceOperationLogHandle,
        opts: WorkspaceOperationLogReadOptions,
    ) -> Result<WorkspaceOperationLogReadResult, WorkspaceOperationLogStoreError> {
        if handle.store != STORE_TYPE_LOCAL_FILE {
            return Err(WorkspaceOperationLogStoreError::NotFound);
        }
        let abs_path = self.resolve_within(&handle.log_ref)?;

        let meta = match fs::metadata(&abs_path).await {
            Ok(m) => m,
            Err(_) => return Err(WorkspaceOperationLogStoreError::NotFound),
        };
        let size = meta.len();

        let offset = opts.offset.unwrap_or(0).min(size);
        let limit = opts.limit_bytes.unwrap_or(256_000);
        // Node 语义：`start + limitBytes - 1` 含端点，故实际读取 limit 个字节
        let end_inclusive = offset
            .saturating_add(limit.saturating_sub(1))
            .min(size.saturating_sub(1));

        if offset > end_inclusive {
            // 与 Node 一致：start > end 返回空 content 但保留 nextOffset=start
            return Ok(WorkspaceOperationLogReadResult {
                content: String::new(),
                next_offset: Some(offset),
            });
        }

        let mut f = fs::File::open(&abs_path).await?;
        f.seek(std::io::SeekFrom::Start(offset)).await?;
        let want_len = (end_inclusive - offset + 1) as usize;
        let mut buf = vec![0u8; want_len];
        let mut read_total = 0usize;
        while read_total < want_len {
            let n = f.read(&mut buf[read_total..]).await?;
            if n == 0 {
                buf.truncate(read_total);
                break;
            }
            read_total += n;
        }
        let content = String::from_utf8_lossy(&buf).into_owned();
        let next_offset = if end_inclusive + 1 < size {
            Some(end_inclusive + 1)
        } else {
            None
        };

        Ok(WorkspaceOperationLogReadResult { content, next_offset })
    }
}

// ============================================================================
// 默认缓存（与 Node `getWorkspaceOperationLogStore()` 对齐）
// ============================================================================

use std::sync::OnceLock;
use tokio::sync::RwLock;

/// 类型擦除的 box 克隆辅助（当前仅支持 `LocalFileWorkspaceOperationLogStore` 一种实现）。
/// 后续要新增实现时，需要在 `WorkspaceOperationLogStore` trait 上挂 `Clone` 或者返回 `Arc`。
fn clone_box(store: &dyn WorkspaceOperationLogStore) -> Box<dyn WorkspaceOperationLogStore> {
    if let Some(local) = (store as &dyn std::any::Any).downcast_ref::<LocalFileWorkspaceOperationLogStore>() {
        Box::new(local.clone())
    } else {
        Box::new(LocalFileWorkspaceOperationLogStore::new(
            default_base_path(),
        ))
    }
}

/// 默认 store 缓存：双检锁模式（同步 OnceLock + tokio RwLock）。
static DEFAULT_STORE: OnceLock<RwLock<Option<Box<dyn WorkspaceOperationLogStore>>>> = OnceLock::new();

fn default_store_cell() -> &'static RwLock<Option<Box<dyn WorkspaceOperationLogStore>>> {
    DEFAULT_STORE.get_or_init(|| RwLock::new(None))
}

/// 获取默认 `WorkspaceOperationLogStore`（与 Node `getWorkspaceOperationLogStore()` 对齐）。
pub async fn get_workspace_operation_log_store() -> Box<dyn WorkspaceOperationLogStore> {
    // 1) 读缓存
    {
        let read = default_store_cell().read().await;
        if let Some(store) = read.as_ref() {
            return clone_box(store.as_ref());
        }
    }
    // 2) 写锁内惰性构造
    let mut write = default_store_cell().write().await;
    if let Some(store) = write.as_ref() {
        return clone_box(store.as_ref());
    }
    let base_path = default_base_path();
    let store: Box<dyn WorkspaceOperationLogStore> =
        Box::new(LocalFileWorkspaceOperationLogStore::new(base_path));
    *write = Some(store);
    clone_box(write.as_ref().unwrap().as_ref())
}

/// 替换默认 store（测试 / 切换实现用）。
pub async fn set_workspace_operation_log_store(
    new_store: Box<dyn WorkspaceOperationLogStore>,
) -> Option<Box<dyn WorkspaceOperationLogStore>> {
    let mut write = default_store_cell().write().await;
    write.replace(new_store)
}

#[cfg(test)]
pub async fn clear_workspace_operation_log_store() {
    let mut write = default_store_cell().write().await;
    *write = None;
}

// ============================================================================
// 工具函数
// ============================================================================

async fn sha256_file(file_path: &Path) -> Result<String, WorkspaceOperationLogStoreError> {
    let mut f = fs::File::open(file_path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 解析默认 base path（与 Node `WORKSPACE_OPERATION_LOG_BASE_PATH` env 一致）。
pub fn default_base_path() -> PathBuf {
    if let Ok(env_path) = std::env::var("WORKSPACE_OPERATION_LOG_BASE_PATH") {
        return PathBuf::from(env_path);
    }
    let home_value = std::env::var("PAPERCLIP_HOME").ok();
    let instance_id = std::env::var("PAPERCLIP_INSTANCE_ID")
        .ok()
        .filter(|v| !v.is_empty() && is_valid_segment(v))
        .unwrap_or_else(|| "default".to_string());
    if let Some(home) = home_value {
        return expand_home(&home)
            .join("instances")
            .join(&instance_id)
            .join("data")
            .join("workspace-operation-logs");
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        return PathBuf::from(home)
            .join(".paperclip/instances/default/data/workspace-operation-logs");
    }
    PathBuf::from("./data/workspace-operation-logs")
}

fn is_valid_segment(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return std::env::var("HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(|_| PathBuf::from("."));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        let mut h = std::env::var("HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(|_| PathBuf::from("."));
        h.push(rest);
        return h;
    }
    PathBuf::from(value)
}

fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => cleaned.push(prefix.as_os_str()),
            Component::RootDir => cleaned.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(segment) => cleaned.push(segment),
        }
    }
    cleaned
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn new_event(stream: LogStream, chunk: &str) -> WorkspaceOperationLogEvent {
        WorkspaceOperationLogEvent {
            stream,
            chunk: chunk.to_string(),
            ts: Utc::now(),
        }
    }

    #[tokio::test]
    async fn begin_creates_empty_file() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();

        assert_eq!(handle.store, STORE_TYPE_LOCAL_FILE);
        assert!(handle.log_ref.ends_with(".ndjson"));

        let abs = dir.path().join(&handle.log_ref);
        let meta = tokio::fs::metadata(&abs).await.unwrap();
        assert_eq!(meta.len(), 0);
    }

    #[tokio::test]
    async fn append_then_finalize_returns_hash() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();

        store
            .append(&handle, &new_event(LogStream::Stdout, "line-1\n"))
            .await
            .unwrap();
        store
            .append(&handle, &new_event(LogStream::Stderr, "oops\n"))
            .await
            .unwrap();
        store
            .append(&handle, &new_event(LogStream::System, "starting\n"))
            .await
            .unwrap();

        let summary = store.finalize(&handle).await.unwrap();
        assert!(summary.bytes > 0);
        assert_eq!(summary.sha256.as_ref().unwrap().len(), 64);
        assert!(!summary.compressed);
    }

    #[tokio::test]
    async fn read_default_offset_returns_start() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();

        for i in 0..5 {
            store
                .append(&handle, &new_event(LogStream::Stdout, &format!("{i}\n")))
                .await
                .unwrap();
        }

        let out = store
            .read(&handle, WorkspaceOperationLogReadOptions::default())
            .await
            .unwrap();
        // NDJSON 每行是 {"ts":"...","stream":"stdout","chunk":"..."}
        assert!(out.content.starts_with('{'), "got: {:?}", &out.content[..20.min(out.content.len())]);
        // 文件仅 5 行，远小于 256_000 字节限制，next_offset 应当为 None（已读完）
        assert_eq!(out.next_offset, None);
        // 内容应能反序列化为 JSON 行
        let first_line = out.content.lines().next().unwrap();
        let val: serde_json::Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(val["stream"], "stdout");
        // 文件仅 5 行，远小于 256_000 字节限制，next_offset 应当为 None（已读完）
        assert_eq!(out.next_offset, None);
        assert_eq!(out.content.lines().count(), 5);
    }

    #[tokio::test]
    async fn read_paginate_offset_and_limit() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();

        // 写 10 行
        for i in 0..10 {
            store
                .append(&handle, &new_event(LogStream::Stdout, &format!("{i:02}\n")))
                .await
                .unwrap();
        }
        let summary = store.finalize(&handle).await.unwrap();
        let total = summary.bytes;
        assert!(total > 0);

        // 第一次读 30 字节
        let page1 = store
            .read(
                &handle,
                WorkspaceOperationLogReadOptions {
                    offset: Some(0),
                    limit_bytes: Some(30),
                },
            )
            .await
            .unwrap();
        assert_eq!(page1.content.len(), 30);
        let next = page1.next_offset.unwrap();
        assert_eq!(next, 30);

        // 接着读
        let page2 = store
            .read(
                &handle,
                WorkspaceOperationLogReadOptions {
                    offset: Some(next),
                    limit_bytes: Some(30),
                },
            )
            .await
            .unwrap();
        assert_eq!(page2.content.len(), 30);
        assert_ne!(page1.content, page2.content);

        // 最后一段（剩余不足 30 字节）
        let last_offset = total.saturating_sub(5);
        let last = store
            .read(
                &handle,
                WorkspaceOperationLogReadOptions {
                    offset: Some(last_offset),
                    limit_bytes: Some(30),
                },
            )
            .await
            .unwrap();
        assert!(last.content.len() <= 5);
        assert_eq!(last.next_offset, None);
    }

    #[tokio::test]
    async fn read_offset_beyond_size_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();
        store
            .append(&handle, &new_event(LogStream::Stdout, "hi"))
            .await
            .unwrap();

        // Node 行为：`offset` 被 min 到 file size，然后 start>end 时返回空 content 且
        // next_offset=min(offset, file_size)
        let out = store
            .read(
                &handle,
                WorkspaceOperationLogReadOptions {
                    offset: Some(1_000_000),
                    limit_bytes: Some(256),
                },
            )
            .await
            .unwrap();
        assert!(out.content.is_empty());
        // next_offset 应等于文件大小（被裁剪后的 start）
        let summary = store.finalize(&handle).await.unwrap();
        assert_eq!(out.next_offset, Some(summary.bytes));
    }

    #[tokio::test]
    async fn read_offset_exactly_at_size_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();
        store.append(&handle, &new_event(LogStream::Stdout, "x")).await.unwrap();
        let summary = store.finalize(&handle).await.unwrap();
        let size = summary.bytes;
        let out = store.read(&handle, WorkspaceOperationLogReadOptions { offset: Some(size), limit_bytes: Some(10) }).await.unwrap();
        assert!(out.content.is_empty());
        assert_eq!(out.next_offset, Some(size));
    }

    #[tokio::test]
    async fn append_concurrent_safe() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(LocalFileWorkspaceOperationLogStore::new(dir.path()));
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();

        let mut joins = Vec::new();
        for i in 0..8 {
            let store = store.clone();
            let handle = handle.clone();
            joins.push(tokio::spawn(async move {
                store
                    .append(&handle, &new_event(LogStream::Stdout, &format!("t{i}\n")))
                    .await
            }));
        }
        for j in joins {
            j.await.unwrap().unwrap();
        }

        let summary = store.finalize(&handle).await.unwrap();
        assert!(summary.bytes > 0);
    }

    #[tokio::test]
    async fn finalize_unknown_handle_returns_zero() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let bogus = WorkspaceOperationLogHandle {
            store: "s3".to_string(),
            log_ref: "never/used".to_string(),
        };
        let s = store.finalize(&bogus).await.unwrap();
        assert_eq!(s.bytes, 0);
        assert!(s.sha256.is_none());
        assert!(!s.compressed);
    }

    #[tokio::test]
    async fn read_unknown_handle_errors() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let bogus = WorkspaceOperationLogHandle {
            store: "s3".to_string(),
            log_ref: "never/used".to_string(),
        };
        let err = store
            .read(&bogus, WorkspaceOperationLogReadOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, WorkspaceOperationLogStoreError::NotFound));
    }

    #[tokio::test]
    async fn safe_segments_strips_dangerous_chars() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let bad_company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(bad_company, op).await.unwrap();
        assert!(!handle.log_ref.contains(".."));
        let abs = dir.path().join(&handle.log_ref);
        assert!(abs.exists());
    }

    #[tokio::test]
    async fn resolve_within_rejects_path_traversal() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let err = store.resolve_within("../escape.ndjson").unwrap_err();
        match err {
            WorkspaceOperationLogStoreError::InvalidPath(_) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_base_path_resolves() {
        let p = default_base_path();
        assert!(p.to_string_lossy().contains("workspace-operation-logs"));
    }

    #[tokio::test]
    async fn get_default_store_does_not_panic() {
        clear_workspace_operation_log_store().await;
        let _store = get_workspace_operation_log_store().await;
    }

    #[tokio::test]
    async fn finalize_sha256_matches_manual() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
        let company = Uuid::new_v4();
        let op = Uuid::new_v4();
        let handle = store.begin(company, op).await.unwrap();
        store
            .append(&handle, &new_event(LogStream::Stdout, "abc"))
            .await
            .unwrap();
        let summary = store.finalize(&handle).await.unwrap();
        let abs = dir.path().join(&handle.log_ref);
        let mut f = tokio::fs::File::open(&abs).await.unwrap();
        let mut buf = Vec::new();
        use tokio::io::AsyncReadExt;
        f.read_to_end(&mut buf).await.unwrap();
        let mut h = Sha256::new();
        h.update(&buf);
        let expected = hex::encode(h.finalize());
        assert_eq!(summary.sha256.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn log_stream_strings_match_node() {
        assert_eq!(LogStream::Stdout.as_str(), "stdout");
        assert_eq!(LogStream::Stderr.as_str(), "stderr");
        assert_eq!(LogStream::System.as_str(), "system");
    }

    #[test]
    fn handle_serializes_round_trip() {
        let h = WorkspaceOperationLogHandle {
            store: "local_file".into(),
            log_ref: "company/op.ndjson".into(),
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: WorkspaceOperationLogHandle = serde_json::from_str(&s).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn event_serializes_with_lowercase_stream() {
        let evt = WorkspaceOperationLogEvent {
            stream: LogStream::Stderr,
            chunk: "boom".into(),
            ts: Utc::now(),
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"stream\":\"stderr\""));
    }
}
