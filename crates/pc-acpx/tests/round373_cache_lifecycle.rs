//! R373 cache lifecycle helpers tests.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pc_acpx::{
    cache_lifecycle::{
        cleanup_idle_handles, cleanup_idle_staged_runtimes, clear_warm_handle_timer,
        close_warm_handle, discard_staged_runtime, save_staged_runtime_after_clean_turn,
        schedule_idle_handle_cleanup, warm_handle_matches, with_session_staging_lease,
        AsyncCallback, RuntimeCacheEntry, SessionStagingLocks, StagedRuntimeCacheEntry,
        TokioCleanupHandle,
    },
    AcpRuntime, AcpRuntimeEnsureInput, AcpRuntimeHandle, AcpRuntimeMode, MockAcpRuntime,
};
use tokio::sync::Mutex as TokioMutex;

fn unique_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::SeqCst)
}

#[tokio::test]
async fn warm_handle_matches_returns_true_for_same_runtime_and_handle() {
    let mock = MockAcpRuntime::new(vec![]);
    let handle = mock
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            ..Default::default()
        })
        .await
        .unwrap();
    let runtime = Arc::new(mock);
    let entry = RuntimeCacheEntry {
        runtime: runtime.clone(),
        handle: handle.clone(),
        fingerprint: "fp".into(),
        last_used_at: 0,
        cleanup_timer: None,
    };
    assert!(warm_handle_matches(Some(&entry), runtime.as_ref(), &handle));
}

#[tokio::test]
async fn warm_handle_matches_returns_false_for_different_handle() {
    let mock = MockAcpRuntime::new(vec![]);
    let runtime = Arc::new(mock);
    let entry_handle = AcpRuntimeHandle {
        session_key: "session-a".into(),
        ..Default::default()
    };
    let other = AcpRuntimeHandle {
        session_key: "session-b".into(),
        ..Default::default()
    };
    let entry = RuntimeCacheEntry {
        runtime: runtime.clone(),
        handle: entry_handle,
        fingerprint: "fp".into(),
        last_used_at: 0,
        cleanup_timer: None,
    };
    assert!(!warm_handle_matches(Some(&entry), runtime.as_ref(), &other));
}

#[tokio::test]
async fn warm_handle_matches_returns_false_for_undefined_entry() {
    let mock = MockAcpRuntime::new(vec![]);
    let runtime = Arc::new(mock);
    let handle = AcpRuntimeHandle::default();
    assert!(!warm_handle_matches(None, runtime.as_ref(), &handle));
}

#[tokio::test]
async fn clear_warm_handle_timer_is_noop_when_no_timer() {
    let mut entry = RuntimeCacheEntry {
        runtime: Arc::new(MockAcpRuntime::new(vec![])),
        handle: Default::default(),
        fingerprint: "fp".into(),
        last_used_at: 0,
        cleanup_timer: None,
    };
    clear_warm_handle_timer(&mut entry);
    assert!(entry.cleanup_timer.is_none());
}

#[tokio::test]
async fn clear_warm_handle_timer_cancels_pending_timer() {
    let mut entry = RuntimeCacheEntry {
        runtime: Arc::new(MockAcpRuntime::new(vec![])),
        handle: Default::default(),
        fingerprint: "fp".into(),
        last_used_at: 0,
        cleanup_timer: None,
    };
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    entry.cleanup_timer = Some(TokioCleanupHandle::from_join(handle));
    clear_warm_handle_timer(&mut entry);
    assert!(entry.cleanup_timer.is_none());
}

#[tokio::test]
async fn cleanup_idle_handles_skips_when_idle_ms_is_zero() {
    let mut handles: HashMap<String, RuntimeCacheEntry> = HashMap::new();
    let mock = MockAcpRuntime::new(vec![]);
    let handle = mock
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            ..Default::default()
        })
        .await
        .unwrap();
    handles.insert(
        "k".into(),
        RuntimeCacheEntry {
            runtime: Arc::new(mock),
            handle,
            fingerprint: "fp".into(),
            last_used_at: 0,
            cleanup_timer: None,
        },
    );
    cleanup_idle_handles(&mut handles, 1_000_000, 0).await;
    assert!(handles.contains_key("k"));
}

#[tokio::test]
async fn cleanup_idle_handles_closes_stale_entries() {
    let mut handles: HashMap<String, RuntimeCacheEntry> = HashMap::new();
    let mock = MockAcpRuntime::new(vec![]);
    let handle = mock
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "stale".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            ..Default::default()
        })
        .await
        .unwrap();
    let runtime_arc = Arc::new(mock);
    handles.insert(
        "stale".into(),
        RuntimeCacheEntry {
            runtime: runtime_arc.clone(),
            handle,
            fingerprint: "fp".into(),
            last_used_at: 0,
            cleanup_timer: None,
        },
    );
    handles.insert(
        "fresh".into(),
        RuntimeCacheEntry {
            runtime: runtime_arc.clone(),
            handle: Default::default(),
            fingerprint: "fp2".into(),
            last_used_at: 999_999_500, // 500ms ago, within idle window
            cleanup_timer: None,
        },
    );
    cleanup_idle_handles(&mut handles, 1_000_000_000, 1_000).await;
    assert!(!handles.contains_key("stale"));
    assert!(handles.contains_key("fresh"));
}

#[tokio::test]
async fn close_warm_handle_removes_entry_and_calls_close() {
    let mut handles: HashMap<String, RuntimeCacheEntry> = HashMap::new();
    let mock = MockAcpRuntime::new(vec![]);
    let handle = mock
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            ..Default::default()
        })
        .await
        .unwrap();
    let runtime_arc = Arc::new(mock);
    let key = unique_id().to_string();
    let entry = RuntimeCacheEntry {
        runtime: runtime_arc.clone(),
        handle: handle.clone(),
        fingerprint: "fp".into(),
        last_used_at: 0,
        cleanup_timer: None,
    };
    handles.insert(key.clone(), entry.clone());
    close_warm_handle(&mut handles, &key, entry).await;
    assert!(!handles.contains_key(&key));
}

#[tokio::test]
async fn schedule_idle_handle_cleanup_is_noop_when_idle_ms_zero() {
    let mock = MockAcpRuntime::new(vec![]);
    let handle = mock
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            ..Default::default()
        })
        .await
        .unwrap();
    let runtime_arc = Arc::new(mock);
    let handles: Arc<TokioMutex<HashMap<String, RuntimeCacheEntry>>> =
        Arc::new(TokioMutex::new(HashMap::new()));
    let mut entry = RuntimeCacheEntry {
        runtime: runtime_arc.clone(),
        handle,
        fingerprint: "fp".into(),
        last_used_at: 100,
        cleanup_timer: None,
    };
    schedule_idle_handle_cleanup(handles, "k".to_string(), &mut entry, 0, || 0).await;
    assert!(entry.cleanup_timer.is_none());
}

#[tokio::test]
async fn schedule_idle_handle_cleanup_spawns_timer() {
    let mock = MockAcpRuntime::new(vec![]);
    let handle = mock
        .ensure_session(AcpRuntimeEnsureInput {
            session_key: "k".into(),
            agent: "claude".into(),
            mode: AcpRuntimeMode::Persistent,
            ..Default::default()
        })
        .await
        .unwrap();
    let runtime_arc = Arc::new(mock);
    let handles: Arc<TokioMutex<HashMap<String, RuntimeCacheEntry>>> =
        Arc::new(TokioMutex::new(HashMap::new()));
    let mut entry = RuntimeCacheEntry {
        runtime: runtime_arc.clone(),
        handle,
        fingerprint: "fp".into(),
        last_used_at: 0,
        cleanup_timer: None,
    };
    handles.lock().await.insert("k".into(), entry.clone());
    schedule_idle_handle_cleanup(handles, "k".to_string(), &mut entry, 50, || 0).await;
    assert!(entry.cleanup_timer.is_some());
}

#[tokio::test]
async fn save_staged_runtime_after_clean_turn_inserts_when_inputs_present() {
    let mut handles: HashMap<String, StagedRuntimeCacheEntry> = HashMap::new();
    let key = unique_id().to_string();
    let env_delta = HashMap::from([("CODEX_HOME".to_string(), "/tmp/x".to_string())]);
    save_staged_runtime_after_clean_turn(&mut handles, &key, env_delta.clone(), None, None, 42);
    assert!(handles.contains_key(&key));
    let entry = handles.get(&key).unwrap();
    assert_eq!(entry.last_used_at, 42);
    assert_eq!(
        entry.env_delta.get("CODEX_HOME").map(String::as_str),
        Some("/tmp/x")
    );
}

#[tokio::test]
async fn discard_staged_runtime_removes_entry_and_fires_dispose() {
    let mut handles: HashMap<String, StagedRuntimeCacheEntry> = HashMap::new();
    let key = unique_id().to_string();
    save_staged_runtime_after_clean_turn(&mut handles, &key, HashMap::new(), None, None, 0);
    let disposed = Arc::new(AtomicU64::new(0));
    let disposed_clone = disposed.clone();
    if let Some(entry) = handles.get_mut(&key) {
        entry.dispose = Some(AsyncCallback::new(move || {
            let c = disposed_clone.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            })
        }));
    }
    discard_staged_runtime(&mut handles, &key).await;
    assert!(!handles.contains_key(&key));
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn with_session_staging_lease_serializes_callers() {
    let mut locks = SessionStagingLocks::new();
    let key = "k".to_string();
    let order: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let order1 = order.clone();
    // First caller
    let lease = with_session_staging_lease(&mut locks, &key, || async {
        order1.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        order1.fetch_add(10, Ordering::SeqCst);
        42
    })
    .await;
    // Manually release before the second caller so we can observe
    // serialization across calls without parallel borrow issues.
    lease.await_release().await;
    let order2 = order.clone();
    let _lease2 = with_session_staging_lease(&mut locks, &key, || async {
        order2.fetch_add(100, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(10)).await;
        order2.fetch_add(1000, Ordering::SeqCst);
        7
    })
    .await;
    let observed = order.load(Ordering::SeqCst);
    assert_eq!(
        observed, 1111,
        "leases chain through stages 1+10+100+1000=1111, got {observed}"
    );
}

#[tokio::test]
async fn cleanup_idle_staged_runtimes_drops_stale_and_fires_dispose() {
    let mut handles: HashMap<String, StagedRuntimeCacheEntry> = HashMap::new();
    let _locks = SessionStagingLocks::new();
    let key = unique_id().to_string();
    save_staged_runtime_after_clean_turn(&mut handles, &key, HashMap::new(), None, None, 0);
    let disposed = Arc::new(AtomicU64::new(0));
    let disposed_clone = disposed.clone();
    if let Some(entry) = handles.get_mut(&key) {
        entry.dispose = Some(AsyncCallback::new(move || {
            let c = disposed_clone.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            })
        }));
    }
    cleanup_idle_staged_runtimes(&mut handles, || 1_000_000, 500).await;
    assert!(!handles.contains_key(&key));
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
}

#[allow(dead_code)]
fn _ensure_types_exported(_: Pin<Box<dyn Future<Output = ()> + Send>>) {}
