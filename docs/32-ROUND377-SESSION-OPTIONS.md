# R377 — Session Config Options + `build_session_params`

## Goal

Land the post-`ensure_session` session configuration seam and the
`buildSessionParams` projection used by the run result. R376 wired the
executor control flow but deferred:
1. The per-session `set_config_option` calls the runtime needs to honor
   the prepared configuration.
2. The `AcpxSessionParams` record the run result carries back to the
   heartbeat (so the next run can resume via the warm handle / staged
   runtime path).

## Module Updates

`crates/pc-acpx/src/session_codec.rs` — adds [`build_session_params`].

| Symbol | Mirrors |
|---|---|
| `build_session_params(prepared, handle)` | Node `buildSessionParams({ prepared, handle })` |

`crates/pc-acpx/src/acpx_engine_executor.rs` — adds 2 helpers + `session_params`
field on [`AdapterExecutionResult`].

| Symbol | Mirrors |
|---|---|
| `AcpxEngineExecutor::apply_session_config_options` | Node `for (const opt of sessionConfigOptions(prepared)) await runtime.setConfigOption(...)` |
| `AcpxEngineExecutor::build_session_params` | (re-export wrapper) |
| `AdapterExecutionResult::with_session_params` | builder method |
| `AdapterExecutionResult::session_params` field | Node `resultJson.sessionParams` |

## Session Config Options (cross-agent matrix)

| Agent | `model` | `effort` | `service_tier` (fast_mode) |
|---|---|---|---|
| `claude` | — (set via `ANTHROPIC_MODEL` env) | ✅ | ✅ |
| `codex` | — (set via `CODEX_CONFIG`) | — (set via `CODEX_CONFIG`) | — (set via `CODEX_CONFIG`) |
| `gemini` | ✅ | ✅ | ✅ |
| custom | ✅ | ✅ | ✅ |

Empty `model` / `effort` / `fast_mode = false` collapse to no
`set_config_option` calls — the helper filters them out before the
runtime factory consults it.

## `build_session_params(prepared, handle)`

Projects the relevant fields from `PreparedRuntime` + `AcpRuntimeHandle`
into `AcpxSessionParams` for the run result.

| Field | Source |
|---|---|
| `runtime_session_name` | `handle.runtime_session_name ?? handle.acpx_record_id` |
| `session_key` | `prepared.session_key` |
| `acpx_record_id` | `handle.acpx_record_id` |
| `acp_session_id` | `handle.backend_session_id` |
| `agent_session_id` | `handle.agent_session_id` |
| `agent` | `prepared.acpx_agent` |
| `cwd` | `prepared.cwd` |
| `mode` | `prepared.mode.as_str()` |
| `state_dir` | `prepared.state_dir` |
| `config_fingerprint` | `prepared.fingerprint` |
| `workspace_id` | `prepared.workspace_id` (when non-empty) |
| `repo_url` | `prepared.workspace_repo_url` (when non-empty) |
| `repo_ref` | `prepared.workspace_repo_ref` (when non-empty) |
| `remote_execution` | — (deferred to R380+; sandbox lane only) |

## Tests

| File | Tests |
|---|---|
| `crates/pc-acpx/src/session_codec.rs::build_session_params_tests` | 6 (new) |
| `crates/pc-acpx/tests/round377_session_options.rs` | 11 (new) |

Total: **17 new tests**, all green.

| pc-acpx | Before R377 | After R377 |
|---|---|---|
| Total tests | 404 | **421** |
| Public types / re-exports | — | +1 (`build_session_params`) |

## Coverage of Node post-`ensureSession` SessionConfigOptions Block

| Concern | Node | Rust (R377) |
|---|---|---|
| `sessionConfigOptions(prepared)` cross-agent matrix | ✅ | ✅ |
| `await runtime.setConfigOption({ handle, key, value })` loop | ✅ | ✅ (`apply_session_config_options`) |
| `buildSessionParams({ prepared, handle })` projection | ✅ | ✅ (`build_session_params`) |
| `resultJson.sessionParams` projection onto the run result | ✅ | ✅ (`session_params` field) |
| `resultJson.sessionDisplayId` precedence (agent ?? backend ?? runtime) | ✅ | ✅ (already in R376) |
| `clearSession` decision | ✅ | ❌ deferred (R378+) |
| `serializeSessionParams` round-trip | ✅ | ✅ (already in R362) |

## Summary

R377 lands the **session configuration seam** the ACP runtime needs to
honor the prepared configuration. After `ensure_session`, the executor:

1. Computes the cross-agent config options list.
2. Calls `runtime.set_config_option({ handle, key, value })` for each
   option (errors swallowed — a partial config doesn't kill the
   session).
3. Builds the `AcpxSessionParams` record from `PreparedRuntime` +
   `AcpRuntimeHandle`.
4. Attaches the session params to the run result via
   `with_session_params(...)`.

The remaining concerns (`clearSession` decision, `resultJson`
deep-shape, `usageBasis`, `costUsd`, `billingFields`, `referencedProjectStagingFailures`)
all live in the **result-shaping** layer that lands in R378+.
