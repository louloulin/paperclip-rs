//! Workspace operation log store：local-file NDJSON log，按 company / operation 分文件。
//!
//! 对齐 Node `services/workspace-operation-log-store.ts`：
//! - `safeSegments`: 替换非 `[a-zA-Z0-9._-]` 字符为 `_`
//! - `resolveWithin`: 解析相对路径到 basePath 下，验证不越界
//! - `WorkspaceOperationLogStore` trait: `begin` / `append` / `finalize` / `read`
//! - `LocalFileWorkspaceOperationLogStore`: 实现 NDJSON 追加、范围读取、sha256 摘要
//! - `getWorkspaceOperationLogStore`: 单例，从 `WORKSPACE_OPERATION_LOG_BASE_PATH` 解析

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Log store 类型（与 Node 1:1 对齐）。
pub const STORE_TYPE_LOCAL_FILE: &str = "local_file";

/// 句柄类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "store", rename_all = "snake_case")]
pub enum WorkspaceOperationLogHandle {
    #[serde(rename_all = "camelCase")]
    LocalFile { log_ref: String },
}

impl WorkspaceOperationLogHandle {
    pub fn log_ref(&self) -> &str {
        match self {
            Self::LocalFile { log_ref } => log_ref,
        }
    }
}

/// Begin input。
#[derive(Debug, Clone)]
pub struct BeginInput {
    pub company_id: String,
    pub operation_id: String,
}

/// Append event。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEvent {
    pub stream: LogStream,
    pub chunk: String,
    pub ts: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
    System,
}

/// 读选项。
#[derive(Debug, Clone, Default)]
pub struct ReadOptions {
    pub offset: Option<u64>,
    pub limit_bytes: Option<u64>,
}

/// 读结果。
#[derive(Debug, Clone)]
pub struct ReadResult {
    pub content: String,
    pub next_offset: Option<u64>,
}

/// Finalize summary。
#[derive(Debug, Clone)]
pub struct FinalizeSummary {
    pub bytes: u64,
    pub sha256: Option<String>,
    pub compressed: bool,
}

/// Store trait。
#[async_trait::async_trait]
pub trait WorkspaceOperationLogStore: Send + Sync {
    async fn begin(&self, input: BeginInput) -> Result<WorkspaceOperationLogHandle, LogStoreError>;
    async fn append(
        &self,
        handle: &WorkspaceOperationLogHandle,
        event: &AppendEvent,
    ) -> Result<(), LogStoreError>;
    async fn finalize(
        &self,
        handle: &WorkspaceOperationLogHandle,
    ) -> Result<FinalizeSummary, LogStoreError>;
    async fn read(
        &self,
        handle: &WorkspaceOperationLogHandle,
        opts: ReadOptions,
    ) -> Result<ReadResult, LogStoreError>;
}

#[derive(Debug, Error)]
pub enum LogStoreError {
    #[error("invalid log path: {0}")]
    InvalidPath(String),
    #[error("workspace operation log not found")]
    NotFound,
    #[error("io error while {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 把单段 sanitize 成 `[a-zA-Z0-9._-]`。
pub fn safe_segments(segments: &[&str]) -> Vec<String> {
    segments
        .iter()
        .map(|s| {
            s.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        })
        .collect()
}

/// 解析 `basePath + relativePath` 到绝对路径，验证结果在 basePath 之内。
pub fn resolve_within(base_path: &Path, relative_path: &Path) -> Result<PathBuf, LogStoreError> {
    let resolved = base_path.join(relative_path);
    let canonical_base = base_path.to_path_buf();
    let base_with_sep = {
        let mut s = canonical_base.to_string_lossy().into_owned();
        if !s.ends_with(std::path::MAIN_SEPARATOR) {
            s.push(std::path::MAIN_SEPARATOR);
        }
        s
    };
    let resolved_str = resolved.to_string_lossy();
    if !resolved_str.starts_with(&base_with_sep) && resolved != canonical_base {
        return Err(LogStoreError::InvalidPath(format!(
            "{resolved_str} escapes {base_with_sep}"
        )));
    }
    Ok(resolved)
}

/// Local-file 实现。
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
}

#[async_trait::async_trait]
impl WorkspaceOperationLogStore for LocalFileWorkspaceOperationLogStore {
    async fn begin(
        &self,
        input: BeginInput,
    ) -> Result<WorkspaceOperationLogHandle, LogStoreError> {
        let company = safe_segments(&[input.company_id.as_str()])
            .into_iter()
            .next()
            .unwrap_or_else(|| "_".to_string());
        let operation = safe_segments(&[input.operation_id.as_str()])
            .into_iter()
            .next()
            .unwrap_or_else(|| "_".to_string());
        let rel_dir = PathBuf::from(&company);
        let rel_path = rel_dir.join(format!("{operation}.ndjson"));
        let abs_dir = resolve_within(&self.base_path, &rel_dir)?;
        fs::create_dir_all(&abs_dir).await.map_err(|e| LogStoreError::Io {
            operation: "mkdir log dir",
            source: e,
        })?;
        let abs_path = resolve_within(&self.base_path, &rel_path)?;
        // 截断创建文件
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&abs_path)
            .await
            .map_err(|e| LogStoreError::Io {
                operation: "create log file",
                source: e,
            })?;
        f.sync_all().await.map_err(|e| LogStoreError::Io {
            operation: "sync log file",
            source: e,
        })?;
        Ok(WorkspaceOperationLogHandle::LocalFile {
            log_ref: rel_path.to_string_lossy().into_owned(),
        })
    }

    async fn append(
        &self,
        handle: &WorkspaceOperationLogHandle,
        event: &AppendEvent,
    ) -> Result<(), LogStoreError> {
        let WorkspaceOperationLogHandle::LocalFile { log_ref } = handle;
        let abs_path = resolve_within(&self.base_path, Path::new(log_ref))?;
        let line = serde_json::json!({
            "ts": event.ts,
            "stream": event.stream,
            "chunk": event.chunk,
        });
        let mut s = serde_json::to_string(&line)?;
        s.push('\n');
        let mut f = OpenOptions::new()
            .append(true)
            .open(&abs_path)
            .await
            .map_err(|e| LogStoreError::Io {
                operation: "open log for append",
                source: e,
            })?;
        f.write_all(s.as_bytes())
            .await
            .map_err(|e| LogStoreError::Io {
                operation: "append log",
                source: e,
            })?;
        f.flush().await.map_err(|e| LogStoreError::Io {
            operation: "flush log",
            source: e,
        })?;
        Ok(())
    }

    async fn finalize(
        &self,
        handle: &WorkspaceOperationLogHandle,
    ) -> Result<FinalizeSummary, LogStoreError> {
        let WorkspaceOperationLogHandle::LocalFile { log_ref } = handle;
        let abs_path = resolve_within(&self.base_path, Path::new(log_ref))?;
        let meta = fs::metadata(&abs_path)
            .await
            .map_err(|e| LogStoreError::Io {
                operation: "stat log file",
                source: e,
            })?;
        if !meta.is_file() {
            return Err(LogStoreError::NotFound);
        }
        let hash = sha256_file(&abs_path).await?;
        Ok(FinalizeSummary {
            bytes: meta.len(),
            sha256: Some(hash),
            compressed: false,
        })
    }

    async fn read(
        &self,
        handle: &WorkspaceOperationLogHandle,
        opts: ReadOptions,
    ) -> Result<ReadResult, LogStoreError> {
        let WorkspaceOperationLogHandle::LocalFile { log_ref } = handle;
        let abs_path = resolve_within(&self.base_path, Path::new(log_ref))?;
        let meta = fs::metadata(&abs_path)
            .await
            .map_err(|e| LogStoreError::Io {
                operation: "stat log for read",
                source: e,
            })?;
        if !meta.is_file() {
            return Err(LogStoreError::NotFound);
        }
        let requested_offset = opts.offset.unwrap_or(0);
        let cap = meta.len().saturating_sub(1);
        let start = requested_offset.min(meta.len());
        let limit = opts.limit_bytes.unwrap_or(256_000);
        let end = start.saturating_add(limit).saturating_sub(1).min(cap);
        if start > end {
            // 与 Node 行为一致：next_offset = 原 requested_offset（未 cap）。
            return Ok(ReadResult {
                content: String::new(),
                next_offset: Some(requested_offset),
            });
        }
        let mut f = File::open(&abs_path)
            .await
            .map_err(|e| LogStoreError::Io {
                operation: "open log for read",
                source: e,
            })?;
        use tokio::io::AsyncSeekExt;
        f.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| LogStoreError::Io {
                operation: "seek log",
                source: e,
            })?;
        let mut buf = vec![0u8; (end - start + 1) as usize];
        let mut total = 0;
        while total < buf.len() {
            let n = f
                .read(&mut buf[total..])
                .await
                .map_err(|e| LogStoreError::Io {
                    operation: "read log",
                    source: e,
                })?;
            if n == 0 {
                break;
            }
            total += n;
        }
        buf.truncate(total);
        let content = String::from_utf8_lossy(&buf).into_owned();
        let next_offset = if end + 1 < meta.len() {
            Some(end + 1)
        } else {
            None
        };
        Ok(ReadResult { content, next_offset })
    }
}

async fn sha256_file(path: &Path) -> Result<String, LogStoreError> {
    let mut f = File::open(path)
        .await
        .map_err(|e| LogStoreError::Io {
            operation: "open for sha256",
            source: e,
        })?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await.map_err(|e| LogStoreError::Io {
            operation: "read for sha256",
            source: e,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
/// 默认 base path（与 Node `getWorkspaceOperationLogStore` 1:1 对齐）。
pub fn default_base_path(instance_root: &Path) -> PathBuf {
    instance_root.join("data").join("workspace-operation-logs")
}

