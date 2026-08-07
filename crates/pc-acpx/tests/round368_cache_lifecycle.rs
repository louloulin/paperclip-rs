//! R368 集成测试 — `pc-acpx` cache 生命周期 + env helpers。
//!
//! 覆盖：cache idle eviction、per-key async lease、env path 解析。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pc_acpx::cache::{
    cleanup_idle_with_report, AsyncKeyedLocks, IdleCache, IdleEvictionReport, LastUsed,
};
use pc_acpx::env_helpers::{ensure_path_in_env, resolve_runtime_env};

#[derive(Debug, Clone)]
struct FakeRuntime {
    label: String,
    last_used: i64,
    /// Counts how many times the runtime was dropped (closer fired).
    drop_count: Arc<AtomicUsize>,
}

impl PartialEq for FakeRuntime {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label && self.last_used == other.last_used
    }
}

impl LastUsed for FakeRuntime {
    fn last_used_at(&self) -> i64 {
        self.last_used
    }
}

fn runtime(label: &str, last_used: i64) -> FakeRuntime {
    FakeRuntime {
        label: label.into(),
        last_used,
        drop_count: Arc::new(AtomicUsize::new(0)),
    }
}

// ============================================================================
// Idle eviction end-to-end
// ============================================================================

#[tokio::test]
async fn cache_evicts_stale_entries_and_fires_closer() {
    let mut cache: IdleCache<String, FakeRuntime> = IdleCache::new();
    let fresh = runtime("fresh", 1_000_000);
    let stale_a = runtime("stale_a", 0);
    let stale_b = runtime("stale_b", 100);
    let drop_count_a = stale_a.drop_count.clone();
    let drop_count_b = stale_b.drop_count.clone();
    cache.put("fresh".into(), fresh, 1_000_000);
    cache.put("stale_a".into(), stale_a, 0);
    cache.put("stale_b".into(), stale_b, 100);

    let now = 1_000_000i64;
    let drop_counts = Arc::new(Mutex::new(Vec::new()));
    let drop_counts_for_closure = drop_counts.clone();
    let report: IdleEvictionReport<String> =
        cleanup_idle_with_report(&mut cache, now, 500, move |_key, value| {
            let drop_counts = drop_counts_for_closure.clone();
            async move {
                drop_counts.lock().unwrap().push(value.label.clone());
                value.drop_count.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;
    assert_eq!(report.evicted.len(), 2);
    assert!(report.evicted.contains(&"stale_a".to_string()));
    assert!(report.evicted.contains(&"stale_b".to_string()));
    // `fresh` survives.
    assert!(cache.contains(&"fresh".to_string()));
    assert!(!cache.contains(&"stale_a".to_string()));
    // Closer fired exactly once per eviction.
    assert_eq!(drop_count_a.load(Ordering::SeqCst), 1);
    assert_eq!(drop_count_b.load(Ordering::SeqCst), 1);
    let dropped = drop_counts.lock().unwrap().clone();
    assert_eq!(dropped.len(), 2);
    let _ = dropped;
}

// ============================================================================
// Per-key async lease end-to-end
// ============================================================================

#[tokio::test]
async fn lease_serializes_critical_section_for_same_key() {
    let locks = Arc::new(AsyncKeyedLocks::<String>::new());
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..5 {
        let locks = locks.clone();
        let in_flight = in_flight.clone();
        let max_concurrent = max_concurrent.clone();
        handles.push(tokio::spawn(async move {
            locks
                .with_lease("session-1".to_string(), || async move {
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    let prev_max = max_concurrent.load(Ordering::SeqCst);
                    if cur > prev_max {
                        max_concurrent.store(cur, Ordering::SeqCst);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                })
                .await
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
    // Single-flight semantics — at most one critical section per key.
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lease_lets_distinct_keys_run_concurrently() {
    let locks = Arc::new(AsyncKeyedLocks::<String>::new());
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for key in ["alpha", "beta", "gamma", "delta"] {
        let locks = locks.clone();
        let in_flight = in_flight.clone();
        let max_concurrent = max_concurrent.clone();
        handles.push(tokio::spawn(async move {
            locks
                .with_lease(key.to_string(), || async move {
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    let prev_max = max_concurrent.load(Ordering::SeqCst);
                    if cur > prev_max {
                        max_concurrent.store(cur, Ordering::SeqCst);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                })
                .await
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
    // Different keys run in parallel; the multi-threaded tokio runtime
    // should observe more than one concurrent critical section.
    let max = max_concurrent.load(Ordering::SeqCst);
    assert!(
        max >= 2,
        "expected concurrent execution across distinct keys, got {max}"
    );
}

// ============================================================================
// Timestamp preservation
// ============================================================================

#[test]
fn cache_preserves_timestamp_on_re_put() {
    let mut cache: IdleCache<String, FakeRuntime> = IdleCache::new();
    cache.put("k".into(), runtime("v1", 100), 100);
    // Re-put with a newer `now` does not refresh the timestamp.
    cache.put("k".into(), runtime("v2", 200), 999);
    assert_eq!(cache.last_used_at(&"k".into()), Some(100));
    assert_eq!(
        cache.get(&"k".into()).map(|r| r.label.clone()),
        Some("v2".to_string())
    );
}

#[test]
fn cache_refreshes_timestamp_on_replace() {
    let mut cache: IdleCache<String, FakeRuntime> = IdleCache::new();
    cache.put("k".into(), runtime("v1", 100), 100);
    cache.replace("k".into(), runtime("v2", 200), 999);
    assert_eq!(cache.last_used_at(&"k".into()), Some(999));
}

#[test]
fn cache_touch_updates_timestamp_without_touching_value() {
    let mut cache: IdleCache<String, FakeRuntime> = IdleCache::new();
    cache.put("k".into(), runtime("v", 100), 100);
    cache.touch(&"k".into(), 500);
    assert_eq!(cache.last_used_at(&"k".into()), Some(500));
    // Value is unchanged.
    assert_eq!(
        cache.get(&"k".into()).map(|r| r.label.clone()),
        Some("v".to_string())
    );
    // Missing key touch is a no-op.
    cache.touch(&"missing".into(), 999);
    assert!(!cache.contains(&"missing".to_string()));
}

// ============================================================================
// Env helpers end-to-end
// ============================================================================

#[test]
fn ensure_path_inserts_default_when_caller_omits_path() {
    let mut env = BTreeMap::new();
    env.insert("FOO".into(), "bar".into());
    ensure_path_in_env(&mut env);
    assert!(env.contains_key("PATH"));
    assert!(env.get("PATH").map(|v| !v.is_empty()).unwrap_or(false));
    assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
}

#[test]
fn resolve_runtime_env_overrides_process_path_when_caller_provides_one() {
    let mut caller = BTreeMap::new();
    caller.insert("PATH".into(), "/caller/bin".into());
    caller.insert("PAPERCLIP_R368".into(), "yes".into());
    let result = resolve_runtime_env(caller);
    assert_eq!(result.get("PATH"), Some(&"/caller/bin".to_string()));
    assert_eq!(result.get("PAPERCLIP_R368"), Some(&"yes".to_string()));
    // Process env is still layered in.
    assert!(result.keys().count() > 1);
}

#[test]
fn resolve_runtime_env_inserts_default_when_neither_layer_has_path() {
    let mut caller = BTreeMap::new();
    caller.insert("UNIQUE_R368".into(), "x".into());
    let result = resolve_runtime_env(caller);
    assert!(result.contains_key("PATH"));
    assert_eq!(result.get("UNIQUE_R368"), Some(&"x".to_string()));
}

// ============================================================================
// Cross-module: env + cache integration
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
struct EnvSnapshot(BTreeMap<String, String>);

impl LastUsed for EnvSnapshot {
    fn last_used_at(&self) -> i64 {
        // Marker — the real timestamp lives in the cache slot, not in
        // the snapshot itself. This impl exists only to satisfy the
        // \`LastUsed\` bound for the test cache.
        0
    }
}

#[test]
fn cache_stores_resolved_env_for_replay() {
    // Round-trip the resolved env through the cache so a future run can
    // reuse it instead of re-resolving from process env.
    let mut cache: IdleCache<String, EnvSnapshot> = IdleCache::new();
    let mut first_env = BTreeMap::new();
    first_env.insert("PATH".into(), "/a/bin".into());
    first_env.insert("PAPERCLIP_AGENT".into(), "claude".into());
    let first_resolved = EnvSnapshot(resolve_runtime_env(first_env));
    cache.put("session-A".into(), first_resolved, 100);

    // Touch the cached entry — its cached env survives.
    cache.touch(&"session-A".to_string(), 200);
    let cached = cache.get(&"session-A".to_string()).unwrap().clone();
    assert_eq!(cached.0.get("PAPERCLIP_AGENT"), Some(&"claude".to_string()));
    assert_eq!(cached.0.get("PATH"), Some(&"/a/bin".to_string()));
    assert_eq!(cache.last_used_at(&"session-A".to_string()), Some(200));
}

// ============================================================================
// Helpers
// ============================================================================

use std::sync::Mutex;
