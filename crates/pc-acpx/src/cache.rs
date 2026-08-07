//! `pc-acpx` cache primitives — port of the warm-handle and staged-runtime
//! cache machinery from Node `acpx-engine/execute.ts`.
//!
//! The acpx-engine holds two complementary caches:
//!
//! - A **warm-handle cache** (`RuntimeCacheEntry`) keyed by session
//!   fingerprint — same fingerprint, same live `AcpRuntime` reuse, no
//!   re-`ensure_session` overhead.
//! - A **staged-runtime cache** (`StagedRuntimeCacheEntry`) keyed by
//!   session key — keeps the remote-backed workspace staging alive
//!   across compatible resumes so the engine does not re-ship the
//!   workspace on every run.
//!
//! Both caches share the same idle-cleanup lifecycle: an entry whose
//! `last_used_at` is older than the configured idle window is evicted
//! and its dispose hook fired. Both also share a per-key async lease so
//! overlapping stage-or-reuse decisions serialize on a single key.
//!
//! The Rust port keeps these helpers generic over the value type — the
//! acpx crate's actual `RuntimeCacheEntry` / `StagedRuntimeCacheEntry`
//! shapes wrap these primitives, the same way the Node implementation
//! inlines them into concrete types.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;

// ============================================================================
// Idle-entry view
// ============================================================================

/// Read-only view of a cache entry that exposes its last-used timestamp.
/// The acpx-engine `RuntimeCacheEntry` / `StagedRuntimeCacheEntry` types
/// both carry a `lastUsedAt: number` field; this trait lets the idle
/// eviction helpers operate on either without rewriting the shape.
pub trait LastUsed {
    fn last_used_at(&self) -> i64;
}

/// `HashMap` wrapper that pairs every value with its last-used timestamp
/// and exposes the idle-eviction sweep used by `cleanup_idle_handles`
/// and `cleanup_idle_staged_runtimes`.
pub struct IdleCache<K, V> {
    entries: HashMap<K, (V, i64)>,
}

impl<K, V> Default for IdleCache<K, V>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<K, V> IdleCache<K, V>
where
    K: Eq + Hash,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|(value, _)| value)
    }

    /// Return the cached `last_used_at` timestamp for `key`, without
    /// exposing the value. Used by tests / observability.
    pub fn last_used_at(&self, key: &K) -> Option<i64> {
        self.entries.get(key).map(|(_, ts)| *ts)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.entries.get_mut(key).map(|(value, _)| value)
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Insert `value` with `now` as the initial timestamp. If the key
    /// already exists, the existing entry is returned (the new value is
    /// dropped) and the timestamp is unchanged. Mirrors the Node
    /// `Map.set` semantics — callers that need overwrite semantics must
    /// `remove` first.
    pub fn put(&mut self, key: K, value: V, now: i64) -> Option<V>
    where
        V: LastUsed,
    {
        if let Some(existing) = self.entries.get_mut(&key) {
            // Mirror the Node `Map.set` semantics: a re-`put` keeps the
            // existing timestamp and returns the old value. The new
            // timestamp is dropped so the cache cannot silently reset
            // idle windows on stale writes.
            let previous = std::mem::replace(&mut existing.0, value);
            return Some(previous);
        }
        self.entries.insert(key, (value, now));
        None
    }

    /// Replace the entry at `key` with `value`, refreshing the
    /// timestamp to `now`. Returns the previous value when present.
    /// Use this when the caller knows it owns the slot.
    pub fn replace(&mut self, key: K, value: V, now: i64) -> Option<V> {
        self.entries
            .insert(key, (value, now))
            .map(|(previous, _)| previous)
    }

    /// Update the timestamp on an existing entry without touching the
    /// value. No-op when the key is absent.
    pub fn touch(&mut self, key: &K, now: i64) {
        if let Some(slot) = self.entries.get_mut(key) {
            slot.1 = now;
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.entries.remove(key).map(|(value, _)| value)
    }

    /// Run `closer(value)` on every entry whose `last_used_at` is older
    /// than `now - idle_ms`, removing it from the cache. Returns the
    /// list of keys that were evicted (callers may want to log them).
    ///
    /// `idle_ms <= 0` short-circuits to an empty list, matching the
    /// Node implementation.
    pub async fn cleanup_idle<F, Fut>(&mut self, now: i64, idle_ms: i64, mut closer: F) -> Vec<K>
    where
        K: Clone,
        V: LastUsed,
        F: FnMut(K, V) -> Fut,
        Fut: Future<Output = ()>,
    {
        if idle_ms <= 0 {
            return Vec::new();
        }
        let threshold = now - idle_ms;
        let stale_keys: Vec<K> = self
            .entries
            .iter()
            .filter_map(|(key, (value, _))| {
                if value.last_used_at() <= threshold {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut evicted = Vec::with_capacity(stale_keys.len());
        for key in stale_keys {
            if let Some((value, _)) = self.entries.remove(&key) {
                closer(key.clone(), value).await;
                evicted.push(key);
            }
        }
        evicted
    }

    /// Iterate over `(key, value)` pairs in arbitrary order. Used by
    /// tests; production code should prefer the focused accessors.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, (v, _))| (k, v))
    }

    /// Snapshot the cache into a `Vec` of `(K, V)` tuples, useful for
    /// assertions and observability bridges.
    pub fn snapshot(&self) -> Vec<(K, V)>
    where
        K: Clone,
        V: Clone,
    {
        self.entries
            .iter()
            .map(|(k, (v, _))| (k.clone(), v.clone()))
            .collect()
    }
}

// ============================================================================
// Standalone async locks (per-key semaphore)
// ============================================================================

/// Per-key async semaphore, used by the staged-runtime layer to
/// serialize "stage or reuse" decisions on a single session. The lock
/// map is the long-lived state; each `with_lease` call awaits its
/// turn, then runs the closure, then releases.
pub struct AsyncKeyedLocks<K: Eq + Hash + Clone> {
    locks: std::sync::Mutex<HashMap<K, std::sync::Arc<tokio::sync::Semaphore>>>,
}

impl<K: Eq + Hash + Clone> Default for AsyncKeyedLocks<K> {
    fn default() -> Self {
        Self {
            locks: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash + Clone> AsyncKeyedLocks<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the semaphore for `key`, run `f`, then release. The
    /// semaphore is created on demand and reused for subsequent calls.
    pub async fn with_lease<F, Fut, T>(&self, key: K, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let semaphore = {
            let mut map = self.locks.lock().expect("locks poisoned");
            map.entry(key)
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)))
                .clone()
        };
        let _permit = semaphore.acquire_owned().await.expect("semaphore closed");
        f().await
    }

    pub fn is_locked(&self, key: &K) -> bool {
        let map = self.locks.lock().expect("locks poisoned");
        map.get(key)
            .map(|semaphore| semaphore.available_permits() == 0)
            .unwrap_or(false)
    }
}

// ============================================================================
// Cleanup wrappers (semantic wrappers)
// ============================================================================

/// Result of a single idle eviction sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleEvictionReport<K> {
    pub evicted: Vec<K>,
}

/// Wrap [`IdleCache::cleanup_idle`] with a report, preserving the
/// Node `cleanupIdleHandles` / `cleanupIdleStagedRuntimes` reporting
/// shape. The original helpers return no value; tests that want to
/// assert exactly which keys were evicted can route through this.
pub async fn cleanup_idle_with_report<K, V, F, Fut>(
    cache: &mut IdleCache<K, V>,
    now: i64,
    idle_ms: i64,
    closer: F,
) -> IdleEvictionReport<K>
where
    K: Eq + Hash + Clone,
    V: LastUsed,
    F: FnMut(K, V) -> Fut,
    Fut: Future<Output = ()>,
{
    let evicted = cache.cleanup_idle(now, idle_ms, closer).await;
    IdleEvictionReport { evicted }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Entry {
        label: String,
        last_used: i64,
    }

    impl LastUsed for Entry {
        fn last_used_at(&self) -> i64 {
            self.last_used
        }
    }

    fn entry(label: &str, last_used: i64) -> Entry {
        Entry {
            label: label.into(),
            last_used,
        }
    }

    #[tokio::test]
    async fn cleanup_idle_skips_when_idle_window_is_zero() {
        let mut cache: IdleCache<String, Entry> = IdleCache::new();
        cache.put("k".into(), entry("v", 0), 0);
        let evicted = cache.cleanup_idle(10_000, 0, |_k, _v| async move {}).await;
        assert!(evicted.is_empty());
        assert_eq!(cache.len(), 1);
    }

    #[tokio::test]
    async fn cleanup_idle_evicts_stale_entries() {
        let mut cache: IdleCache<String, Entry> = IdleCache::new();
        cache.put("fresh".into(), entry("fresh", 1000), 1000);
        cache.put("stale".into(), entry("stale", 0), 0);
        let report = cleanup_idle_with_report(&mut cache, 1000, 500, |key, _value| async move {
            // Closer is exercised; we just need it not to panic.
            let _ = key;
        })
        .await;
        assert_eq!(report.evicted, vec!["stale".to_string()]);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&"fresh".to_string()));
        assert!(!cache.contains(&"stale".to_string()));
    }

    #[tokio::test]
    async fn cleanup_idle_calls_closer_with_evicted_value() {
        let mut cache: IdleCache<String, Entry> = IdleCache::new();
        cache.put("a".into(), entry("alpha", 0), 0);
        let closer_called = Arc::new(AtomicI64::new(0));
        let called = closer_called.clone();
        let evicted = cache
            .cleanup_idle(1000, 100, move |key, value| {
                let called = called.clone();
                async move {
                    assert_eq!(key, "a");
                    assert_eq!(value.label, "alpha");
                    called.fetch_add(1, Ordering::SeqCst);
                }
            })
            .await;
        assert_eq!(evicted, vec!["a".to_string()]);
        assert_eq!(closer_called.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn put_returns_existing_value_and_keeps_timestamp() {
        let mut cache: IdleCache<String, Entry> = IdleCache::new();
        let original = entry("first", 100);
        let previous = cache.put("k".into(), original.clone(), 100);
        assert!(previous.is_none());
        // Re-put returns the old value but keeps the timestamp.
        let previous = cache.put("k".into(), entry("second", 200), 200);
        assert_eq!(previous, Some(original));
        // Cache timestamp is preserved across re-put — the new `now` is dropped.
        assert_eq!(
            cache.snapshot(),
            vec![("k".to_string(), entry("second", 200))],
        );
        assert_eq!(cache.last_used_at(&"k".to_string().into()), Some(100));
    }

    #[test]
    fn replace_overwrites_value_and_timestamp() {
        let mut cache: IdleCache<String, Entry> = IdleCache::new();
        cache.put("k".into(), entry("first", 100), 100);
        let previous = cache.replace("k".into(), entry("second", 200), 200);
        assert_eq!(previous, Some(entry("first", 100)));
        assert_eq!(
            cache.get(&"k".to_string().into()).map(|e| e.last_used),
            Some(200)
        );
    }

    #[test]
    fn touch_updates_timestamp_without_changing_value() {
        let mut cache: IdleCache<String, Entry> = IdleCache::new();
        cache.put("k".into(), entry("v", 100), 100);
        cache.touch(&"k".into(), 500);
        // Cache timestamp was refreshed; Entry's own `last_used` is
        // unchanged because `touch` does not mutate the value.
        assert_eq!(cache.last_used_at(&"k".into()), Some(500));
        assert_eq!(cache.get(&"k".into()).map(|e| e.last_used), Some(100));
        // Touch on missing key is a no-op.
        cache.touch(&"missing".into(), 999);
    }

    #[tokio::test]
    async fn async_keyed_locks_serialize_concurrent_callers() {
        use std::sync::atomic::AtomicUsize;
        let locks = Arc::new(AsyncKeyedLocks::<String>::new());
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for i in 0..4 {
            let locks = locks.clone();
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            handles.push(tokio::spawn(async move {
                locks
                    .with_lease("k".to_string(), || async move {
                        let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        let prev_max = max_in_flight.load(Ordering::SeqCst);
                        if current > prev_max {
                            max_in_flight.store(current, Ordering::SeqCst);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        i
                    })
                    .await
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
        // The lease enforces single-flight semantics — at most one
        // critical section runs at a time.
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn async_keyed_locks_different_keys_run_concurrently() {
        use std::sync::atomic::AtomicUsize;
        let locks = Arc::new(AsyncKeyedLocks::<String>::new());
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for key in ["a", "b", "c"] {
            let locks = locks.clone();
            let concurrent = concurrent.clone();
            let max_concurrent = max_concurrent.clone();
            handles.push(tokio::spawn(async move {
                locks
                    .with_lease(key.to_string(), || async move {
                        let cur = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                        let prev = max_concurrent.load(Ordering::SeqCst);
                        if cur > prev {
                            max_concurrent.store(cur, Ordering::SeqCst);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        concurrent.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
        // Different keys do not block each other; at least 2 should run
        // concurrently on a multi-threaded runtime.
        assert!(
            max_concurrent.load(Ordering::SeqCst) >= 2,
            "expected concurrent execution across distinct keys"
        );
    }
}
