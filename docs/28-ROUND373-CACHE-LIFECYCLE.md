# R373 — Cache Lifecycle Helpers

## Goal

Port the 9 cache-management functions from Node
`acpx-engine/execute.ts`. These helpers manage two warm-handle maps
(`warmHandles`, `stagedRuntimes`) plus a per-session staging lease chain.
R373 closes the gap that R368 explicitly deferred.

## Module Added

`crates/pc-acpx/src/cache_lifecycle.rs` — pure-ish helpers (async for the
ones that touch `tokio::time` and `runtime.close`).

| Function | Mirrors |
|---|---|
| `warm_handle_matches` | `warmHandleMatches` |
| `clear_warm_handle_timer` | `clearWarmHandleTimer` |
| `close_warm_handle` | `closeWarmHandle` |
| `cleanup_idle_handles` | `cleanupIdleHandles` |
| `schedule_idle_handle_cleanup` | `scheduleIdleHandleCleanup` |
| `save_staged_runtime_after_clean_turn` | `saveStagedRuntimeAfterCleanTurn` |
| `discard_staged_runtime` | `discardStagedRuntime` |
| `cleanup_idle_staged_runtimes` | `cleanupIdleStagedRuntimes` |
| `with_session_staging_lease` | `withSessionStagingLease` |

Plus supporting types:

| Type | Notes |
|---|---|
| `RuntimeCacheEntry` | runtime + handle + fingerprint + last_used_at + cleanup_timer |
| `StagedRuntimeCacheEntry` | env_delta + teardown + dispose + last_used_at |
| `SessionStagingLocks` | `Arc`-shared per-key async lease chain |
| `SessionStagingLease<T>` | RAII handle returned by `with_session_staging_lease` |
| `AsyncCallback` | `Arc<dyn Fn() -> Pin<Box<dyn Future<Output=()> + Send>>>` |
| `TokioCleanupHandle` | `JoinHandle<()>` wrapper with `cancel()` + `Drop` abort |

`MockAcpRuntime` is now re-exported from `pc_acpx::*` so downstream tests
can spin up a fake `AcpRuntime` without coupling to the `acp_runtime`
module path.

## Design Notes

### Pure-vs-async split

The cache helpers split naturally into:
- Pure (no I/O): `warm_handle_matches`, `clear_warm_handle_timer`,
  `save_staged_runtime_after_clean_turn`.
- Async (touch `runtime.close` / `tokio::time`): `close_warm_handle`,
  `cleanup_idle_handles`, `schedule_idle_handle_cleanup`,
  `discard_staged_runtime`, `cleanup_idle_staged_runtimes`,
  `with_session_staging_lease`.

`with_session_staging_lease` is the most subtle — it chains per-key
leases by inserting a new `StagingGate` promise into the lock map and
waiting on the prior gate before running the caller's future. The
returned `SessionStagingLease` releases the gate via `await_release` (or
implicitly on drop via a fire-and-forget background task).

### Shared ownership for `schedule_idle_handle_cleanup`

The cleanup task outlives the call to `schedule_idle_handle_cleanup`,
so the `handles` map must be `Arc<TokioMutex<HashMap>>` rather than
`&mut HashMap`. The task then locks the map at fire time and removes
the entry only if the fingerprint + handle + `last_used_at` still
match — a concurrent reuse that bumped `last_used_at` will cancel
the cleanup.

### Decoupled from `PreparedRuntime`

The Node version reads `prepared.stagedRuntime`, `prepared.remoteStagingEnvDelta`,
`prepared.remoteManagedHomeTeardown`, `prepared.remoteStagingDispose`
directly off the `PreparedRuntime`. The Rust `PreparedRuntime` (R364)
does not yet carry those fields. R373 therefore takes each value as a
plain parameter, and R374's `buildRuntime` integration will wire the
prepared fields through.

### `AsyncCallback` instead of `Box<dyn FnOnce(...)>`

The Node teardown / dispose closures can fire multiple times (a
shared `teardown` re-fires on every turn's cleanup). Rust closures
that capture state need to be `Clone`-able for that pattern, so the
helpers take `Arc<...>` wrappers via the `AsyncCallback` newtype. This
preserves the multi-fire semantics without leaking ownership.

## Tests

`crates/pc-acpx/tests/round373_cache_lifecycle.rs` — 14 tests, all green:

| Group | Tests |
|---|---|
| `warm_handle_matches` (true / false / undefined) | 3 |
| `clear_warm_handle_timer` (noop / cancel) | 2 |
| `cleanup_idle_handles` (zero idle / stale eviction) | 2 |
| `close_warm_handle` (remove + close) | 1 |
| `schedule_idle_handle_cleanup` (zero idle / spawns timer) | 2 |
| `save_staged_runtime_after_clean_turn` (insert) | 1 |
| `discard_staged_runtime` (remove + fire dispose) | 1 |
| `with_session_staging_lease` (serialize) | 1 |
| `cleanup_idle_staged_runtimes` (drop + dispose) | 1 |

## Baseline

- `pc-acpx` lib + integration tests: **303 / 303 green** (R372 was 289; +14).
- `pc-heartbeat` integration tests: **928 / 928 green** (no regression).

## What's Next

R373 closes the cache-helper gap. Remaining acpx-engine work:

1. **R374 — buildRuntime / createAcpxEngineExecutor / execute** —
   top-level engine entry point. Composes
   `prepare_*_skill_runtime` + `SubprocessAcpRuntime` + cache helpers
   + prompt builder into the end-to-end `execute(ctx)` flow.
   Highest remaining ROI; ~300 lines, ~10 tests.

2. **R375+ — stageAcpRemoteRuntime** — runner-backed sandbox staging,
   across `paperclip/packages/sandbox-utils/`. Lowest priority; only
   matters when the remote-bridge path is active.

### Completion Estimate

| Area | Status |
|---|---|
| `pc-acpx` helpers + protocol + JSON-RPC wire | 100% |
| `pc-acpx` SubprocessAcpRuntime (all trait methods) | 100% |
| `pc-acpx` cache lifecycle | **100% (R373)** |
| `pc-acpx` buildRuntime top-level | 0% (R374) |
| `pc-acpx` stageAcpRemoteRuntime | 0% (R375+) |
| Recovery main chain (pc-heartbeat + pc-repos + pc-core) | ~96% |
| Full backend (adapters + plugins) | ~72% |
