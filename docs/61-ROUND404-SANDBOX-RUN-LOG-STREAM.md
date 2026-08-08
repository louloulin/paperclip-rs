# R404 — `sandbox_run_log_stream` (host-side run-log tail loop)

## Module overview

| | Node | Rust (R404) |
|--|--|--|
| Source file | `packages/adapter-utils/src/sandbox-run-log-stream.ts` (278 lines) | `crates/pc-acpx/src/sandbox_run_log_stream.rs` (814 lines) |
| Public re-exports | `createSandboxRunLogTailFactory` | `create_sandbox_run_log_tail_factory` + types |
| Constants ported | 5 default + 3 marker | 8 `pub const` (1:1) |
| Helpers ported | `normalizePositiveInt`, `decodeBase64Section` | `normalize_positive_int`, `decode_base64_section` + private `shell_quote` + private `build_tick_script` + `parse_tick_output` |
| Async half | `start`, `finish`, `abort` (real async polling) | Same shape, real `tokio::sync::Mutex` + `tokio::task` + `oneshot` wake interrupt |
| Integration tests | — | `crates/pc-acpx/tests/round404_run_log_stream.rs` (9 tests) |

## Design choices

1. **Async boundary — first truly async module.** All previous rounds (R396-R403) deferred async functions because they needed a real SSH/sandbox runner. R404 is the first pc-acpx module that *owns* a background task: `start()` spawns a `tokio::task` that polls the runner at `poll_interval_ms`, decodes the base64 sections delimited by the `TAIL_MARKER_*` sentinels, and forwards chunks to the supplied sink. `finish()` / `abort()` interrupt the loop via a `tokio::sync::oneshot` channel.

2. **Minimal local runner trait.** Node imports `CommandManagedRuntimeRunner` from `command-managed-runtime.ts`. That full trait depends on the deferred SSH / sandbox runners (R403 was pure-helper-only). To keep this module self-contained, we define a minimal `SandboxRunLogRunner` trait containing exactly the `execute()` subset the tick consumes (`command`, `args`, `cwd`, `env`, `timeoutMs` → `exit_code`, `timed_out`, `stdout`). When the full `CommandManagedRuntimeRunner` lands, the local trait can be deleted in favor of an alias.

3. **`SandboxRunLogSink` as `Arc<dyn Fn(...) -> BoxFuture>`.** Node's `SandboxRunLogSink = (stream, chunk) => Promise<void>` is naturally a closure. Rust mirrors with a boxed `Fn` returning a `BoxFuture<'static, ()>`. Storing the sink as `Arc<...>` lets callers keep their own reference independent of the tail handle's lifetime.

4. **`wrap_command` is sync, `start/finish/abort` are async.** `wrap_command` reads only the immutable config fields of `TailHandleInner` (no lock needed). The three lifecycle methods acquire `state.lock()` once each — no double-mutex, no nested awaits inside a held lock.

5. **`String::from_utf8_lossy` for tick chunks.** Node uses a `StringDecoder("utf8")` which splits across ticks. Rust uses `from_utf8_lossy` which replaces invalid UTF-8 byte sequences with the U+FFFD replacement char — semantically equivalent for run-log streaming (the consumer is the UI, not a JSON parser).

6. **`shell_quote` duplication.** `ssh.rs`, `command_managed_runtime.rs`, and `sandbox_managed_runtime.rs` each carry a private `shell_quote` (Node source has the same duplication). R404 follows suit — extracting to a shared helper is a refactor deferred to a future round.

## Constants parity

| Node constant | Rust constant | Value |
|--|--|--|
| `DEFAULT_TAIL_POLL_INTERVAL_MS` | `DEFAULT_TAIL_POLL_INTERVAL_MS` | `250` |
| `DEFAULT_TAIL_MAX_CHUNK_BYTES` | `DEFAULT_TAIL_MAX_CHUNK_BYTES` | `64 * 1024` |
| `DEFAULT_TAIL_TICK_TIMEOUT_MS` | `DEFAULT_TAIL_TICK_TIMEOUT_MS` | `15_000` |
| `DEFAULT_TAIL_MAX_CONSECUTIVE_FAILURES` | `DEFAULT_TAIL_MAX_CONSECUTIVE_FAILURES` | `3` |
| `TAIL_MARKER_STDOUT` | `TAIL_MARKER_STDOUT` | `"__PAPERCLIP_RUN_LOG_STDOUT__"` |
| `TAIL_MARKER_STDERR` | `TAIL_MARKER_STDERR` | `"__PAPERCLIP_RUN_LOG_STDERR__"` |
| `TAIL_MARKER_END` | `TAIL_MARKER_END` | `"__PAPERCLIP_RUN_LOG_END__"` |
| `SANDBOX_EXEC_CHANNEL_ENV` | re-exported from `sandbox_callback_bridge` | `"PAPERCLIP_SANDBOX_EXEC_CHANNEL"` |
| `SANDBOX_EXEC_CHANNEL_BRIDGE` | re-exported from `sandbox_callback_bridge` | `"bridge"` |

## Test inventory

### Unit tests (`crates/pc-acpx/src/sandbox_run_log_stream.rs::tests`)

15 tests:

- `default_constants_match_node` — 5 default constant values
- `marker_constants_match_node` — 3 marker constant values
- `normalize_returns_value_when_positive` — happy path + boundary (1)
- `normalize_falls_back_on_zero_or_none` — 0 and None collapse to fallback
- `decode_base64_handles_empty_input` — empty / whitespace-only input
- `decode_base64_handles_whitespace_between_chunks` — multi-line input
- `decode_base64_returns_empty_on_invalid_input` — malformed input is non-fatal
- `parse_tick_output_splits_stdout_stderr` — happy path
- `parse_tick_output_handles_missing_sections` — partial marker sequence
- `parse_tick_output_rejects_out_of_order_markers` — end-before-stderr
- `shell_quote_wraps_simple_values` — plain / empty / embedded `'`
- `wrap_command_uses_shell_and_tee_script` — script + log paths + tee structure
- `factory_assigns_sequential_log_names` — `run-1`, `run-2` sequence
- `factory_normalizes_invalid_options` — None / Some(0) → defaults
- `stream_as_str_matches_node` — `Stdout`/`Stderr` → wire name

### Integration tests (`crates/pc-acpx/tests/round404_run_log_stream.rs`)

9 tests:

- `tick_streams_stdout_and_stderr_chunks` — single tick forwards both streams
- `finish_emits_only_suffix_past_streamed_offset` — multi-tick + dedup semantics
- `abort_stops_loop_without_flushing` — runner stops ticking after abort
- `consecutive_failures_mark_degraded_and_finish_emits_message` — degraded path
- `start_is_idempotent` — 3 calls, 0 spawned duplicate loops
- `tick_timeout_counts_as_failure` — `timed_out: true` ticks the failure counter
- `parse_tick_output_round_trips_utf8_and_binary` — UTF-8 + NUL bytes
- `parse_tick_output_rejects_malformed_input` — empty / partial / jumbled
- `factory_normalizes_options` — options → defaults via public surface

## Test counts

| Bucket | Pre-R404 | Post-R404 | Δ |
|--|--|--|--|
| `pc-acpx` lib tests | 733 | **748** | **+15** |
| `pc-acpx` integration test files | 36 | **37** | **+1** |
| `pc-acpx` `pub mod` count | 63 | **64** | **+1** |
| Node source lines ported (cumulative, R396-R404) | ~7 500 | **~7 800** | **+278** |

## Coverage gaps deferred to later rounds

- **Full `CommandManagedRuntimeRunner` trait** in `command_managed_runtime.rs` (R396 deferred the SSH / sandbox runners, so the trait was never defined; R404 introduces a minimal local substitute).
- **Live SSH / sandbox runners** that implement `SandboxRunLogRunner::execute` (deferred).
- **Refactor `shell_quote` to a shared helper** (3 modules currently carry private copies; low-risk cleanup but touches multiple files).
