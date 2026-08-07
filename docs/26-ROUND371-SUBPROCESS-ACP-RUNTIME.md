# R371 — SubprocessAcpRuntime (Control Surface)

## Goal

Replace `MockAcpRuntime` with the real `SubprocessAcpRuntime` that talks to
the `acpx` binary over the JSON-RPC wire introduced in R370. R371 covers the
**synchronous** control surface (`ensure_session`, `get_capabilities`,
`get_status`, `set_mode`, `set_config_option`, `cancel`, `close`,
`doctor`) and ships a placeholder `start_turn` so the trait contract is
satisfied end-to-end. R372 will replace the placeholder with a real
broadcast/event/result correlation pipeline.

## Module Added

`crates/pc-acpx/src/subprocess_acp_runtime.rs` — the real `AcpRuntime`
implementation.

| Method | Implementation |
|---|---|
| `ensure_session` | `request("session/new", params)` → `session_id`, `backend_session_id`, `agent_session_id` from response |
| `get_capabilities` | `request("session/capabilities", None)` → `AcpRuntimeCapabilities` |
| `get_status` | `request("session/status", params)` → `AcpRuntimeStatus` |
| `set_mode` | `request("session/set_mode", params)` → ok |
| `set_config_option` | `request("session/set_config_option", params)` → ok |
| `cancel` | Lazy `SubprocessHandle::cancel` (SIGKILL via `start_kill`) |
| `close` | `close_stdin` + `cancel` + drop the handle |
| `doctor` | Reports `pid > 0` while the subprocess is alive |
| `start_turn` | **R372 placeholder** — empty event stream, `Completed` result |

`lib.rs` exposes `SubprocessAcpRuntime` and `SubprocessAcpRuntimeSpec` via
`pub use` re-exports.

## Design Notes

### Lazy spawn
`ensure_subprocess` only spawns the `acpx` child on the first request that
needs it. This keeps the runtime cheap to construct (no I/O at `new`) and
lets callers wire it up before any side effects happen. The handle is
stored in `Arc<Mutex<RuntimeState>>` so multiple tasks can hold clones of
the handle without holding the runtime lock during I/O.

### Serialized requests (R371)
The `request` method writes one frame, reads one frame, and returns. This
relies on the JSON-RPC 2.0 contract that each request produces exactly one
response frame. Concurrent requests would interleave frames and break
correlation; R372 will lift this restriction via a background reader task
that demuxes notifications and responses into per-request oneshot senders
+ a broadcast channel.

### Error mapping
JSON-RPC `error` frames become `AcpxError::JsonRpcParse { line, reason }`
with the JSON-RPC `code` and `message` interpolated. R372 will introduce
a dedicated `AcpxError::RpcError { code, message }` variant for clearer
diagnostics; the current mapping is sufficient for R371's control surface.

### Idempotent close
`close` takes the inner handle out of `state.handle`, then closes stdin and
kills the child. Subsequent calls observe `state.handle == None` and return
`Ok(())` without touching the (now-reaped) process.

### Drop safety
`SubprocessHandle::spawn` sets `kill_on_drop(true)`, so even if a caller
never reaches `close`, the OS will reap the child when the last clone of
the handle drops. `SubprocessAcpRuntime::drop` is a no-op placeholder that
documents this invariant.

## Tests

`crates/pc-acpx/tests/round371_subprocess_acp_runtime.rs` — 10 tests, all
green. Each test spawns a tiny shell script that pretends to be the `acpx`
binary and emits canned JSON-RPC frames:

| Test | Verifies |
|---|---|
| `ensure_session_handshakes_with_session_new_response` | session/new handshake returns `backend_session_id` + `agent_session_id` |
| `ensure_session_propagates_session_new_error` | JSON-RPC error frame becomes `AcpRuntimeError::SessionError` |
| `get_capabilities_returns_advertised_controls` | Capabilities deserialize into `AcpRuntimeCapabilities` (snake_case enum tags) |
| `get_status_returns_session_handle_fields` | Status deserialize into `AcpRuntimeStatus` |
| `set_mode_succeeds_with_session_set_mode_response` | `session/set_mode` round-trip |
| `set_config_option_succeeds_with_ok_response` | `session/set_config_option` round-trip |
| `cancel_kills_long_running_child` | `cancel` reaches the child via `kill_on_drop` |
| `close_shuts_down_child` | `close` reaps the child |
| `doctor_reports_ok_when_process_is_alive` | `doctor` reports `ok: true` while child is alive |
| `start_turn_placeholder_returns_empty_event_stream` | R371 placeholder returns zero events |

## Baseline

- `pc-acpx` lib + integration tests: **284 / 284 green** (R370 was 272; +10).
- `pc-heartbeat` integration tests: **928 / 928 green** (no regression).

## What's Next

R371 closes the control-surface gap. The remaining `acpx-engine/execute.ts`
work that still requires porting:

1. **R372 — SubprocessAcpRuntime.start_turn** — Replace the placeholder with
   the real broadcast + oneshot correlation pipeline:
   - Continuous background reader task that demuxes stdout lines.
   - `tokio::sync::broadcast` channel for `AcpRuntimeEvent`s consumed by
     the active turn's event stream.
   - Per-request oneshot channel for the terminal result.
   - AcpxNotification → `AcpRuntimeEvent` mapping (`text_delta`,
     `tool_call`, `status`, `done`, `error`).
   - 8-10 tests: notification-to-event mapping, broadcast fan-out,
     result-await timeout, mid-turn cancellation.

2. **R373 — Cache lifecycle helpers** — The 7 functions R368 left out:
   `cleanupIdleHandles`, `scheduleIdleHandleCleanup`, `closeWarmHandle`,
   `discardStagedRuntime`, `withSessionStagingLease`,
   `saveStagedRuntimeAfterCleanTurn`, `warmHandleMatches`.

3. **R374 — buildRuntime / createAcpxEngineExecutor / execute** —
   top-level engine entry point. Composes `prepare_*_skill_runtime` +
   `SubprocessAcpRuntime` + cache helpers + prompt builder into the
   end-to-end `execute(ctx)` flow.

4. **R375+ — stageAcpRemoteRuntime** — runner-backed sandbox staging,
   across `paperclip/packages/sandbox-utils/`. Lowest priority.

### Completion Estimate

| Area | Status |
|---|---|
| `pc-acpx` helpers + protocol + JSON-RPC wire | ~99.5% |
| `pc-acpx` SubprocessAcpRuntime control surface | 100% (R371) |
| `pc-acpx` SubprocessAcpRuntime start_turn | 0% (R372) |
| `pc-acpx` cache lifecycle | 0% (R373) |
| `pc-acpx` buildRuntime top-level | 0% (R374) |
| Recovery main chain (pc-heartbeat + pc-repos + pc-core) | ~96% |
| Full backend (adapters + plugins) | ~72% |
