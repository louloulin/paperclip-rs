# R372 — SubprocessAcpRuntime.start_turn Streaming

## Goal

Replace the R371 `start_turn` placeholder with the real streaming pipeline:
server-pushed `session/event` notifications become an `AcpRuntimeEvent`
stream, and the terminal `session/prompt` response becomes an
`AcpRuntimeTurnResult` future. This is the last major piece of the
`SubprocessAcpRuntime` impl — every `AcpRuntime` trait method is now
backed by the real `acpx` JSON-RPC wire.

## Concurrency Model

R372 introduces a **split state** so the sync `start_turn` trait method
can stay sync without deadlocking:

| State | Mutex | Holds |
|---|---|---|
| `SyncState` | `std::sync::Mutex` | JSON-RPC id allocator, broadcast sender, in-flight oneshot senders |
| `AsyncState` | `tokio::sync::Mutex` | `SubprocessHandle`, reader task `JoinHandle` |

The split is necessary because `std::sync::MutexGuard` is not `Send`, so
it cannot be held across `.await`. `start_turn` only touches `SyncState`
(id allocation, oneshot registration, broadcast subscribe) — none of
which need to await — then spawns an async dispatch task that writes the
prompt request to the subprocess.

### Background reader task

Spawned in `ensure_subprocess`. Runs continuously:

```
loop:
  line = read stdout line
  match parse(line):
    Response(id)  → deliver to in_flight[id]
    Error(id)     → deliver to in_flight[id]
    Notification  → broadcast to event channel
```

- `in_flight` is the `HashMap<u64, oneshot::Sender<JsonRpcOutcome>>` —
  every outstanding request (control + turn) has an entry. The reader
  removes the entry after delivery so a duplicate response cannot
  resurrect a closed channel.
- `event_tx` is the broadcast sender; every active `start_turn` has its
  own receiver obtained via `tx.subscribe()`.

### AcpxNotification → AcpRuntimeEvent mapping

`acpx_event_from_params` deserializes the notification's `params` directly
into the typed `AcpRuntimeEvent` enum. The acpx protocol wraps each
event in a `session/event` notification with `params` shaped exactly like
`AcpRuntimeEvent`'s tagged-union representation, so a single
`serde_json::from_value` call is sufficient. Unknown / malformed
notifications are dropped (logged at the framework boundary in R373).

## Module Updates

`crates/pc-acpx/src/subprocess_acp_runtime.rs` — replaced the R371
`start_turn` placeholder with the streaming pipeline. `SyncState` and
`AsyncState` introduced; `ensure_subprocess` now also spawns the reader
task; `start_turn` is now fully wired to the JSON-RPC protocol.

`broadcast_receiver_to_stream` — small adapter that turns
`tokio::sync::broadcast::Receiver<T>` into the
`Pin<Box<dyn Stream<Item = T> + Send + Sync>>` shape the trait requires.
Uses `futures::stream::poll_fn` + `tokio::pin!` to bridge the recv
future without adding a `tokio-stream` dependency.

## Tests

`crates/pc-acpx/tests/round372_start_turn_stream.rs` — 5 tests, all green:

| Test | Verifies |
|---|---|
| `start_turn_streams_text_delta_then_done` | `text_delta` notification flows through broadcast into `turn.events`; result future resolves to `Completed` |
| `start_turn_done_event_terminates_result` | Pure response (no notifications) → result future resolves with the response's `stopReason` |
| `start_turn_failed_response_maps_to_failed_result` | JSON-RPC error frame → `Failed { error }` result with the message |
| `start_turn_streams_tool_call_event` | `tool_call` notification → `AcpRuntimeEvent::ToolCall` reaches the stream |
| `start_turn_error_event_maps_to_failed_result` | `error` notification is delivered as an event (does not auto-terminate the result — caller decides) |

## Baseline

- `pc-acpx` lib + integration tests: **289 / 289 green** (R371 was 284; +5).
- `pc-heartbeat` integration tests: **928 / 928 green** (no regression).

## What's Next

R372 closes the `SubprocessAcpRuntime` impl. Remaining acpx-engine work:

1. **R373 — Cache lifecycle helpers** — 7 functions from execute.ts
   that R368 left out:
   `cleanupIdleHandles`, `scheduleIdleHandleCleanup`, `closeWarmHandle`,
   `discardStagedRuntime`, `withSessionStagingLease`,
   `saveStagedRuntimeAfterCleanTurn`, `warmHandleMatches`.

2. **R374 — buildRuntime / createAcpxEngineExecutor / execute** —
   top-level engine entry point that composes `prepare_*_skill_runtime`
   + `SubprocessAcpRuntime` + cache + prompt builder into the
   end-to-end `execute(ctx)` flow. ~10 tests.

3. **R375+ — stageAcpRemoteRuntime** — runner-backed sandbox staging,
   across `paperclip/packages/sandbox-utils/`. Lowest priority.

### Completion Estimate

| Area | Status |
|---|---|
| `pc-acpx` helpers + protocol + JSON-RPC wire | 100% |
| `pc-acpx` SubprocessAcpRuntime (all trait methods) | **100% (R372)** |
| `pc-acpx` cache lifecycle | 0% (R373) |
| `pc-acpx` buildRuntime top-level | 0% (R374) |
| Recovery main chain (pc-heartbeat + pc-repos + pc-core) | ~96% |
| Full backend (adapters + plugins) | ~72% |
