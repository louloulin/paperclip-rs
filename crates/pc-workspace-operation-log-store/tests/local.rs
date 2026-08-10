//! R735: e2e for `pc-workspace-operation-log-store` using tempfile-managed dir.

use pc_workspace_operation_log_store::{
    safe_segments, AppendEvent, BeginInput, LocalFileWorkspaceOperationLogStore, LogStream,
    ReadOptions, WorkspaceOperationLogHandle, WorkspaceOperationLogStore,
};
use std::sync::Mutex;
use tempfile::TempDir;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn fresh_dir() -> TempDir {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    TempDir::new().expect("tempdir")
}

#[test]
fn safe_segments_replaces_special_chars() {
    assert_eq!(safe_segments(&["hello/world"]), vec!["hello_world"]);
    assert_eq!(safe_segments(&["a-b.c_d"]), vec!["a-b.c_d"]);
    assert_eq!(safe_segments(&["a b\tc"]), vec!["a_b_c"]);
}

#[test]
fn safe_segments_preserves_dots_dashes_underscores() {
    assert_eq!(safe_segments(&["file-1.0_v2"]), vec!["file-1.0_v2"]);
}

#[test]
fn handle_serializes_local_file_with_log_ref() {
    let h = WorkspaceOperationLogHandle::LocalFile {
        log_ref: "co-1/op-1.ndjson".to_string(),
    };
    let s = serde_json::to_string(&h).unwrap();
    assert!(s.contains("\"store\":\"local_file\""));
    assert!(s.contains("\"logRef\":\"co-1/op-1.ndjson\""));
}

#[tokio::test(flavor = "current_thread")]
async fn begin_creates_file_and_returns_handle() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "co-1".to_string(),
            operation_id: "op-1".to_string(),
        })
        .await
        .expect("begin");
    let abs = dir.path().join("co-1/op-1.ndjson");
    assert!(abs.exists());
    match &handle {
        WorkspaceOperationLogHandle::LocalFile { log_ref } => {
            assert_eq!(log_ref, "co-1/op-1.ndjson");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn append_writes_ndjson_lines() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "co-1".to_string(),
            operation_id: "op-1".to_string(),
        })
        .await
        .expect("begin");
    store
        .append(
            &handle,
            &AppendEvent {
                stream: LogStream::Stdout,
                chunk: "hello".to_string(),
                ts: "2024-01-01T00:00:00Z".to_string(),
            },
        )
        .await
        .expect("append stdout");
    store
        .append(
            &handle,
            &AppendEvent {
                stream: LogStream::Stderr,
                chunk: "oops".to_string(),
                ts: "2024-01-01T00:00:01Z".to_string(),
            },
        )
        .await
        .expect("append stderr");

    let raw = tokio::fs::read_to_string(dir.path().join("co-1/op-1.ndjson"))
        .await
        .unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"stream\":\"stdout\""));
    assert!(lines[0].contains("\"chunk\":\"hello\""));
    assert!(lines[1].contains("\"stream\":\"stderr\""));
    assert!(lines[1].contains("\"chunk\":\"oops\""));
}

#[tokio::test(flavor = "current_thread")]
async fn append_sanitizes_company_id_path_segment() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "../evil/path".to_string(),
            operation_id: "op-1".to_string(),
        })
        .await
        .expect("begin");
    // 不应创建 /evil/path/op-1.ndjson，而应在 .._evil_path/ 下创建
    let abs = dir.path().join(".._evil_path/op-1.ndjson");
    assert!(abs.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn finalize_returns_sha256_and_bytes() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "co".to_string(),
            operation_id: "op".to_string(),
        })
        .await
        .expect("begin");
    store
        .append(
            &handle,
            &AppendEvent {
                stream: LogStream::System,
                chunk: "hello".to_string(),
                ts: "ts".to_string(),
            },
        )
        .await
        .expect("append");
    let summary = store.finalize(&handle).await.expect("finalize");
    assert!(summary.bytes > 0);
    assert!(summary.sha256.is_some());
    assert!(!summary.compressed);
    // 64 hex chars
    assert_eq!(summary.sha256.unwrap().len(), 64);
}

#[tokio::test(flavor = "current_thread")]
async fn read_returns_full_content_by_default() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "co".to_string(),
            operation_id: "op".to_string(),
        })
        .await
        .expect("begin");
    store
        .append(
            &handle,
            &AppendEvent {
                stream: LogStream::Stdout,
                chunk: "hello world".to_string(),
                ts: "ts".to_string(),
            },
        )
        .await
        .expect("append");
    let r = store
        .read(&handle, ReadOptions::default())
        .await
        .expect("read");
    assert!(
        r.content.contains("hello world"),
        "content missing 'hello world', got {:?}",
        r.content
    );
    assert!(r.next_offset.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn read_with_offset_and_limit_bytes_paginates() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "co".to_string(),
            operation_id: "op".to_string(),
        })
        .await
        .expect("begin");
    for i in 0..10 {
        store
            .append(
                &handle,
                &AppendEvent {
                    stream: LogStream::Stdout,
                    chunk: format!("chunk-{i:02}-padding"),
                    ts: "ts".to_string(),
                },
            )
            .await
            .expect("append");
    }
    let total_len = tokio::fs::metadata(dir.path().join("co/op.ndjson"))
        .await
        .unwrap()
        .len();
    // 限制 50 bytes，offset=0
    let r1 = store
        .read(
            &handle,
            ReadOptions {
                offset: Some(0),
                limit_bytes: Some(50),
            },
        )
        .await
        .expect("read");
    assert_eq!(r1.content.as_bytes().len(), 50);
    assert_eq!(r1.next_offset, Some(50));

    // 第二次读 offset=50，剩余全部
    let r2 = store
        .read(
            &handle,
            ReadOptions {
                offset: Some(50),
                limit_bytes: Some(total_len * 2),
            },
        )
        .await
        .expect("read");
    assert_eq!(r2.content.as_bytes().len(), (total_len - 50) as usize);
    assert!(r2.next_offset.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn read_offset_past_end_returns_empty_with_next_offset() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "co".to_string(),
            operation_id: "op".to_string(),
        })
        .await
        .expect("begin");
    store
        .append(
            &handle,
            &AppendEvent {
                stream: LogStream::Stdout,
                chunk: "x".to_string(),
                ts: "ts".to_string(),
            },
        )
        .await
        .expect("append");
    let total = tokio::fs::metadata(dir.path().join("co/op.ndjson"))
        .await
        .unwrap()
        .len();
    let r = store
        .read(
            &handle,
            ReadOptions {
                offset: Some(total + 100),
                limit_bytes: Some(10),
            },
        )
        .await
        .expect("read");
    assert!(r.content.is_empty());
    assert_eq!(r.next_offset, Some(total + 100));
}

#[tokio::test(flavor = "current_thread")]
async fn begin_sanitizes_unsafe_operation_id() {
    let dir = fresh_dir();
    let store = LocalFileWorkspaceOperationLogStore::new(dir.path());
    let handle = store
        .begin(BeginInput {
            company_id: "co".to_string(),
            operation_id: "../etc/passwd".to_string(),
        })
        .await
        .expect("begin");
    // 不应创建 /etc/passwd.ndjson
    assert!(dir.path().join("co/.._etc_passwd.ndjson").exists());
}
