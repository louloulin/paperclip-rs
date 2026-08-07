# R376 — `AcpxEngineExecutor::execute(ctx)` 顶层入口

## Goal

Port the `executeAcpxEngine` closure returned by
`createAcpxEngineExecutor` from Node `acpx-engine/execute.ts`
(line 2928). R376 lands the full **control flow**:
evict → build → ensure_session → start_turn → collect events →
terminal decision → warm-handle retention/drop → `AdapterExecutionResult`.

Bridge bring-up, billing identity, prompt construction, sandbox
staging seam, run-result shaping (usage / cost / session params)
remain in later rounds (R377+).

## Module Update

`crates/pc-acpx/src/acpx_engine_executor.rs` — adds the `execute(ctx)`
method + 4 new public types:

| Symbol | Mirrors |
|---|---|
| `AdapterExecutionContext` | Node `AdapterExecutionContext` (subset) |
| `AdapterExecutionSink` | `ctx.onLog` + `ctx.onEvent` (trait) |
| `ExecutorLogStream` | `"stdout" \| "stderr"` literal |
| `NoopSink` | sink that drops every log/event |
| `AdapterExecutionResult` | Node `AdapterExecutionResult` (subset) |
| `AcpxEngineExecutor::execute` | Node `executeAcpxEngine` |

## Control Flow

```
AdapterExecutionContext { run_id, agent, config, context, run_prompt, cwd, ... }
   │
   ▼
AcpxEngineExecutor::execute(ctx)
   │
   ├─ evict_idle_staged_runtimes()                 [R375]
   ├─ to_build_runtime_input()                     [R374 input adapter]
   ├─ build(input) → PreparedRuntime               [R374 pure assembly]
   ├─ sink.on_log(stderr, timeout_resolution_line)  [diagnostic]
   ├─ evict_idle_warm_handles()                    [R375]
   ├─ ensure_session(prepared, None) → EnsureOutcome  [R375]
   ├─ build AcpRuntimeTurnInput (handle, request_id, text, mode, timeout_ms)
   ├─ runtime.start_turn(input) → AcpRuntimeTurn   [R371/R372]
   ├─ for each event: accumulate text_delta + sink.on_event()
   ├─ await turn.result.future → AcpRuntimeTurnResult
   ├─ warm-handle retention decision:
   │    - Completed + Persistent  → refresh last_used_at
   │    - Completed + OneShot     → close + drop
   │    - Failed / Cancelled      → close + drop
   ├─ emit result.sink log/error
   └─ return AdapterExecutionResult { exit_code, status, summary, ... }
```

## AdapterExecutionSink

```rust
#[async_trait]
pub trait AdapterExecutionSink: Send + Sync {
    async fn on_log(&self, stream: ExecutorLogStream, chunk: String);
    async fn on_event(&self, event: serde_json::Value);
}
```

Tests provide a `RecordingSink` that captures every call. Production
wires this against the Node `ctx.onLog` / `ctx.onEvent` callbacks.

## AdapterExecutionResult

| Field | Mirrors Node |
|---|---|
| `exit_code: i32` | `exitCode: 0 / 1` |
| `timed_out: bool` | `timedOut: boolean` |
| `error_message: Option<String>` | `errorMessage: string \| null` |
| `error_code: Option<String>` | `errorCode: "acpx_turn_failed" \| "acpx_timeout" \| null` |
| `session_id: Option<String>` | `sessionId: backend_session_id ?? runtime_session_name` |
| `session_display_id: Option<String>` | `sessionDisplayId: agent ?? backend ?? runtime` |
| `summary: String` | `summary: textParts.join("").trim()` |
| `stop_reason: Option<String>` | `terminalStopReason` |
| `status: String` | `terminal.status` (`"completed" \| "failed" \| "cancelled"`) |

## Tests

| File | Tests |
|---|---|
| `crates/pc-acpx/tests/round376_executor_entry_point.rs` | 16 (new) |

Total: **16 new tests**, all green.

| pc-acpx | Before R376 | After R376 |
|---|---|---|
| Total tests | 388 | **404** |
| Modules | 33 | 33 (no new module) |
| Public types / re-exports | — | +7 (`AdapterExecutionContext`, `AdapterExecutionSink`, `AdapterExecutionResult`, `ExecutorLogStream`, `NoopSink`) |

## Coverage of Node `executeAcpxEngine`

| Concern | Node | Rust (R376) |
|---|---|---|
| `cleanupIdleStagedRuntimes(...)` | ✅ | ✅ |
| `await buildRuntime(...)` | ✅ | ✅ |
| `ctx.onLog(timeout resolution line)` | ✅ | ✅ |
| `cleanupIdleHandles(...)` | ✅ | ✅ |
| `runtime.ensureSession(...)` + warm-hit | ✅ | ✅ |
| `runtime.startTurn(...)` + event stream | ✅ | ✅ |
| `turn.result` await | ✅ | ✅ |
| Warm-handle retention on `completed + persistent` | ✅ | ✅ |
| Warm-handle close + drop on `failed / cancelled` | ✅ | ✅ |
| Warm-handle drop on `completed + oneshot` | ✅ | ✅ |
| `summary: textParts.join("").trim()` | ✅ | ✅ |
| `terminalStopReason` extraction | ✅ | ✅ |
| `sessionId` / `sessionDisplayId` precedence | ✅ | ✅ |
| `exitCode` / `timedOut` / `errorCode` mapping | ✅ | ✅ |
| `AcpxError::Spawn` when no factory | ✅ | ✅ |
| `referenceBillingIdentity` resolution | ✅ | ❌ deferred (R377+) |
| `runtimeOptions` construction (cwd / spawnCwd / agentRegistry / mcpServers / verbose / onAgentStderr / onAgentSpawn) | ✅ | ❌ deferred (R377+) |
| `acp.handshake` step metrics | ✅ | ❌ deferred (R377+) |
| `startAdapterExecutionTargetPaperclipBridge` / `startAdapterExecutionTargetProcessSessionBridge` | ✅ | ❌ deferred (R377+) |
| `createRuntimeMs` / `ensureSessionMs` | ✅ | ❌ deferred (R377+) |
| `runAcpxTurn` (prompt + options + timeout controller) | ✅ | ❌ deferred (R377+) |
| `summarizeAcpxTurnUsage(...)` | ✅ | ❌ deferred (R377+) |
| `cleanupRemoteBridges` | ✅ | ❌ deferred (R377+) |
| `buildSessionParams` + run-result shaping | ✅ | ❌ deferred (R377+) |
| Span / tracer (`spanParent` + `acpx.startup` root) | ✅ | ❌ deferred (R377+) |
| Flush child stderr | ✅ | ❌ deferred (R377+) |
| `clearSession` decision | ✅ | ❌ deferred (R377+) |
| Resume-on-retry / `acpx.handshake` retry | ✅ | ❌ deferred (R377+) |

## Summary

R376 lands the full executor control flow with a real, end-to-end
`execute(ctx)` method that:

1. Runs the entire prepared-runtime assembly (`evict → build →
   ensure_session`) inside the executor.
2. Drives the `AcpRuntime::start_turn` lifecycle (event collection +
   terminal await).
3. Decides warm-handle retention based on terminal status + session
   mode.
4. Returns a structured `AdapterExecutionResult` with stable
   session-handle id and per-run summary.

The remaining concerns (bridge bring-up, billing, prompt options,
runtime-options assembly, run-result shaping) all live in the
**production executor** (`createAcpxEngineExecutor` with full
`AcpxEngineExecutorOptions`), not in the `execute(ctx)` control flow.
They will land in R377+ as the supporting helpers become available.
