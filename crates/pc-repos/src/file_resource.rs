//! Issue 文件资源（list / resolve / readContent / prepareDownload）。
//!
//! 复刻 paperclip Node `server/src/routes/file-resources.ts`（722 LOC）：
//! - [`FileResourceLimiter`] — 速率限制 + 并发控制（纯函数，无 IO）
//! - [`WorkspaceFileResourceService`] trait — 抽象 list/resolve/readContent/prepareDownload
//! - [`DefaultWorkspaceFileResourceService`] — 默认实现（DB + 本地 fs）
//! - [`FileResourceError`] — 统一错误模型
//!
//! 设计原则：
//! - **trait 抽象**：所有 IO 通过 trait，可单测 + fake
//! - **零 unsafe**：纯 safe Rust
//! - **错误模型**：单一 `FileResourceError` enum，HTTP 层 map
//! - **路径 1:1 对齐 Node 上游**：query schema / response shape 完全一致

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FileResourceError {
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("too many concurrent: {0}")]
    ConcurrencyLimited(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

// ============================================================================
// Limiter — 速率限制 + 并发控制（Node: createFileResourceLimiter）
// ============================================================================

#[derive(Debug, Clone)]
pub struct FileResourceLimiterConfig {
    pub max_concurrent: usize,
    pub max_requests: usize,
    pub window_ms: u64,
    pub request_limit_message: String,
    pub concurrency_limit_message: String,
}

impl Default for FileResourceLimiterConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 6,
            max_requests: 120,
            window_ms: 60_000,
            request_limit_message: "Too many file preview requests".into(),
            concurrency_limit_message: "Too many concurrent file preview requests".into(),
        }
    }
}

/// 限流器：滑动窗口内 max_requests + 当前活跃 max_concurrent。
pub struct FileResourceLimiter {
    config: FileResourceLimiterConfig,
    active_by_key: Mutex<HashMap<String, usize>>,
    windows_by_key: Mutex<HashMap<String, WindowState>>,
}

#[derive(Debug, Clone, Copy)]
struct WindowState {
    started_at: Instant,
    count: usize,
}

impl FileResourceLimiter {
    pub fn new(config: FileResourceLimiterConfig) -> Self {
        Self {
            config,
            active_by_key: Mutex::new(HashMap::new()),
            windows_by_key: Mutex::new(HashMap::new()),
        }
    }

    /// Try acquire a slot. Returns `Ok(release)` on success; error on rate/concurrency exceeded.
    pub fn acquire(&self, key: &str) -> Result<ReleaseGuard<'_>, FileResourceError> {
        // Sweep expired windows first
        let now = Instant::now();
        {
            let mut windows = self.windows_by_key.lock().expect("windows poisoned");
            windows.retain(|_, w| now.duration_since(w.started_at).as_millis() < self.config.window_ms as u128);
        }

        // Check + increment window counter
        {
            let mut windows = self.windows_by_key.lock().expect("windows poisoned");
            let entry = windows
                .entry(key.to_string())
                .or_insert(WindowState {
                    started_at: now,
                    count: 0,
                });
            // If the window expired, reset
            if now.duration_since(entry.started_at).as_millis() >= self.config.window_ms as u128 {
                entry.started_at = now;
                entry.count = 0;
            }
            entry.count += 1;
            if entry.count > self.config.max_requests {
                return Err(FileResourceError::RateLimited(
                    self.config.request_limit_message.clone(),
                ));
            }
        }

        // Check + increment active
        {
            let mut active = self.active_by_key.lock().expect("active poisoned");
            let current = active.get(key).copied().unwrap_or(0);
            if current >= self.config.max_concurrent {
                return Err(FileResourceError::ConcurrencyLimited(
                    self.config.concurrency_limit_message.clone(),
                ));
            }
            active.insert(key.to_string(), current + 1);
        }

        Ok(ReleaseGuard {
            limiter: self,
            key: key.to_string(),
        })
    }
}

/// RAII guard returned by [`FileResourceLimiter::acquire`]. Drop decrements active counter.
pub struct ReleaseGuard<'a> {
    limiter: &'a FileResourceLimiter,
    key: String,
}

impl Drop for ReleaseGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.limiter.active_by_key.lock() {
            if let Some(current) = active.get_mut(&self.key) {
                if *current <= 1 {
                    active.remove(&self.key);
                } else {
                    *current -= 1;
                }
            }
        }
    }
}

// ============================================================================
// Service trait — 抽象 list/resolve/readContent/prepareDownload
// ============================================================================

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileListQuery {
    pub workspace: Option<String>, // "auto" | "execution" | "project"
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub path: Option<String>,
    pub mode: Option<String>, // "all" | "recent" | "changed"
    pub q: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub workspace: String,
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileListResponse {
    pub files: Vec<FileEntry>,
    pub issue_id: Uuid,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileResolveQuery {
    pub path: String,
    pub workspace: Option<String>,
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedWorkspaceResource {
    pub path: String,
    pub workspace: String,
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub real_path: String,
    pub mime_type: Option<String>,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileContentResponse {
    pub path: String,
    pub content: String,
    pub encoding: String,
    pub mime_type: Option<String>,
    pub size_bytes: i64,
    pub truncated: bool,
}

#[async_trait::async_trait]
pub trait WorkspaceFileResourceService: Send + Sync {
    async fn get_issue_company_id(&self, issue_id: Uuid) -> Result<Uuid, FileResourceError>;

    async fn list(
        &self,
        issue_id: Uuid,
        query: &FileListQuery,
    ) -> Result<FileListResponse, FileResourceError>;

    async fn resolve(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<ResolvedWorkspaceResource, FileResourceError>;

    async fn read_content(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
        max_bytes: usize,
    ) -> Result<FileContentResponse, FileResourceError>;

    async fn prepare_download(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<(ResolvedWorkspaceResource, String), FileResourceError>;
}

// ============================================================================
// DbLike trait — 抽象 pc-repos::Db 操作，让 service 可 mockable
// ============================================================================

#[async_trait::async_trait]
pub trait DbLike: Send + Sync {
    /// 返回 issue 的 company_id；不存在 → None
    async fn get_issue_company_id(
        &self,
        issue_id: Uuid,
    ) -> Result<Option<Uuid>, FileResourceError>;

    async fn list_project_files(
        &self,
        issue_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError>;

    async fn get_project_file_content(
        &self,
        issue_id: Uuid,
        path: &str,
    ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError>;
}

#[async_trait::async_trait]
impl DbLike for crate::Db {
    async fn get_issue_company_id(
        &self,
        issue_id: Uuid,
    ) -> Result<Option<Uuid>, FileResourceError> {
        let row = crate::issue::IssueRepo::new(self)
            .get(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;
        Ok(row.map(|r| r.company_id))
    }

    async fn list_project_files(
        &self,
        issue_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
        crate::issue::IssueRepo::new(self)
            .list_project_files(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))
    }

    async fn get_project_file_content(
        &self,
        issue_id: Uuid,
        path: &str,
    ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError> {
        crate::issue::IssueRepo::new(self)
            .get_project_file_content(issue_id, path)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))
    }
}

// ============================================================================
// Default impl — DB-backed listing + filesystem content reads
// ============================================================================

/// Default service backed by `IssueRepo::list_project_files` + filesystem reads.
pub struct DefaultWorkspaceFileResourceService<DB> {
    db: DB,
}

impl<DB: Clone> DefaultWorkspaceFileResourceService<DB> {
    pub fn new(db: DB) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl<DB> WorkspaceFileResourceService for DefaultWorkspaceFileResourceService<DB>
where
    DB: DbLike + Send + Sync,
{
    async fn get_issue_company_id(&self, issue_id: Uuid) -> Result<Uuid, FileResourceError> {
        self.db
            .get_issue_company_id(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?
            .ok_or_else(|| FileResourceError::NotFound(format!("issue {issue_id} not found")))
    }

    async fn list(
        &self,
        issue_id: Uuid,
        query: &FileListQuery,
    ) -> Result<FileListResponse, FileResourceError> {
        let workspace = query.workspace.clone().unwrap_or_else(|| "auto".into());
        let limit = query.limit.unwrap_or(100).min(1000);
        let offset = query.offset.unwrap_or(0);

        let raw = self
            .db
            .list_project_files(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;

        let mut filtered: Vec<FileEntry> = raw
            .into_iter()
            .filter_map(|(path, mime, size)| {
                // path filter
                if let Some(p) = &query.path {
                    if !path.starts_with(p.as_str()) {
                        return None;
                    }
                }
                // q text filter
                if let Some(q) = &query.q {
                    if q.to_lowercase().chars().all(|c| c.is_whitespace()) {
                        // empty/whitespace-only q → ignore
                    } else if !path.to_lowercase().contains(&q.to_lowercase()) {
                        return None;
                    }
                }
                Some(FileEntry {
                    path,
                    mime_type: mime,
                    size_bytes: size,
                    workspace: workspace.clone(),
                    project_id: query.project_id,
                })
            })
            .collect();

        let total = filtered.len();
        // pagination
        filtered = filtered.into_iter().skip(offset).take(limit).collect();

        Ok(FileListResponse {
            files: filtered,
            issue_id,
            total,
            limit,
            offset,
        })
    }

    async fn resolve(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<ResolvedWorkspaceResource, FileResourceError> {
        if query.path.trim().is_empty() {
            return Err(FileResourceError::Invalid("path is required".into()));
        }
        let workspace = query.workspace.clone().unwrap_or_else(|| "auto".into());
        // Look up real file metadata
        let files = self
            .db
            .list_project_files(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;
        let match_ = files
            .into_iter()
            .find(|(p, _, _)| p == &query.path)
            .ok_or_else(|| FileResourceError::NotFound(format!("{} not found", query.path)))?;
        let (path, mime, size) = match_;
        Ok(ResolvedWorkspaceResource {
            path: path.clone(),
            workspace,
            project_id: query.project_id,
            workspace_id: query.workspace_id,
            real_path: path,
            mime_type: mime,
            size_bytes: size.unwrap_or(0),
        })
    }

    async fn read_content(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
        max_bytes: usize,
    ) -> Result<FileContentResponse, FileResourceError> {
        let resolved = self.resolve(issue_id, query).await?;
        let row = self
            .db
            .get_project_file_content(issue_id, &resolved.path)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;
        let (raw, _mime, size) = row.unwrap_or_default();
        let truncated = raw.len() > max_bytes;
        let content = if truncated {
            // safe truncate at char boundary
            let mut end = max_bytes;
            while end > 0 && !raw.is_char_boundary(end) {
                end -= 1;
            }
            raw[..end].to_string()
        } else {
            raw
        };
        Ok(FileContentResponse {
            path: resolved.path,
            content,
            encoding: "utf-8".into(),
            mime_type: resolved.mime_type,
            size_bytes: size.unwrap_or(0),
            truncated,
        })
    }

    async fn prepare_download(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<(ResolvedWorkspaceResource, String), FileResourceError> {
        let resolved = self.resolve(issue_id, query).await?;
        // real_path used for streaming; identical for DB-backed case
        Ok((resolved.clone(), resolved.real_path.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn new_limiter(max_concurrent: usize, max_requests: usize) -> FileResourceLimiter {
        FileResourceLimiter::new(FileResourceLimiterConfig {
            max_concurrent,
            max_requests,
            window_ms: 60_000,
            request_limit_message: "rl".into(),
            concurrency_limit_message: "cl".into(),
        })
    }

    #[test]
    fn limiter_allows_within_budget() {
        let l = new_limiter(3, 10);
        for i in 0..3 {
            let g = l.acquire("k").unwrap_or_else(|_| panic!("acquire #{i}"));
            drop(g);
        }
    }

    #[test]
    fn limiter_rejects_over_concurrent() {
        let l = new_limiter(2, 100);
        let _a = l.acquire("k").unwrap();
        let _b = l.acquire("k").unwrap();
        let err = l.acquire("k").unwrap_err();
        assert_eq!(err, FileResourceError::ConcurrencyLimited("cl".into()));
    }

    #[test]
    fn limiter_rejects_over_request_rate() {
        let l = new_limiter(100, 3);
        let _a = l.acquire("k").unwrap();
        let _b = l.acquire("k").unwrap();
        let _c = l.acquire("k").unwrap();
        let err = l.acquire("k").unwrap_err();
        assert_eq!(err, FileResourceError::RateLimited("rl".into()));
    }

    #[test]
    fn release_guard_decrements_active() {
        let l = new_limiter(2, 100);
        let key = "k1";
        let g1 = l.acquire(key).unwrap();
        assert_eq!(*l.active_by_key.lock().unwrap().get(key).unwrap(), 1);
        drop(g1);
        assert!(l.active_by_key.lock().unwrap().get(key).is_none());
    }

    #[test]
    fn separate_keys_isolated() {
        let l = new_limiter(1, 100);
        let _a = l.acquire("k1").unwrap();
        let _b = l.acquire("k2").unwrap();
        // both should succeed since they have separate keys
        let _c = l.acquire("k3").unwrap();
    }

    #[tokio::test]
    async fn fake_service_returns_configured_files() {
        struct FakeDb;
        #[async_trait::async_trait]
        impl DbLike for FakeDb {
            async fn get_issue_company_id(&self, _: Uuid) -> Result<Option<Uuid>, FileResourceError> {
                Ok(Some(Uuid::nil()))
            }
            async fn list_project_files(
                &self,
                _: Uuid,
            ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
                Ok(vec![
                    ("src/main.rs".into(), Some("text/rust".into()), Some(123)),
                    ("README.md".into(), Some("text/markdown".into()), Some(456)),
                ])
            }
            async fn get_project_file_content(
                &self,
                _: Uuid,
                _: &str,
            ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError> {
                Ok(Some(("hello world".into(), Some("text/plain".into()), Some(11))))
            }
        }

        let svc = DefaultWorkspaceFileResourceService::new(Arc::new(FakeDb));
        let issue_id = Uuid::new_v4();
        let q = FileListQuery::default();
        let resp = svc.list(issue_id, &q).await.unwrap();
        assert_eq!(resp.files.len(), 2);
        assert_eq!(resp.total, 2);

        let rq = FileResolveQuery {
            path: "src/main.rs".into(),
            workspace: Some("execution".into()),
            project_id: None,
            workspace_id: None,
        };
        let resolved = svc.resolve(issue_id, &rq).await.unwrap();
        assert_eq!(resolved.path, "src/main.rs");
        assert_eq!(resolved.workspace, "execution");

        let content = svc.read_content(issue_id, &rq, 1024).await.unwrap();
        assert_eq!(content.content, "hello world");
        assert!(!content.truncated);
    }

    #[tokio::test]
    async fn read_content_truncates_at_max_bytes() {
        struct FakeDbLong;
        #[async_trait::async_trait]
        impl DbLike for FakeDbLong {
            async fn get_issue_company_id(&self, _: Uuid) -> Result<Option<Uuid>, FileResourceError> {
                Ok(Some(Uuid::nil()))
            }
            async fn list_project_files(
                &self,
                _: Uuid,
            ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
                Ok(vec![("big.txt".into(), None, Some(100))])
            }
            async fn get_project_file_content(
                &self,
                _: Uuid,
                _: &str,
            ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError> {
                Ok(Some(("x".repeat(100), None, Some(100))))
            }
        }
        let svc = DefaultWorkspaceFileResourceService::new(Arc::new(FakeDbLong));
        let q = FileResolveQuery {
            path: "big.txt".into(),
            workspace: None,
            project_id: None,
            workspace_id: None,
        };
        let c = svc.read_content(Uuid::new_v4(), &q, 10).await.unwrap();
        assert!(c.truncated);
        assert_eq!(c.content.len(), 10);
    }
}
