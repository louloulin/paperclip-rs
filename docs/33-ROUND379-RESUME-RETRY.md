# R379 — Resume-Retry + Terminal Cleanup + Wall-Clock Timeout

## Goal

R376/R377/R378 wired the executor entry point, the `result_json` shape,
and the `set_config_option` seam. They left the Node-aligned **terminal
cleanup + resume-retry + wall-clock timeout** behavior unverified. R379
lands:

1. `MockAcpRuntime::set_config_option` honors overrides by default so
   tests below the strict-path branch can stay simple.
2. `execute()` always sets `clear_session=true` on the wall-clock
   timeout path — the warm handle is dropped and the session is killed,
   so the persisted session is dead no matter what the prior resume
   decision was.
3. R379 integration suite covering the seven terminal / resume /
   timeout paths.

## Module Updates

### `crates/pc-acpx/src/acp_runtime.rs`

```rust
async fn set_config_option(
    &self,
    _input: AcpRuntimeSetConfigOptionInput,
) -> Result<(), AcpRuntimeError> {
    Ok(())
}
```

Mock actors now opt out of the strict path by default. The strict
`apply_session_config_options_strict` (Node-aligned) keeps propagating
the first error to the caller when the runtime errors out — production
runtimes still reject unsupported overrides.

### `crates/pc-acpx/src/acpx_engine_executor.rs`

The wall-clock timeout branch now passes `true` to `build_timeout_result`
instead of `ensured.clear_session`. A timeout kills the session and
drops the warm handle, so the persisted session is dead regardless of
whether the prior resume decision cleared it.

```rust
return Ok(build_timeout_result(
    &prepared,
    &outcome.handle,
    session_params,
    message,
    true,
));
```

The Completed branch in `build_terminal_result` is unchanged: completed
turns leave `clear_session=false` unless the resume-retry path already
flagged it (the trailing `if clear_session { result.clear_session = true; }`
fallback handles that).

## Tests

`crates/pc-acpx/tests/round379_resume_retry.rs` — 7 tests, all green:

| Test | What it pins |
|---|---|
| `execute_warm_hits_after_compatible_resume` | Second call with `previous_session_params` carrying the first run's `session_codec::serialize` blob reuses the warm handle; `ensure_calls` stays at 1. |
| `execute_retries_fresh_session_when_resume_fails` | First `ensure_session` errors with "resume session not found" → same runtime retries once with `resume_id=None` and succeeds; `clear_session=true`, `ensure_calls=2`, `warm_handle_count=1`, stdout logs the retry notice. |
| `execute_starts_fresh_when_previous_session_params_incompatible` | `previous_session_params` has a different `configFingerprint` → cold start, no resume attempt, stdout logs the fingerprint-mismatch notice. |
| `execute_emits_timeout_result_when_wall_clock_fires` | `timeout_sec=1` + a hanging turn → `error_code=acpx_timeout`, `timed_out=true`, `status="cancelled"`, `clear_session=true`, `cancel` + `close` invoked, warm handle dropped. |
| `execute_returns_failed_terminal_with_clear_session` | Turn script returns `Failed` → `error_code=acpx_turn_failed`, `clear_session=true`, runtime closed, warm handle dropped. |
| `execute_oneshot_completed_drops_warm_handle` | `mode: oneshot` + completed → runtime closed, `warm_handle_count=0`. |
| `execute_persistent_completed_refreshes_last_used_at` | Persistent + completed with an injected clock advances `last_used_at` on the warm handle. |

The test runtime uses `session_codec::serialize` (camelCase) when
materializing `previous_session_params`, mirroring the heartbeat's
storage layer. `serde_json::to_value(&AcpxSessionParams)` would emit
snake_case keys which `is_compatible_session_value` would refuse.

## Node Parity

| Node path | Rust path |
|---|---|
| `isCompatibleSession` warm-hit branch | `ensure_session_with_resume_retry` with `reuse_warm_handle=true` |
| `isResumeFailure` retry path | `if resume_session_id.is_some() && is_resume_failure(&error)` |
| `clearSession: true` on timeout | passed `true` to `build_timeout_result` |
| `cancelSession + closeRuntime` on timeout | `runtime.cancel` + `runtime.close(discard_persistent_state=true)` + `drop_warm_handle` |
| `refreshLastUsedAt` on persistent completed | `cleanup_after_turn` mutates `last_used_at` in the warm handle entry |

## Coverage Delta

| Metric | Before R379 | After R379 |
|---|---|---|
| pc-acpx tests | 421 | **438** (+17 from the executor-internal `tests` module + 7 from R379's integration suite) |
| `MockAcpRuntime` defaults | strict | permissive (override-aware) |
| Timeout `clear_session` semantics | only when resume was attempted | always true |

## Next Steps

- R380: plug the production `SubprocessAcpRuntime::ensure_session` into
  the default executor factory so `execute()` runs against a real
  `acpx` binary without an injected mock.
- R381: re-enable pre/post-status read coalescing (current path reads
  `get_status` twice).
- R382: surface billing identity + provider family through the executor
  (currently only the hook exists).
