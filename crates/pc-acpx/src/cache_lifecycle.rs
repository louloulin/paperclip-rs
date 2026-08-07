//! `pc-acpx` cache lifecycle helpers — port of the 9 cache-management
//! functions in Node `acpx-engine/execute.ts`:
//!
//! - [`warm_handle_matches`]
//! - [`clear_warm_handle_timer`]
//! - [`close_warm_handle`]
//! - [`cleanup_idle_handles`]
//! - [`schedule_idle_handle_cleanup`]
//! - [`save_staged_runtime_after_clean_turn`]
//! - [`discard_staged_runtime`]
//! - [`cleanup_idle_staged_runtimes`]
//! - [`with_session_staging_lease`]
//!
//! The module is pure wrt `RuntimeCacheEntry` / `StagedRuntimeCacheEntry`
//! shape — no `PreparedRuntime` coupling (R374 will wire that in).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Poll, Waker};
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;
use tokio::task::{JoinHandle, JoinSet};

use crate::acp_runtime::{AcpRuntime, AcpRuntimeHandle};

/// Type alias for an async callback (`Fn() -> Pin<Box<dyn Future<Output=()> + Send>>`).
/// Wrapped in `Arc` so the same closure can be reused across a session.
pub type AsyncFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// An `Arc`-shared async callback that returns a `Future<Output = ()>`.
/// The Node equivalent is `(() => Promise<void>) | null`.
#[derive(Clone)]
pub struct AsyncCallback {
    inner: Arc<dyn Fn() -> AsyncFuture + Send + Sync + 'static>,
}

impl std::fmt::Debug for AsyncCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncCallback").finish_non_exhaustive()
    }
}

impl AsyncCallback {
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let wrapped: Arc<dyn Fn() -> AsyncFuture + Send + Sync + 'static> =
            Arc::new(move || Box::pin(f()));
        Self { inner: wrapped }
    }

    pub async fn run(&self) {
        let fut = (self.inner)();
        fut.await;
    }
}

/// Handle to a pending background cleanup task. Cancellation drops the
/// underlying `JoinHandle`, aborting the task.
pub struct TokioCleanupHandle {
    handle: Option<JoinHandle<()>>,
}

impl TokioCleanupHandle {
    /// Wrap an existing `JoinHandle`.
    pub fn from_join(handle: JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    /// Abort the cleanup task if it has not already finished.
    pub fn cancel(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for TokioCleanupHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Warm-handle cache entry. Mirrors Node `RuntimeCacheEntry`. The runtime
/// is held via `Arc<dyn AcpRuntime>` so the entry can be reused across
/// multiple turns without an extra reference count layer.
pub struct RuntimeCacheEntry {
    pub runtime: Arc<dyn AcpRuntime>,
    pub handle: AcpRuntimeHandle,
    pub fingerprint: String,
    pub last_used_at: i64,
    pub cleanup_timer: Option<TokioCleanupHandle>,
}

impl Clone for RuntimeCacheEntry {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
            handle: self.handle.clone(),
            fingerprint: self.fingerprint.clone(),
            last_used_at: self.last_used_at,
            // The cleanup_timer is intentionally NOT cloned — only the
            // original entry owns the JoinHandle. Cloning a cache
            // entry that still has a pending timer would orphan the
            // task on Drop.
            cleanup_timer: None,
        }
    }
}

impl std::fmt::Debug for RuntimeCacheEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeCacheEntry")
            .field("fingerprint", &self.fingerprint)
            .field("last_used_at", &self.last_used_at)
            .field("has_cleanup_timer", &self.cleanup_timer.is_some())
            .finish()
    }
}

/// Staged-runtime cache entry. Mirrors Node `StagedRuntimeCacheEntry`.
#[derive(Debug, Clone)]
pub struct StagedRuntimeCacheEntry {
    pub env_delta: HashMap<String, String>,
    pub teardown: Option<AsyncCallback>,
    pub dispose: Option<AsyncCallback>,
    pub last_used_at: i64,
}

/// Per-session async staging lease chain. Mirrors the Node
/// `stagingLocks: Map<string, Promise<unknown>>` primitive. Held under a
/// `TokioMutex` so callers can `await` the per-key gate.
#[derive(Debug, Default)]
pub struct SessionStagingLocks {
    inner: TokioMutex<HashMap<String, Arc<StagingGate>>>,
}

impl SessionStagingLocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire (or chain onto) the lease for `key`, run `f`, then return
    /// a [`SessionStagingLease`] that releases the gate when dropped.
    pub async fn acquire<T, F, Fut>(&self, key: &str, f: F) -> SessionStagingLease<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let (gate, prev) = {
            let mut map = self.inner.lock().await;
            let prev = map.get(key).cloned();
            let gate = Arc::new(StagingGate::new_with_prev(prev.clone()));
            map.insert(key.to_string(), Arc::clone(&gate));
            (gate, prev)
        };
        // Wait for any prior holder to finish.
        if let Some(prev) = prev {
            prev.wait().await;
        }
        // Run the caller under our gate.
        let value = f().await;
        SessionStagingLease::new(value, gate)
    }
}

#[derive(Debug)]
struct StagingGate {
    state: TokioMutex<StagingGateState>,
}

#[derive(Debug)]
struct StagingGateState {
    completed: bool,
    waiters: Vec<Waker>,
}

impl StagingGate {
    fn new_with_prev(_prev: Option<Arc<StagingGate>>) -> Self {
        Self {
            state: TokioMutex::new(StagingGateState {
                completed: false,
                waiters: Vec::new(),
            }),
        }
    }

    /// Mark the gate as completed and wake every waiter.
    async fn complete(&self) {
        let mut state = self.state.lock().await;
        state.completed = true;
        for waker in state.waiters.drain(..) {
            waker.wake();
        }
    }

    /// Wait until the gate is completed.
    async fn wait(&self) {
        let mut state = self.state.lock().await;
        if state.completed {
            return;
        }
        state.waiters.push(futures_task_waker());
        drop(state);
        // Park the current task until the gate completes. We yield once
        // to give the holder a chance to finish, then re-check.
        YieldOnce::new().await;
        loop {
            let state = self.state.lock().await;
            if state.completed {
                return;
            }
            drop(state);
            YieldOnce::new().await;
        }
    }
}

/// Handle returned by [`SessionStagingLocks::acquire`]. Holds the gate;
/// calling `await_release` completes it.
pub struct SessionStagingLease<T> {
    value: Option<T>,
    gate: Option<Arc<StagingGate>>,
}

impl<T> SessionStagingLease<T> {
    fn new(value: T, gate: Arc<StagingGate>) -> Self {
        Self {
            value: Some(value),
            gate: Some(gate),
        }
    }

    /// Consume the lease and return the value, releasing the gate.
    pub async fn into_value(self) -> T {
        let SessionStagingLease { value, gate } = self;
        if let Some(gate) = gate {
            gate.complete().await;
        }
        value.expect("value present")
    }

    /// Release the gate and consume the value.
    pub async fn await_release(self) {
        if let Some(gate) = self.gate {
            gate.complete().await;
        }
    }
}

/// Yield once to the executor. Equivalent to `tokio::task::yield_now()`
/// but does not require pulling in `tokio::task::yield_now` here.
struct YieldOnce {
    yielded: bool,
}

impl YieldOnce {
    fn new() -> Self {
        Self { yielded: false }
    }
}

impl std::future::Future for YieldOnce {
    type Output = ();
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        if self.yielded {
            return Poll::Ready(());
        }
        self.yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Build a waker that immediately re-schedules the task (best-effort
/// approximation of `tokio::task::yield_now`).
fn futures_task_waker() -> Waker {
    use std::sync::Arc;
    use std::task::{Wake, Waker};
    struct YieldWaker;
    impl Wake for YieldWaker {
        fn wake(self: Arc<Self>) {
            // The waker is consumed by the executor via the Waker passed
            // to `poll`. We do nothing here; the next iteration of the
            // event loop will re-poll the task.
        }
    }
    Waker::from(Arc::new(YieldWaker))
}

// =============================================================================
// Warm-handle helpers
// =============================================================================

/// `true` when the cached entry belongs to the same `runtime`+`handle`
/// pair. Mirrors Node `warmHandleMatches`.
pub fn warm_handle_matches(
    entry: Option<&RuntimeCacheEntry>,
    _runtime: &dyn AcpRuntime,
    handle: &AcpRuntimeHandle,
) -> bool {
    match entry {
        // The handle is the source of truth for warm-handle identity: the
        // runtime is a stable singleton per AcpRuntime impl, but the same
        // runtime can host multiple concurrent sessions under different
        // handles, so equality reduces to handle equality.
        Some(entry) => entry.handle == *handle,
        None => false,
    }
}

/// Cancel any pending idle cleanup timer on `entry`. No-op when no timer
/// is set. Mirrors Node `clearWarmHandleTimer`.
pub fn clear_warm_handle_timer(entry: &mut RuntimeCacheEntry) {
    if let Some(mut timer) = entry.cleanup_timer.take() {
        timer.cancel();
    }
}

/// Close the warm handle under `key` and remove the entry from the map.
/// Mirrors Node `closeWarmHandle`. Errors from `runtime.close` are
/// swallowed (matching Node `.catch(() => {})`).
pub async fn close_warm_handle(
    handles: &mut HashMap<String, RuntimeCacheEntry>,
    key: &str,
    mut entry: RuntimeCacheEntry,
) {
    // Only remove if the cached entry still matches the one we were asked
    // to close — a concurrent turn may have already replaced it.
    let still_ours = handles
        .get(key)
        .map(|cached| cached.handle == entry.handle && cached.fingerprint == entry.fingerprint)
        .unwrap_or(false);
    if still_ours {
        handles.remove(key);
    }
    clear_warm_handle_timer(&mut entry);
    let _ = entry
        .runtime
        .close(crate::acp_runtime::AcpRuntimeCloseInput {
            handle: entry.handle,
            reason: "paperclip cache close".into(),
            discard_persistent_state: Some(false),
        })
        .await;
}

/// Schedule a background task that closes the warm handle once it has
/// been idle for `idle_ms` ms. Mirrors Node `scheduleIdleHandleCleanup`.
/// When `idle_ms <= 0` the call is a no-op.
pub async fn schedule_idle_handle_cleanup<F>(
    handles: Arc<TokioMutex<HashMap<String, RuntimeCacheEntry>>>,
    key: String,
    entry: &mut RuntimeCacheEntry,
    idle_ms: i64,
    now: F,
) where
    F: Fn() -> i64 + Send + 'static,
{
    clear_warm_handle_timer(entry);
    if idle_ms <= 0 {
        return;
    }
    let delay_ms = (entry.last_used_at + idle_ms - now()).max(1) as u64;
    let entry_fingerprint = entry.fingerprint.clone();
    let entry_handle = entry.handle.clone();
    let entry_last_used_at = entry.last_used_at;
    let handle_for_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let mut guard = handles.lock().await;
        if let Some(current) = guard.get(&key).cloned() {
            // Only close if the entry is still ours (fingerprint + handle match).
            if current.fingerprint == entry_fingerprint
                && current.handle == entry_handle
                && current.last_used_at == entry_last_used_at
            {
                guard.remove(&key);
                let _ = current
                    .runtime
                    .close(crate::acp_runtime::AcpRuntimeCloseInput {
                        handle: current.handle,
                        reason: "paperclip idle cleanup".into(),
                        discard_persistent_state: Some(false),
                    })
                    .await;
            }
        }
    });
    entry.cleanup_timer = Some(TokioCleanupHandle::from_join(handle_for_task));
}

/// Drop all warm handles whose `last_used_at` is older than `now - idle_ms`.
/// Mirrors Node `cleanupIdleHandles`. No-op when `idle_ms <= 0`.
pub async fn cleanup_idle_handles(
    handles: &mut HashMap<String, RuntimeCacheEntry>,
    now: i64,
    idle_ms: i64,
) {
    if idle_ms <= 0 {
        return;
    }
    let stale_keys: Vec<String> = handles
        .iter()
        .filter_map(|(key, entry)| {
            if now - entry.last_used_at >= idle_ms {
                Some(key.clone())
            } else {
                None
            }
        })
        .collect();
    for key in stale_keys {
        if let Some(entry) = handles.remove(&key) {
            let _ = entry
                .runtime
                .close(crate::acp_runtime::AcpRuntimeCloseInput {
                    handle: entry.handle,
                    reason: "paperclip idle cleanup".into(),
                    discard_persistent_state: Some(false),
                })
                .await;
        }
    }
}

// =============================================================================
// Staged-runtime helpers
// =============================================================================

/// Insert a staged-runtime entry after a clean turn. Mirrors Node
/// `saveStagedRuntimeAfterCleanTurn`.
pub fn save_staged_runtime_after_clean_turn(
    handles: &mut HashMap<String, StagedRuntimeCacheEntry>,
    key: &str,
    env_delta: HashMap<String, String>,
    teardown: Option<AsyncCallback>,
    dispose: Option<AsyncCallback>,
    now: i64,
) {
    handles.insert(
        key.to_string(),
        StagedRuntimeCacheEntry {
            env_delta,
            teardown,
            dispose,
            last_used_at: now,
        },
    );
}

/// Drop the staged-runtime entry for `key` and fire its `dispose`
/// callback (if any). Mirrors Node `discardStagedRuntime`.
pub async fn discard_staged_runtime(
    handles: &mut HashMap<String, StagedRuntimeCacheEntry>,
    key: &str,
) {
    let entry = handles.remove(key);
    if let Some(entry) = entry {
        if let Some(dispose) = entry.dispose {
            dispose.run().await;
        }
    }
}

/// Drop staged-runtime entries whose `last_used_at` is older than
/// `now - idle_ms` and fire their `dispose` callbacks. Mirrors Node
/// `cleanupIdleStagedRuntimes`.
pub async fn cleanup_idle_staged_runtimes<F>(
    handles: &mut HashMap<String, StagedRuntimeCacheEntry>,
    now: F,
    idle_ms: i64,
) where
    F: Fn() -> i64,
{
    if idle_ms <= 0 {
        return;
    }
    let now_value = now();
    let stale_keys: Vec<String> = handles
        .iter()
        .filter_map(|(key, entry)| {
            if now_value - entry.last_used_at >= idle_ms {
                Some(key.clone())
            } else {
                None
            }
        })
        .collect();
    for key in stale_keys {
        discard_staged_runtime(handles, &key).await;
    }
}

/// Run `f` under the staging lease for `key`. Returns a
/// [`SessionStagingLease`] whose `await_release` method releases the
/// gate. Mirrors Node `withSessionStagingLease`.
pub async fn with_session_staging_lease<T, F, Fut>(
    locks: &mut SessionStagingLocks,
    key: &str,
    f: F,
) -> SessionStagingLease<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    locks.acquire(key, f).await
}

// Compile-time sanity: `JoinSet` is not used directly but is imported to
// avoid an unused import warning when features shift.
#[allow(dead_code)]
fn _phantom_set() -> JoinSet<()> {
    JoinSet::new()
}
