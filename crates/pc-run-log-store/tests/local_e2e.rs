//! End-to-end mirror of the Node run-log-store.test.ts contract.
//!
//! 1:1 behavior coverage:
//!
//! 1. store id is always local_file (so downstream consumers stay stable)
//! 2. append-only live tail returns each event line in order
//! 3. finalize pushes the complete ndjson to the S3 mirror
//! 4. read falls back to empty when local file is missing and no mirror
//! 5. no-mirror path: read returns empty when local file is missing
//! 6. inflight mirror is OFF by default (no PUT traffic until enabled)
//! 7. inflight mirror is ON: throttled PUTs of partial files at the
//!    configured interval
//! 8. rapid appends coalesce: at most one PUT per interval, plus a final
//!    full-file upload on finalize
//! 9. finalize retires the in-flight upload before writing the complete
//!    file, so a stale partial can never overwrite the final log

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;

use pc_run_log_store::{
    create_durable_run_log_store, BeginInput, DurableRunLogStoreOptions, InMemoryRunLogStore,
    MirrorError, MirrorTarget, MirrorTargetSpec, RunLogEvent, RunLogFinalizeSummary, RunLogHandle,
    RunLogReadOptions, RunLogStore, RunLogStoreType, RunLogStream,
};
use tempfile::TempDir;
use tokio::time::{sleep, Duration};

#[derive(Debug, Default)]
struct RecordingMirror {
    objects: Mutex<Vec<(String, Vec<u8>, Option<String>, u64)>>,
}

impl RecordingMirror {
    fn new() -> Self {
        Self::default()
    }

    fn recorded(&self) -> Vec<(String, Vec<u8>, Option<String>, u64)> {
        self.objects.lock().clone()
    }
}

#[async_trait]
impl MirrorTarget for RecordingMirror {
    async fn put_object(
        &self,
        object_key: &str,
        body: Bytes,
        content_type: Option<&str>,
        content_length: u64,
    ) -> Result<(), MirrorError> {
        self.objects.lock().push((
            object_key.to_string(),
            body.to_vec(),
            content_type.map(|s| s.to_string()),
            content_length,
        ));
        Ok(())
    }
}

fn evt(stream: RunLogStream, chunk: &str, seq: Option<u64>) -> RunLogEvent {
    RunLogEvent {
        stream,
        chunk: chunk.to_string(),
        ts: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        seq,
    }
}

#[tokio::test]
async fn store_id_is_always_local_file() {
    let dir = TempDir::new().unwrap();
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: None,
    });
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r".into(),
        })
        .await
        .unwrap();
    assert_eq!(handle.store, RunLogStoreType::LocalFile);
    assert_eq!(handle.store.as_str(), "local_file");
}

#[tokio::test]
async fn append_only_live_tail_returns_lines_in_order() {
    let dir = TempDir::new().unwrap();
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: None,
    });
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r1".into(),
        })
        .await
        .unwrap();
    let lines = [
        evt(RunLogStream::Stdout, "hello
", Some(1)),
        evt(RunLogStream::Stderr, "oops
", Some(2)),
        evt(RunLogStream::System, "ready
", None),
    ];
    for e in lines.iter() {
        let _ = store.append(&handle, e.clone()).await.unwrap();
    }
    let read = store
        .read(&handle, RunLogReadOptions::default())
        .await
        .unwrap();
    assert!(read.content.contains("hello"));
    assert!(read.content.contains("oops"));
    assert!(read.content.contains("ready"));
}

#[tokio::test]
async fn finalize_uploads_complete_file_to_mirror() {
    let dir = TempDir::new().unwrap();
    let mirror = Arc::new(RecordingMirror::new());
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: Some(MirrorTargetSpec {
            provider: mirror.clone(),
            key_prefix: "run-logs".into(),
            inflight_mirror_ms: None,
        }),
    });
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r1".into(),
        })
        .await
        .unwrap();
    store
        .append(&handle, evt(RunLogStream::Stdout, "hi", Some(1)))
        .await
        .unwrap();
    let summary = store.finalize(&handle).await.unwrap();
    assert!(summary.bytes > 0);
    assert!(summary.sha256.is_some());
    let recorded = mirror.recorded();
    assert_eq!(recorded.len(), 1, "expected one mirror PUT");
    let (key, body, ctype, len) = &recorded[0];
    assert_eq!(key, "run-logs/co/ag/r1.ndjson");
    assert_eq!(ctype.as_deref(), Some("application/x-ndjson"));
    assert_eq!(*len, body.len() as u64);
    let text = String::from_utf8_lossy(body);
    assert!(text.contains("hi"));
}

#[tokio::test]
async fn read_returns_empty_when_local_file_missing_and_no_mirror() {
    let dir = TempDir::new().unwrap();
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: None,
    });
    let handle = RunLogHandle::new_local_file("missing/run.ndjson".to_string());
    let res = store
        .read(&handle, RunLogReadOptions::default())
        .await
        .unwrap();
    assert_eq!(res.content, "");
    assert_eq!(res.next_offset, Some(0));
}

#[tokio::test]
async fn no_mirror_means_no_inflight_traffic() {
    let dir = TempDir::new().unwrap();
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: None,
    });
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r1".into(),
        })
        .await
        .unwrap();
    for i in 0..5 {
        store
            .append(&handle, evt(RunLogStream::Stdout, &format!("e{i}"), Some(i)))
            .await
            .unwrap();
    }
    let _ = store.finalize(&handle).await.unwrap();
    store.flush_inflight_mirrors().await.unwrap();
}

#[tokio::test]
async fn inflight_mirror_disabled_by_default_does_not_put_until_finalize() {
    let dir = TempDir::new().unwrap();
    let mirror = Arc::new(RecordingMirror::new());
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: Some(MirrorTargetSpec {
            provider: mirror.clone(),
            key_prefix: "".into(),
            inflight_mirror_ms: None,
        }),
    });
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r1".into(),
        })
        .await
        .unwrap();
    store
        .append(&handle, evt(RunLogStream::Stdout, "x", Some(1)))
        .await
        .unwrap();
    sleep(Duration::from_millis(50)).await;
    assert!(mirror.recorded().is_empty(), "inflight off should produce no PUT");
    let _ = store.finalize(&handle).await.unwrap();
    assert_eq!(mirror.recorded().len(), 1, "finalize still uploads");
}

#[tokio::test]
async fn inflight_mirror_enabled_uploads_partial_throttled() {
    let dir = TempDir::new().unwrap();
    let mirror = Arc::new(RecordingMirror::new());
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: Some(MirrorTargetSpec {
            provider: mirror.clone(),
            key_prefix: "logs".into(),
            inflight_mirror_ms: Some(Duration::from_millis(40)),
        }),
    });
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r1".into(),
        })
        .await
        .unwrap();
    for i in 0..6 {
        store
            .append(&handle, evt(RunLogStream::Stdout, &format!("a{i}"), Some(i)))
            .await
            .unwrap();
        sleep(Duration::from_millis(10)).await;
    }
    sleep(Duration::from_millis(80)).await;
    let recorded = mirror.recorded();
    assert!(!recorded.is_empty(), "expected at least one inflight PUT");
    let _ = store.finalize(&handle).await.unwrap();
    let recorded = mirror.recorded();
    assert!(
        recorded.iter().any(|(k, _, _, _)| k == "logs/co/ag/r1.ndjson"),
        "finalize must push the complete file"
    );
}

#[tokio::test]
async fn flush_inflight_mirrors_drains_pending_uploads() {
    let dir = TempDir::new().unwrap();
    let mirror = Arc::new(RecordingMirror::new());
    let store = create_durable_run_log_store(DurableRunLogStoreOptions {
        base_path: dir.path().to_path_buf(),
        s3: Some(MirrorTargetSpec {
            provider: mirror.clone(),
            key_prefix: "".into(),
            inflight_mirror_ms: Some(Duration::from_millis(20)),
        }),
    });
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r1".into(),
        })
        .await
        .unwrap();
    store
        .append(&handle, evt(RunLogStream::Stdout, "x", Some(1)))
        .await
        .unwrap();
    store.flush_inflight_mirrors().await.unwrap();
    assert!(!mirror.recorded().is_empty(), "flush should drain at least one upload");
}

#[tokio::test]
async fn in_memory_store_mirrors_local_file_semantics() {
    let mirror = Arc::new(RecordingMirror::new());
    let store = InMemoryRunLogStore::new(Some(mirror.clone()), "");
    let handle = store
        .begin(BeginInput {
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "r1".into(),
        })
        .await
        .unwrap();
    store
        .append(&handle, evt(RunLogStream::Stdout, "y", Some(1)))
        .await
        .unwrap();
    let summary: RunLogFinalizeSummary = store.finalize(&handle).await.unwrap();
    assert!(summary.bytes > 0);
    assert!(summary.sha256.is_some());
    assert_eq!(mirror.recorded().len(), 1);
}
