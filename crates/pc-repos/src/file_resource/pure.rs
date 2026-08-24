#![forbid(unsafe_code)]
//! Pure data + limiter for file resources.
//!
//! R792B split: pc-repos::file_resource::pure contains:
//! - FileResourceError -- unified error model
//! - FileResourceLimiter / FileResourceLimiterConfig -- rate + concurrency limiter
//! - ReleaseGuard -- RAII release guard
//! - Query / response structs: FileListQuery / FileEntry / FileListResponse /
//!   FileResolveQuery / ResolvedWorkspaceResource / FileContentResponse
//!
//! 1:1 parity with Node server/src/routes/file-resources.ts query/response schema.


use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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
    pub(crate) active_by_key: Mutex<HashMap<String, usize>>,
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
            windows.retain(|_, w| {
                now.duration_since(w.started_at).as_millis() < self.config.window_ms as u128
            });
        }

        // Check + increment window counter
        {
            let mut windows = self.windows_by_key.lock().expect("windows poisoned");
            let entry = windows.entry(key.to_string()).or_insert(WindowState {
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

impl std::fmt::Debug for ReleaseGuard<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReleaseGuard")
            .field("key", &self.key)
            .finish()
    }
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
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub workspace: String,
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileListResponse {
    pub files: Vec<FileEntry>,
    pub issue_id: Uuid,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileResolveQuery {
    pub path: Option<String>,
    pub workspace: Option<String>,
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
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


