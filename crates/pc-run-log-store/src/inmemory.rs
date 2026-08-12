//! In-memory run-log store for tests and ephemeral use cases.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::types::{
    BeginInput, DynRunLogStore, MirrorTarget, RunLogError, RunLogEvent,
    RunLogFinalizeSummary, RunLogHandle, RunLogReadOptions, RunLogReadResult, RunLogStore,
    RunLogStoreType,
};

/// In-memory run-log store. Mirrors the on-disk `LocalFileRunLogStore`
/// semantics (ndjson events, stable `local_file` identity, mirror
/// through `MirrorTarget` when configured) but stores lines in a
/// `HashMap<logRef, Vec<u8>>` so it is cheap to spin up in tests.
#[derive(Debug, Default)]
pub struct InMemoryRunLogStore {
    inner: Mutex<HashMap<String, Vec<u8>>>,
    s3: Option<Arc<dyn MirrorTarget>>,
    s3_prefix: String,
}

impl InMemoryRunLogStore {
    pub fn new(s3: Option<Arc<dyn MirrorTarget>>, s3_prefix: impl Into<String>) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            s3,
            s3_prefix: s3_prefix.into(),
        }
    }

    pub fn bytes(&self, log_ref: &str) -> Option<Vec<u8>> {
        self.inner.lock().get(log_ref).cloned()
    }
}

#[async_trait]
impl RunLogStore for InMemoryRunLogStore {
    async fn begin(&self, input: BeginInput) -> Result<RunLogHandle, RunLogError> {
        let log_ref = format!(
            "{}/{}/{}.ndjson",
            sanitize(&input.company_id),
            sanitize(&input.agent_id),
            sanitize(&input.run_id)
        );
        self.inner.lock().entry(log_ref.clone()).or_default();
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
        let line = serialize(&event)?;
        let mut guard = self.inner.lock();
        let buf = guard.entry(handle.log_ref.clone()).or_default();
        buf.extend_from_slice(line.as_bytes());
        Ok(buf.len() as u64)
    }

    async fn finalize(
        &self,
        handle: &RunLogHandle,
    ) -> Result<RunLogFinalizeSummary, RunLogError> {
        if handle.store != RunLogStoreType::LocalFile {
            return Err(RunLogError::StoreIdMismatch(handle.store.as_str().to_string()));
        }
        let bytes_total = {
            let guard = self.inner.lock();
            guard.get(&handle.log_ref).map(|b| b.len() as u64).unwrap_or(0)
        };
        let sha256 = if bytes_total > 0 {
            let guard = self.inner.lock();
            let buf = guard.get(&handle.log_ref).cloned().unwrap_or_default();
            drop(guard);
            let mut hasher = Sha256::new();
            hasher.update(&buf);
            Some(hex::encode(hasher.finalize()))
        } else {
            None
        };
        if let Some(s3) = &self.s3 {
            if bytes_total > 0 {
                let body = {
                    let guard = self.inner.lock();
                    guard.get(&handle.log_ref).cloned().unwrap_or_default()
                };
                let key = if self.s3_prefix.is_empty() {
                    handle.log_ref.clone()
                } else {
                    format!("{}/{}", self.s3_prefix, handle.log_ref)
                };
                s3.put_object(&key, Bytes::from(body), Some("application/x-ndjson"), bytes_total)
                    .await
                    .ok();
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
        let guard = self.inner.lock();
        let buf = match guard.get(&handle.log_ref) {
            Some(b) => b.clone(),
            None => Vec::new(),
        };
        drop(guard);
        let start = opts.offset.unwrap_or(0).min(buf.len() as u64) as usize;
        let max = opts
            .limit_bytes
            .unwrap_or(64 * 1024)
            .min((buf.len() as u64).saturating_sub(start as u64)) as usize;
        if max == 0 {
            return Ok(RunLogReadResult {
                content: String::new(),
                next_offset: Some(start as u64),
            });
        }
        let end = start + max;
        let content = String::from_utf8_lossy(&buf[start..end]).to_string();
        Ok(RunLogReadResult {
            content,
            next_offset: Some(end as u64),
        })
    }

    async fn flush_inflight_mirrors(&self) -> Result<(), RunLogError> {
        // In-memory store has no throttled in-flight mirror; nothing to do.
        Ok(())
    }
}

impl From<InMemoryRunLogStore> for DynRunLogStore {
    fn from(value: InMemoryRunLogStore) -> Self {
        Arc::new(value)
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn serialize(event: &RunLogEvent) -> Result<String, RunLogError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RunLogStream;
    use chrono::TimeZone;

    fn evt(stream: RunLogStream, chunk: &str, seq: Option<u64>) -> RunLogEvent {
        RunLogEvent {
            stream,
            chunk: chunk.to_string(),
            ts: chrono::Utc.timestamp_opt(0, 0).unwrap(),
            seq,
        }
    }

    #[tokio::test]
    async fn begin_appends_serialize_and_finalize() {
        let store = InMemoryRunLogStore::new(None, "");
        let handle = store
            .begin(BeginInput {
                company_id: "co".into(),
                agent_id: "ag".into(),
                run_id: "r1".into(),
            })
            .await
            .unwrap();
        assert_eq!(handle.store, RunLogStoreType::LocalFile);
        let len = store
            .append(&handle, evt(RunLogStream::Stdout, "hello", Some(1)))
            .await
            .unwrap();
        assert!(len > 0);
        let summary = store.finalize(&handle).await.unwrap();
        assert_eq!(summary.bytes, len);
        assert!(summary.sha256.is_some());
        let read = store
            .read(&handle, RunLogReadOptions::default())
            .await
            .unwrap();
        assert!(read.content.contains("hello"));
    }
}
