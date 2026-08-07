# R375 — `AcpxEngineExecutor` 工厂

## Goal

Port the top-level factory + entry-point from Node `createAcpxEngineExecutor`
(`acpx-engine/execute.ts` line 2920). R375 lands the **factory plumbing** —
the executor struct, the warm-handle / staged-runtime caches, the
runtime factory hook, and the cold-start / warm-hit decision in
`ensure_session`. Turn execution, prompt construction, and result
shaping are still in the Node side; they land in later rounds once
the per-concern helpers exist.

## Module Added

`crates/pc-acpx/src/acpx_engine_executor.rs` — factory + executor.

| Symbol | Mirrors |
|---|---|
| `AcpxEngineExecutorState` | `warmHandles` / `stagedRuntimes` / `stagingLocks` maps |
| `AcpxEngineExecutorDeps` | `AcpxEngineExecutorOptions` (subset) |
| `AcpxRuntimeFactory` | `createRuntime` factory hook |
| `AcpxEngineExecutor` | `createAcpxEngineExecutor` closure |
| `AcpxEngineExecutor::build` | `await buildRuntime(...)` |
| `AcpxEngineExecutor::ensure_session` | warm-hit / cold-start decision + `runtime.ensureSession(...)` |
| `AcpxEngineExecutor::evict_idle_staged_runtimes` | `cleanupIdleStagedRuntimes(...)` |
| `AcpxEngineExecutor::evict_idle_warm_handles` | `cleanupIdleHandles(...)` |
| `AcpxEngineExecutor::drop_warm_handle` | `clearWarmHandleTimer` + entry removal |
| `EnsureOutcome` | `runtime` + `handle` + `warm_hit` |

## Factory Flow

```
AcpxEngineExecutorDeps (optional now / warm_handle_idle_ms / runtime_factory / state_factory)
   │
   ▼
AcpxEngineExecutor::new(deps)
   │
   ├─ state_factory() → AcpxEngineExecutorState { warm_handles, staged_runtimes, staging_locks, idle_ms }
   ├─ now: Arc<dyn Fn() -> i64>
   └─ runtime_factory: Arc<dyn Fn(&PreparedRuntime) -> Result<Arc<dyn AcpRuntime>, AcpxError>>

.execute(input: BuildRuntimeInput)                  [R376+ — wired in next round]
   ├─ evict_idle_staged_runtimes()                  [mirrors Node cleanupIdleStagedRuntimes call]
   ├─ build(input) → PreparedRuntime                [pure assembly]
   ├─ evict_idle_warm_handles()                     [mirrors Node cleanupIdleHandles call]
   ├─ ensure_session(prepared, resume_session_id)   [warm-hit / cold-start decision]
   ├─ start_turn(...)                               [R376+ — wires SubprocessAcpRuntime start_turn]
   └─ close(...)                                    [R376+ — warm-handle cleanup]
```

## `EnsureOutcome`

```rust
pub struct EnsureOutcome {
    pub runtime: Arc<dyn AcpRuntime>,        // warm: cached, cold: factory-built
    pub handle: AcpRuntimeHandle,            // warm: cached, cold: from ensure_session
    pub warm_hit: bool,                       // true on warm path
}
```

A manual `Debug` impl (the `Arc<dyn AcpRuntime>` is not `Debug`-able by
default), `Clone` is derived so the outcome can flow through the
per-turn protocol without an extra reference layer.

## `AsyncCallback` / `StagedRuntimeCacheEntry` / `SessionStagingLocks`

These types in `cache_lifecycle.rs` previously were missing `Clone` or
`Debug`. R375 added `#[derive(Debug, Clone)]` to
`StagedRuntimeCacheEntry` (so it can clone through the
`cached_staged_runtime` getter), and `#[derive(Debug, Default)]` to
`SessionStagingLocks` (so the executor state can derive `Debug`).
`StagingGate` and `StagingGateState` also gained `#[derive(Debug)]`.

`cleanup_idle_staged_runtimes` lost its unused `_locks: &mut
SessionStagingLocks` parameter — the function never read it, and
`SessionStagingLocks` is internally async-locked so callers don't need
to pass it explicitly.

## Tests

| File | Tests |
|---|---|
| `crates/pc-acpx/src/acpx_engine_executor.rs::tests` | 12 (new) |
| `crates/pc-acpx/tests/round375_executor_factory.rs` | 19 (new) |

Total: **31 new tests**, all green.

| pc-acpx | Before R375 | After R375 |
|---|---|---|
| Total tests | 357 | **388** |
| Modules | 32 | **33** (+ acpx_engine_executor) |
| Public types / re-exports | — | +5 (`AcpxEngineExecutor`, `AcpxEngineExecutorDeps`, `AcpxEngineExecutorState`, `AcpxRuntimeFactory`, `EnsureOutcome`, `NowFn`, `system_now_ms`) |

## Coverage of Node `createAcpxEngineExecutor`

| Concern | Node | Rust (R375) |
|---|---|---|
| Executor state (warm handles / staged / locks) | ✅ | ✅ |
| Runtime factory hook | ✅ | ✅ (`AcpxRuntimeFactory`) |
| Clock injection | ✅ | ✅ (`NowFn` + `system_now_ms`) |
| State factory injection | ✅ | ✅ (`AcpxExecutorStateFactory`) |
| `await cleanupIdleStagedRuntimes(...)` | ✅ | ✅ (`evict_idle_staged_runtimes`) |
| `await cleanupIdleHandles(...)` | ✅ | ✅ (`evict_idle_warm_handles`) |
| `await buildRuntime(...)` | ✅ | ✅ (`build`) |
| `runtime.ensureSession(...)` + cache write | ✅ | ✅ (`ensure_session` cold path) |
| Warm-handle hit / cache reuse | ✅ | ✅ (`ensure_session` warm path) |
| `AcpxError::Spawn` when no factory | (throws) | ✅ |
| `AcpxError::SubprocessIo` on handshake failure | (throws) | ✅ |
| Resume via `resumeSessionId` | ✅ | ✅ (passed through; warm-hit ignores) |
| `createRuntimeMs` round-trip | ✅ | ❌ deferred (R376+) |
| `ensureSessionMs` round-trip | ✅ | ❌ deferred (R376+) |
| `acp.handshake` step metrics | ✅ | ❌ deferred (R376+) |
| Billing identity resolution | ✅ | ❌ deferred (R376+) |
| Span / tracer wiring | ✅ | ❌ deferred (R376+) |
| `start_turn` streaming | ✅ | ❌ deferred (R376+) |
| Result shaping (referencedProjectStagingFailures, etc.) | ✅ | ❌ deferred (R376+) |
| `acpx.handshake` retry-on-resume | ✅ | ❌ deferred (R376+) |

## Summary

R375 lifts the executor factory into its own module, wiring the
already-built cache helpers (R368 / R373) and `SubprocessAcpRuntime`
(R371 / R372) into a single `AcpxEngineExecutor` object. The factory
plumbing is now in place; the missing pieces (`start_turn`, billing,
result shaping, span wiring) all belong to the top-level
`execute(ctx)` orchestration that lands in R376+.

The executor is **deterministically testable**: the runtime factory is
injected, the state is injected, and the clock is injectable. R375's
integration tests prove the warm-hit / cold-start / eviction flows
without spawning a real subprocess.
