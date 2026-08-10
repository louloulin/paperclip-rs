//! E2E tests for `pc-agent-start-lock`.
//!
//! 与 Node `server/src/__tests__/heartbeat-start-lock.test.ts` 1:1 对齐。

use futures::FutureExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pc_agent_start_lock::{
    clear_all_locks_for_tests, is_locked, with_agent_start_lock,
    AGENT_START_LOCK_STALE_MS,
};
use tokio::sync::Mutex;

// ============================================================================
// Stale timeout
// ============================================================================

#[tokio::test]
async fn r671_stale_lock_does_not_freeze_later_queued_run_starts() {
    clear_all_locks_for_tests();

    let first_started = Arc::new(AtomicUsize::new(0));
    let second_started = Arc::new(AtomicUsize::new(0));

    let fs = first_started.clone();
    // First start: never resolves (simulates dead lock).
    let first_handle = tokio::spawn(async move {
        with_agent_start_lock("agent-stale", || async move {
            fs.fetch_add(1, Ordering::SeqCst);
            // Yield once so caller can observe `firstStarted == 1`.
            tokio::time::sleep(Duration::from_millis(1)).await;
            // Then sleep "forever" — never resolves.
            std::future::pending::<()>().await
        })
        .await;
    });

    // Let first start execute.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(first_started.load(Ordering::SeqCst), 1);
    assert!(is_locked("agent-stale"));

    let ss = second_started.clone();
    let second_handle = tokio::spawn(async move {
        with_agent_start_lock("agent-stale", || async move {
            ss.fetch_add(1, Ordering::SeqCst);
            "started"
        })
        .await
    });

    // Give second a moment — should NOT have started because first is still pending.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        second_started.load(Ordering::SeqCst),
        0,
        "second caller should wait while first is fresh"
    );

    // Advance past 30s — second should now bypass stale lock.
    tokio::time::sleep(Duration::from_millis(AGENT_START_LOCK_STALE_MS + 100)).await;
    assert_eq!(
        second_started.load(Ordering::SeqCst),
        1,
        "second caller should run after stale timeout"
    );

    let result = second_handle.await.unwrap();
    assert_eq!(result, "started");

    // Abort first (it's still pending forever).
    first_handle.abort();
}

// ============================================================================
// Lock cleanup
// ============================================================================

#[tokio::test]
async fn r671_lock_is_removed_after_completion() {
    clear_all_locks_for_tests();

    with_agent_start_lock("agent-cleanup", || async move {
        // do nothing
    })
    .await;

    assert!(!is_locked("agent-cleanup"));
}

#[tokio::test]
async fn r671_lock_present_during_execution() {
    clear_all_locks_for_tests();

    let observed = Arc::new(Mutex::new(false));
    let o = observed.clone();
    with_agent_start_lock("agent-during", || async move {
        *o.lock().await = is_locked("agent-during");
    })
    .await;

    assert!(
        *observed.lock().await,
        "lock should be present while future is running"
    );
    assert!(!is_locked("agent-during"), "lock should be cleaned up");
}

// ============================================================================
// Serialization per agent
// ============================================================================

#[tokio::test]
async fn r671_same_agent_serializes_through_queue() {
    clear_all_locks_for_tests();

    let order = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();

    for i in 0..3 {
        let order = order.clone();
        let id = format!("agent-q-{i}");
        handles.push(tokio::spawn(async move {
            with_agent_start_lock("agent-q", || async move {
                order.lock().await.push(format!("start-{id}"));
                tokio::time::sleep(Duration::from_millis(10)).await;
                order.lock().await.push(format!("end-{id}"));
            })
            .await;
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let log = order.lock().await.clone();
    // Each call to agent-q should run start-then-end before any subsequent call starts.
    // Walk log: expect "start-N" immediately followed by "end-N" before next "start-N+1".
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    let mut current_start: Option<&str> = None;
    for entry in &log {
        if entry.starts_with("start-") {
            current_start = Some(entry.as_str());
        } else if entry.starts_with("end-") {
            if let Some(s) = current_start.take() {
                pairs.push((s, entry.as_str()));
            }
        }
    }
    assert_eq!(pairs.len(), 3, "expected 3 serialized pairs, got {log:?}");
}

#[tokio::test]
async fn r671_lock_count_grows_with_concurrent_agents() {
    clear_all_locks_for_tests();

    let mut handles = Vec::new();
    for i in 0..5 {
        let id: Arc<str> = format!("agent-conc-{i}").into();
        let id_inner = id.clone();
        handles.push(tokio::spawn(async move {
            with_agent_start_lock(&id, || async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                assert!(is_locked(&id_inner));
            })
            .await;
        }));
    }

    tokio::time::sleep(Duration::from_millis(5)).await;
    for i in 0..5 {
        let id = format!("agent-conc-{i}");
        assert!(is_locked(&id), "agent {id} should be locked");
    }

    for h in handles {
        h.await.unwrap();
    }
    for i in 0..5 {
        let id = format!("agent-conc-{i}");
        assert!(!is_locked(&id), "agent {id} should be released");
    }
}

// ============================================================================
// Value passthrough
// ============================================================================

#[tokio::test]
async fn r671_returns_future_result() {
    clear_all_locks_for_tests();

    let r1 = with_agent_start_lock("a", || async { 42 }).await;
    assert_eq!(r1, 42);

    let r2 = with_agent_start_lock("a", || async { String::from("hi") }).await;
    assert_eq!(r2, "hi");

    let r3 = with_agent_start_lock("a", || async { Vec::<i32>::new() }).await;
    assert!(r3.is_empty());
}

#[tokio::test]
async fn r671_propagates_panic_via_future() {
    clear_all_locks_for_tests();
    let result = std::panic::AssertUnwindSafe(with_agent_start_lock("a", || async move {
        panic!("intentional");
    }))
    .catch_unwind()
    .await;
    assert!(result.is_err(), "panic should propagate");
    assert!(!is_locked("a"), "lock should still be cleaned up after panic");
}
