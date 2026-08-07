# R370 — JSON-RPC Wire & Subprocess Handle

## Goal

Lay the JSON-RPC and child-process foundations for the real
`SubprocessAcpRuntime` (R371). With these two modules in place, the
`acpx` binary can be spawned, written to, and read from using deterministic,
fully-tested primitives — independent of any protocol-level knowledge.

## Modules Added (2 new files in `crates/pc-acpx/src/`)

| Module | Functions / Types | Notes |
|---|---|---|
| `jsonrpc_wire.rs` | `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcNotification`, `JsonRpcErrorBody`, `JsonRpcFrame`, `JsonRpcIdAllocator`, `next_jsonrpc_id`, `encode_jsonrpc_request`, `encode_jsonrpc_response`, `encode_jsonrpc_error`, `encode_jsonrpc_notification`, `parse_jsonrpc_line`, `decode_jsonrpc_frame`, `jsonrpc_error_from_value`, `JSONRPC_VERSION` | Pure JSON-RPC 2.0 framing; threadsafe id allocator |
| `subprocess_handle.rs` | `SubprocessHandle`, `SpawnAcpxInput`, `SubprocessTermination` | Async tokio wrapper; spawn, write_request, read_response_line, close_stdin, cancel, wait, pid |

`error.rs` gained four new variants: `AcpxError::Spawn { command, error }`,
`AcpxError::AlreadyReaped { pid }`, `AcpxError::JsonRpcParse { line, reason }`,
`AcpxError::SubprocessIo { target, error }`, `AcpxError::ReadTimeout { timeout_ms }`.

`lib.rs` exposes the new types and helpers via `pub use` re-exports.

## Design Notes

### `jsonrpc_wire`
- Pure helpers — no I/O. Every encoding function returns a single line of
  valid JSON (no trailing newline) so the caller can safely split stdout on
  `\n` and dispatch each line.
- `JsonRpcFrame` is the discriminated union over the four shapes a line can
  take: `Request`, `Response`, `Error { id, error }`, `Notification`. The
  discriminator logic lives in `decode_jsonrpc_value`; `parse_jsonrpc_line`
  and `decode_jsonrpc_frame` are thin wrappers around it.
- `JsonRpcIdAllocator` wraps `AtomicU64` for monotonic id allocation from
  any task. `next_jsonrpc_id(&alloc)` is the ergonomic free-function form.
  Concurrency is verified by a dedicated test (`jsonrpc_id_allocator_handles_concurrency`).
- `JsonRpcRequest::params` and `JsonRpcResponse::result` are
  `skip_serializing_if = "Option::is_none"` so we can omit `params` cleanly.

### `subprocess_handle`
- Async via `tokio::process::Command` + `tokio::io`. `kill_on_drop(true)`
  ensures the child cannot outlive the handle if a future is cancelled.
- stdin/stdout/child are wrapped in `Arc<Mutex<Option<_>>>` so multiple
  tasks can hold clones of the handle and write requests / read responses
  concurrently. `close_stdin`, `wait`, and `cancel` take the lock and clear
  the slot — subsequent calls return `AcpxError::AlreadyReaped { pid }`.
- stderr is drained to /dev/null by a background task to prevent pipe
  back-pressure deadlocking the child. R371 will route stderr through the
  existing `child_stderr` helpers.
- `read_response_line(timeout)` uses `tokio::time::timeout` so a hung child
  cannot stall the runtime indefinitely. On timeout we surface
  `AcpxError::ReadTimeout { timeout_ms }`.

## Tests

`crates/pc-acpx/tests/round370_jsonrpc_wire.rs` — 19 tests, all green:

| Group | Tests |
|---|---|
| `JSONRPC_VERSION` | 1 |
| `JsonRpcIdAllocator` + thread-safety | 2 |
| Encode request / response / error / notification | 6 |
| `parse_jsonrpc_line` discriminator + rejection | 3 |
| `decode_jsonrpc_frame` round-trip | 1 |
| `jsonrpc_error_from_value` | 1 |
| `JsonRpcRequest/Response` serde | 1 |
| `SubprocessHandle` spawn / wait / cancel / stdin/stdout / missing binary | 5 |

## Baseline

- `pc-acpx` lib + integration tests: **272 / 272 green** (R369 was 253; +19).
- `pc-heartbeat` integration tests: **928 / 928 green** (no regression).

## What's Next

R370 closes the wire-and-child gap. The remaining work to ship a real
`SubprocessAcpRuntime` falls into:

1. **R371 — SubprocessAcpRuntime impl** — Layered on top of R370:
   `ensure_session` (spawn + `session/new` handshake), `start_turn`
   (correlate stdout responses to in-flight requests via
   `JsonRpcIdAllocator`, emit `AcpRuntimeEvent`s from JSON-RPC
   notifications), `get_capabilities`/`get_status`/`set_mode`/
   `set_config_option`/`cancel`/`close` JSON-RPC wrappers.
   Drop-in replacement for `MockAcpRuntime`.

2. **R372 — Cache lifecycle helpers** — The 7 functions R368 left out:
   `cleanupIdleHandles`, `scheduleIdleHandleCleanup`, `closeWarmHandle`,
   `discardStagedRuntime`, `withSessionStagingLease`,
   `saveStagedRuntimeAfterCleanTurn`, `warmHandleMatches`.

3. **R373 — buildRuntime / createAcpxEngineExecutor / execute** —
   top-level engine entry point that wires every helper together.
   Only feasible once R371 lands.

### Completion Estimate

| Area | Status |
|---|---|
| `pc-acpx` helpers + protocol + JSON-RPC wire | ~99.5% (R370 closed the wire) |
| `pc-acpx` real runtime (`SubprocessAcpRuntime`) | 0% — wire + handle ready (R371) |
| `pc-acpx` cache lifecycle | 0% (R372) |
| `pc-acpx` buildRuntime top-level | 0% (R373) |
| Recovery main chain (pc-heartbeat + pc-repos + pc-core) | ~96% |
| Full backend (adapters + plugins) | ~72% |
