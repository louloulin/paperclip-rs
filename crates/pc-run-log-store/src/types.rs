//! Public types for the run-log store.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable identity for the local-file store.
///
/// The Node implementation never returns a different store id; this
/// constant exists so downstream consumers (heartbeat reads, feedback tail,
/// fixtures) can switch on it without parsing opaque strings.
pub const RUN_LOG_STORE_LOCAL_FILE: RunLogStoreType = RunLogStoreType::LocalFile;

/// Logical store kind. Mirrors the Node `RunLogStoreType` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLogStoreType {
    LocalFile,
}

impl RunLogStoreType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
        }
    }
}

/// Handle identifying a run log. The `log_ref` is the relative path within
/// the base dir (segments have been sanitized via `safe_segments`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLogHandle {
    pub store: RunLogStoreType,
    pub log_ref: String,
}

impl RunLogHandle {
    pub fn new_local_file(log_ref: impl Into<String>) -> Self {
        Self {
            store: RunLogStoreType::LocalFile,
            log_ref: log_ref.into(),
        }
    }
}

/// Begin call inputs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BeginInput {
    pub company_id: String,
    pub agent_id: String,
    pub run_id: String,
}

/// Event kinds written to the ndjson stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLogStream {
    Stdout,
    Stderr,
    System,
}

impl RunLogStream {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::System => "system",
        }
    }
}

/// Event to append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLogEvent {
    pub stream: RunLogStream,
    pub chunk: String,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub seq: Option<u64>,
}

/// Read options.
#[derive(Debug, Clone, Default)]
pub struct RunLogReadOptions {
    pub offset: Option<u64>,
    pub limit_bytes: Option<u64>,
}

/// Read result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLogReadResult {
    pub content: String,
    pub next_offset: Option<u64>,
}

/// Finalize summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLogFinalizeSummary {
    pub bytes: u64,
    pub sha256: Option<String>,
    pub compressed: bool,
}

/// Mirror target trait. Adapt `pc_storage::StorageProvider` to this by
/// converting `&StorageLocation`/`Bytes` pairs into the
/// `put_object` call. The mirror is best-effort: a failing upload must
/// never break the run.
#[async_trait]
pub trait MirrorTarget: Send + Sync + std::fmt::Debug {
    async fn put_object(
        &self,
        object_key: &str,
        body: bytes::Bytes,
        content_type: Option<&str>,
        content_length: u64,
    ) -> Result<(), MirrorError>;
}

/// Mirror errors are non-fatal: the run log store logs and continues.
#[derive(Debug, Error)]
pub enum MirrorError {
    #[error("mirror transport error: {0}")]
    Transport(String),
    #[error("mirror response error: {0}")]
    Response(String),
    #[error("mirror not configured")]
    NotConfigured,
}

impl From<MirrorError> for RunLogError {
    fn from(value: MirrorError) -> Self {
        match value {
            MirrorError::NotConfigured => RunLogError::MirrorNotConfigured,
            other => RunLogError::Mirror(other.to_string()),
        }
    }
}

/// Run log store trait.
///
/// `begin` is idempotent and returns a stable handle (segments are
/// sanitized). `append` writes one ndjson line and returns the new byte
/// length. `finalize` retires in-flight mirrors and uploads the complete
/// file when a mirror is configured, then returns the byte count and
/// sha256. `read` returns the local file content if present and falls
/// back to the mirror otherwise. `flush_inflight_mirrors` is a no-op
/// when no mirror or in-flight mirroring is disabled.
#[async_trait]
pub trait RunLogStore: Send + Sync + std::fmt::Debug {
    async fn begin(&self, input: BeginInput) -> Result<RunLogHandle, RunLogError>;
    async fn append(
        &self,
        handle: &RunLogHandle,
        event: RunLogEvent,
    ) -> Result<u64, RunLogError>;
    async fn finalize(
        &self,
        handle: &RunLogHandle,
    ) -> Result<RunLogFinalizeSummary, RunLogError>;
    async fn read(
        &self,
        handle: &RunLogHandle,
        opts: RunLogReadOptions,
    ) -> Result<RunLogReadResult, RunLogError>;
    async fn flush_inflight_mirrors(&self) -> Result<(), RunLogError>;
}

/// Run log error.
#[derive(Debug, Error)]
pub enum RunLogError {
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid log path: {0}")]
    InvalidPath(String),
    #[error("mirror not configured")]
    MirrorNotConfigured,
    #[error("mirror error: {0}")]
    Mirror(String),
    #[error("store id mismatch: expected local_file, got {0}")]
    StoreIdMismatch(String),
}

impl From<std::io::Error> for RunLogError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

/// Public path resolution helper. Mirrors the Node `resolveWithin` check.
pub fn resolve_within(base_path: &std::path::Path, relative: &str) -> Result<PathBuf, RunLogError> {
    if relative.is_empty() {
        return Err(RunLogError::InvalidPath("empty relative path".into()));
    }
    let candidate = base_path.join(relative);
    // Reject any ParentDir component lexically (mirrors path.resolve
    // + base check used by Node, which collapses ".." before comparing).
    for component in candidate.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(RunLogError::InvalidPath(format!(
                "path contains ..: {}",
                candidate.display()
            )));
        }
    }
    let base_canon = std::path::absolute(base_path)
        .map_err(|e| RunLogError::InvalidPath(format!("base path invalid: {e}")))?;
    let candidate_canon = std::path::absolute(&candidate)
        .map_err(|e| RunLogError::InvalidPath(format!("path invalid: {e}")))?;
    let mut base_with_sep = base_canon.as_os_str().to_string_lossy().into_owned();
    if !base_with_sep.ends_with(std::path::MAIN_SEPARATOR) {
        base_with_sep.push(std::path::MAIN_SEPARATOR);
    }
    let candidate_str = candidate_canon.as_os_str().to_string_lossy();
    if !candidate_str.starts_with(&base_with_sep) && candidate_canon != base_canon {
        return Err(RunLogError::InvalidPath(format!(
            "path escapes base: {} not under {}",
            candidate_canon.display(),
            base_canon.display()
        )));
    }
    Ok(candidate_canon)
}

/// Type alias for an owned `dyn RunLogStore`.
pub type DynRunLogStore = Arc<dyn RunLogStore>;
