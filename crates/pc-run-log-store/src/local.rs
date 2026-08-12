//! LocalFileRunLogStore: durable per-run ndjson log stored on the local
//! filesystem with an optional S3-compatible mirror and throttled
//! in-flight tail upload.
//!
//! 1:1 alignment with the Node createDurableRunLogStore
//! (server/src/services/run-log-store.ts:140-340).
//!
//! Behavior summary:
//! - begin({companyId, agentId, runId}) returns a handle whose logRef is
//!   safe_segments(...).join("/") + ".ndjson". The base directory and
//!   the file are created on first append.
//! - append(handle, event) serializes one ndjson line
//!   {ts, stream, chunk, seq?} and appends it. Returns the new byte
//!   length.
//! - finalize(handle) retires the in-flight mirror bookkeeping (waits
//!   for any upload still on the wire so a stale partial can never
//!   overwrite the complete file), then mirrors the complete file when a
//!   MirrorTarget is configured. Returns {bytes, sha256, compressed}.
//! - read(handle, opts) returns the local file content within
//!   [offset, offset+limitBytes). When the local file is missing, an
//!   empty result is returned; a future extension can add a get_object
//!   path to the MirrorTarget trait for cold reads from the mirror.
//! - flush_inflight_mirrors() is a graceful-shutdown hook: it waits for
//!   every in-flight mirror upload to finish, then uploads any still
//!   dirty entries. No-op when the mirror is not configured or
//!   in-flight mirroring is off.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::sleep;
use tracing::warn;

use crate::factory::{safe_segments, DurableRunLogStoreOptions, MirrorTargetSpec};
use crate::types::{
    resolve_within, BeginInput, DynRunLogStore, MirrorError, RunLogError, RunLogEvent,
    RunLogFinalizeSummary, RunLogHandle, RunLogReadOptions, RunLogReadResult, RunLogStore,
    RunLogStoreType,
};

#[derive(Debug)]
struct InflightSlot {
    last_mirror_at: Instant,
    inflight: bool,
    dirty: bool,
}

#[derive(Debug)]
struct StoreState {
    base_path: PathBuf,
    s3: Option<Arc<MirrorTargetSpec>>,
    inflight_mirror: Duration,
    inflight: Mutex<HashMap<String, InflightSlot>>,
    flush_lock: AsyncMutex<()>,
}

impl StoreState {
    fn s3_key(&self, log_ref: &str) -> String {
        match &self.s3 {
            None => log_ref.to_string(),
            Some(spec) if spec.key_prefix.is_empty() => log_ref.to_string(),
            Some(spec) => format!("{}/{}", spec.key_prefix, log_ref),
        }
    }

    fn abs_path(&self, log_ref: &str) -> Result<PathBuf, RunLogError> {
        resolve_within(&self.base_path, log_ref)
    }

    async fn ensure_dir(dir: &Path) -> Result<(), RunLogError> {
        fs::create_dir_all(dir).await.map_err(Into::into)
    }
}

/// Local-file run-log store. Cloneable so the in-flight tail scheduler can
/// hold its own Arc; the public API goes through the trait object.
#[derive(Debug, Clone)]
pub struct LocalFileRunLogStore {
    state: Arc<StoreState>,
}

impl LocalFileRunLogStore {
    pub fn new(opts: DurableRunLogStoreOptions) -> Self {
        let inflight_mirror = opts
            .s3
            .as_ref()
            .and_then(|s| s.inflight_mirror_ms)
            .filter(|d| !d.is_zero())
            .unwrap_or(Duration::ZERO);
        let state = StoreState {
            base_path: opts.base_path,
            s3: opts.s3.map(Arc::new),
            inflight_mirror,
            inflight: Mutex::new(HashMap::new()),
            flush_lock: AsyncMutex::new(()),
        };
        Self { state: Arc::new(state) }
    }

    pub fn base_path(&self) -> &Path {
        &self.state.base_path
    }

    fn build_log_ref(company_id: &str, agent_id: &str, run_id: &str) -> String {
        let safe = safe_segments(&[company_id, agent_id, run_id]);
        format!("{}/{}/{}.ndjson", safe[0], safe[1], safe[2])
    }

    fn serialize_event(event: &RunLogEvent) -> Result<String, RunLogError> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "ts".into(),
            serde_json::Value::String(event.ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
        obj.insert(
            "stream".into(),
            serde_json::Value::String(event.stream.as_str().to_string()),
        );
        obj.insert("chunk".into(), serde_json::Value::String(event.chunk.clone()));
        if let Some(seq) = event.seq {
            obj.insert("seq".into(), serde_json::Value::Number(seq.into()));
        }
        serde_json::to_string(&obj)
            .map(|s| format!("{s}\n"))
            .map_err(|e| RunLogError::Io(format!("serialize event: {e}")))
    }

    /// Mark the in-flight slot dirty and spawn a throttled uploader if no
    /// upload is already in flight for this log_ref.
    fn note_inflight(&self, log_ref: String) {
        if self.state.s3.is_none() || self.state.inflight_mirror.is_zero() {
            return;
        }
        let should_spawn = {
            let mut guard = self.state.inflight.lock();
            let entry = guard.entry(log_ref.clone()).or_insert(InflightSlot {
                last_mirror_at: Instant::now(),
                inflight: false,
                dirty: false,
            });
            entry.dirty = true;
            if entry.inflight {
                false
            } else {
                entry.inflight = true;
                true
            }
        };
        if should_spawn {
            let state = self.state.clone();
            tokio::spawn(async move {
                run_inflight_loop(state, log_ref).await;
            });
        }
    }
}

#[async_trait]
impl RunLogStore for LocalFileRunLogStore {
    async fn begin(&self, input: BeginInput) -> Result<RunLogHandle, RunLogError> {
        let log_ref = Self::build_log_ref(&input.company_id, &input.agent_id, &input.run_id);
        let abs = self.state.abs_path(&log_ref)?;
        if let Some(parent) = abs.parent() {
            StoreState::ensure_dir(parent).await?;
        }
        Ok(RunLogHandle::new_local_file(log_ref))
    }

    async fn append(
        &self,
        handle: &RunLogHandle,
        event: RunLogEvent,
    ) -> Result<u64, RunLogError> {
        if handle.store != RunLogStoreType::LocalFile {
            return Err(RunLogError::StoreIdMismatch(handle.store.as_str().to_string()));
        }
        let path = self.state.abs_path(&handle.log_ref)?;
        if let Some(parent) = path.parent() {
            StoreState::ensure_dir(parent).await?;
        }
        let line = Self::serialize_event(&event)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        let len = file.metadata().await?.len();
        drop(file);

        self.note_inflight(handle.log_ref.clone());

        Ok(len)
    }

    async fn finalize(
        &self,
        handle: &RunLogHandle,
    ) -> Result<RunLogFinalizeSummary, RunLogError> {
        if handle.store != RunLogStoreType::LocalFile {
            return Err(RunLogError::StoreIdMismatch(handle.store.as_str().to_string()));
        }
        retire_inflight_mirror(&self.state, &handle.log_ref).await;

        let path = self.state.abs_path(&handle.log_ref)?;
        let bytes_total = match fs::metadata(&path).await {
            Ok(m) => m.len(),
            Err(_) => 0,
        };

        let sha256 = if bytes_total > 0 {
            let mut file = File::open(&path).await?;
            let mut hasher = Sha256::new();
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Some(hex::encode(hasher.finalize()))
        } else {
            None
        };

        if let Some(spec) = &self.state.s3 {
            if bytes_total > 0 {
                let body = fs::read(&path).await?;
                let key = self.state.s3_key(&handle.log_ref);
                if let Err(e) = spec
                    .provider
                    .put_object(
                        &key,
                        Bytes::from(body),
                        Some("application/x-ndjson"),
                        bytes_total,
                    )
                    .await
                {
                    warn!(
                        target: "pc_run_log_store",
                        "failed to upload finalized run log to {}: {}",
                        key, e
                    );
                }
            }
        }

        Ok(RunLogFinalizeSummary {
            bytes: bytes_total,
            sha256,
            compressed: false,
        })
    }

    async fn read(
        &self,
        handle: &RunLogHandle,
        opts: RunLogReadOptions,
    ) -> Result<RunLogReadResult, RunLogError> {
        if handle.store != RunLogStoreType::LocalFile {
            return Err(RunLogError::StoreIdMismatch(handle.store.as_str().to_string()));
        }
        let path = self.state.abs_path(&handle.log_ref)?;
        let stat = match fs::metadata(&path).await {
            Ok(s) => s,
            Err(_) => {
                return Ok(RunLogReadResult {
                    content: String::new(),
                    next_offset: Some(0),
                });
            }
        };
        let size = stat.len();
        let start = opts.offset.unwrap_or(0).min(size);
        let max = opts
            .limit_bytes
            .unwrap_or(64 * 1024)
            .min(size.saturating_sub(start));
        if max == 0 {
            return Ok(RunLogReadResult {
                content: String::new(),
                next_offset: Some(start),
            });
        }
        let mut file = File::open(&path).await?;
        file.seek(SeekFrom::Start(start)).await?;
        let mut buf = vec![0u8; max as usize];
        let n = file.read_exact(&mut buf).await?;
        buf.truncate(n);
        let content = String::from_utf8_lossy(&buf).to_string();
        let next_offset = Some(start + n as u64);
        Ok(RunLogReadResult {
            content,
            next_offset,
        })
    }

    async fn flush_inflight_mirrors(&self) -> Result<(), RunLogError> {
        let _g = self.state.flush_lock.lock().await;
        let log_refs: Vec<String> = {
            let guard = self.state.inflight.lock();
            guard.keys().cloned().collect()
        };
        for log_ref in log_refs {
            loop {
                let should_upload = {
                    let mut guard = self.state.inflight.lock();
                    match guard.get_mut(&log_ref) {
                        Some(entry) => {
                            if entry.inflight {
                                true
                            } else if entry.dirty {
                                entry.inflight = true;
                                entry.dirty = false;
                                true
                            } else {
                                false
                            }
                        }
                        None => false,
                    }
                };
                if !should_upload {
                    break;
                }
                mirror_inflight_now(&self.state, &log_ref).await;
                if !self.state.inflight_mirror.is_zero() {
                    sleep(self.state.inflight_mirror).await;
                }
            }
        }
        Ok(())
    }
}

async fn retire_inflight_mirror(state: &StoreState, log_ref: &str) {
    loop {
        let inflight = {
            let mut guard = state.inflight.lock();
            match guard.get_mut(log_ref) {
                Some(entry) => {
                    if entry.inflight {
                        true
                    } else {
                        guard.remove(log_ref);
                        false
                    }
                }
                None => false,
            }
        };
        if !inflight {
            break;
        }
        sleep(Duration::from_millis(5)).await;
    }
}

async fn run_inflight_loop(state: Arc<StoreState>, log_ref: String) {
    loop {
        sleep(state.inflight_mirror).await;
        let keep_going = mirror_inflight_once(&state, &log_ref).await;
        if !keep_going {
            break;
        }
    }
}

async fn mirror_inflight_once(state: &StoreState, log_ref: &str) -> bool {
    // Clear dirty under the lock; if nothing to do, release the inflight
    // slot and exit.
    let should_upload = {
        let mut guard = state.inflight.lock();
        match guard.get_mut(log_ref) {
            Some(entry) => {
                if entry.dirty {
                    entry.dirty = false;
                    true
                } else {
                    entry.inflight = false;
                    false
                }
            }
            None => false,
        }
    };
    if !should_upload {
        return false;
    }
    let success = mirror_inflight_now(state, log_ref).await;
    if !success {
        // Failed upload: re-dirty and keep the loop alive.
        let mut guard = state.inflight.lock();
        if let Some(entry) = guard.get_mut(log_ref) {
            entry.dirty = true;
        }
        return true;
    }
    // After a successful upload, see if more has been appended since.
    let still_dirty = {
        let mut guard = state.inflight.lock();
        match guard.get_mut(log_ref) {
            Some(entry) => {
                if entry.dirty {
                    // Drain the dirty flag; the next loop iteration will
                    // upload.
                    entry.dirty = false;
                    true
                } else {
                    entry.inflight = false;
                    false
                }
            }
            None => false,
        }
    };
    still_dirty
}

async fn mirror_inflight_now(state: &StoreState, log_ref: &str) -> bool {
    let path = match state.abs_path(log_ref) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                target: "pc_run_log_store",
                "inflight mirror: invalid path for {}: {}",
                log_ref, e
            );
            return false;
        }
    };
    let stat = match fs::metadata(&path).await {
        Ok(s) => s,
        Err(_) => return true,
    };
    if stat.len() == 0 {
        return true;
    }
    let body = match fs::read(&path).await {
        Ok(b) => b,
        Err(e) => {
            warn!(
                target: "pc_run_log_store",
                "inflight mirror: read failed for {}: {}",
                log_ref, e
            );
            return false;
        }
    };
    let key = state.s3_key(log_ref);
    let result = if let Some(spec) = &state.s3 {
        spec.provider
            .put_object(
                &key,
                Bytes::from(body),
                Some("application/x-ndjson"),
                stat.len(),
            )
            .await
    } else {
        Err(MirrorError::NotConfigured)
    };
    match result {
        Ok(_) => true,
        Err(e) => {
            warn!(
                target: "pc_run_log_store",
                "inflight mirror: upload failed for {}: {}",
                key, e
            );
            false
        }
    }
}

impl From<LocalFileRunLogStore> for DynRunLogStore {
    fn from(value: LocalFileRunLogStore) -> Self {
        Arc::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn begin_returns_local_file_handle() {
        let dir = TempDir::new().unwrap();
        let store = LocalFileRunLogStore::new(DurableRunLogStoreOptions {
            base_path: dir.path().to_path_buf(),
            s3: None,
        });
        let handle = store
            .begin(BeginInput {
                company_id: "co-1".into(),
                agent_id: "ag-1".into(),
                run_id: "run-1".into(),
            })
            .await
            .unwrap();
        assert_eq!(handle.store, RunLogStoreType::LocalFile);
        assert_eq!(handle.log_ref, "co-1/ag-1/run-1.ndjson");
    }

    #[tokio::test]
    async fn safe_segments_replaces_unsafe_chars() {
        let s = safe_segments(&["a b", "c/d", "e.f"]);
        assert_eq!(s, vec!["a_b", "c_d", "e.f"]);
    }

    #[tokio::test]
    async fn resolve_within_rejects_path_traversal() {
        let dir = TempDir::new().unwrap();
        let bad = dir.path().join("../etc/passwd");
        let result = resolve_within(dir.path(), bad.to_str().unwrap());
        assert!(matches!(result, Err(RunLogError::InvalidPath(_))));
    }
}
