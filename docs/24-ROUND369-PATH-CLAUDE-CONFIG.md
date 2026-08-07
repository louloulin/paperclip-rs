# R369 — Path Resolution, Claude Settings, and Session Config Helpers

## Goal

Continue the R362→R368 port of `paperclip/packages/adapter-utils/src/acpx-engine/execute.ts`
into `paperclip-rs/crates/pc-acpx`. R369 covers the remaining Node pure helpers
and JSON I/O seams that did not fit the earlier rounds:

- Path resolution (`defaultPaperclipInstanceDir`, `defaultStateDir`,
  `resolveManagedCodexHomeDir`).
- Gemini command-shell normalization (`normalizeGeminiAcpCommandShell`).
- Codex startup config builder (`buildCodexStartupConfig`).
- Session compatibility (`uniqueSorted`, `isCompatibleSession`).
- Session config option derivation (`sessionConfigOptions`,
  `resultErrorMessage`, `usageBreakdownsEqual`, `renderPaperclipEnvNote`,
  `renderApiAccessNote`).
- Per-worktree Claude settings writer (`writePaperclipClaudeSettings`).
- Referenced-source content signature (`referencedSourceContentSignature`).

Each port mirrors its Node counterpart exactly: pure helpers stay pure, the
file-writing helper uses `tokio::fs` and the existing atomic-write helper, and
all decision points remain the responsibility of the caller.

## Modules Added (6 new files in `crates/pc-acpx/src/`)

| Module | Functions | Notes |
|---|---|---|
| `paths.rs` | `expand_home_prefix`, `resolve_paperclip_instance_root`, `default_paperclip_instance_dir`, `default_state_dir`, `resolve_managed_codex_home_dir` | Pure resolver + `std::env` wrapper; `InvalidInstanceId` error variant |
| `gemini_command_shell.rs` | `normalize_gemini_acp_command_shell_with_env`, `normalize_gemini_acp_command_shell` | Composes with existing `gemini_version` helpers; version sourced from env override |
| `codex_startup_config.rs` | `build_codex_startup_config` (+ `CodexStartupConfigInput/Output`) | Pure JSON merge, flags invalid existing configs |
| `session_compat.rs` | `unique_sorted`, `is_compatible_session` (+ `AcpxPreparedRuntimeLite`) | Pure helpers; canonical path comparison |
| `session_config_options.rs` | `session_config_options`, `result_error_message`, `usage_breakdowns_equal`, `render_paperclip_env_note`, `render_api_access_note` (+ `SessionConfigOption`) | Pure helpers; rich env-text rendering |
| `paperclip_claude_settings.rs` | `paperclip_claude_settings_write_with`, `referenced_source_content_signature` (+ input/result types) | Async I/O via `fs_ops`; SHA-256 sig over file tree |

`error.rs` gained two variants: `AcpxError::InvalidInstanceId(String)` and
`AcpxError::Json { context, error }`.

`lib.rs` exposes the new types and functions via `pub use` re-exports.

## Design Notes

### Pure helpers stay pure
- `resolve_paperclip_instance_root` accepts a caller-provided env map so tests
  drive every input deterministically. `default_paperclip_instance_dir()` is
  the production wrapper that reads `std::env`.
- `normalize_gemini_acp_command_shell_with_env` reads
  `PAPERCLIP_GEMINI_VERSION_OVERRIDE` from the env map instead of spawning the
  real `gemini --version`. This keeps the helper pure and means the engine
  can pre-probe the version once and reuse the result.
- `build_codex_startup_config` returns `None` when no runtime override is
  requested, so the engine skips the disk rewrite entirely.

### JSON I/O is async + atomic
- `paperclip_claude_settings_write_with` is `async` and uses the existing
  `ensure_parent_dir` + `write_file_atomically` (mode `0o600`) to land the
  settings file safely. The merge preserves the user's existing
  `permissions.allow` and `permissions.additionalDirectories`, prepends the
  Paperclip bridge entries, and force-overrides `defaultMode: "dontAsk"`.

### `AcpxPreparedRuntimeLite`
A minimal view of `PreparedRuntime` carrying only the fields that affect
session routing and runtime overrides. The full `PreparedRuntime` is too heavy
for `is_compatible_session` and `session_config_options`. The struct exposes a
`with_overrides(model, effort, fast_mode)` builder so callers can apply
runtime overrides without threading extra fields through the type.

### `referenced_source_content_signature`
Walks the directory tree, hashing `file:<path>:<size>` then the bytes for
regular files, `symlink:<path>:<target>` for symlinks, and `other:<path>:<mode>`
for everything else. Skips `node_modules`, `.git`, `target`, `dist`, `.next`.
On any I/O failure the digest is replaced with `unreadable:<reason>` so the
caller can distinguish "tree changed" from "tree unreadable" by prefix.

## Tests

`crates/pc-acpx/tests/round369_path_helpers.rs` — 26 tests, all green:

| Group | Tests |
|---|---|
| `unique_sorted` | 2 |
| `resolve_paperclip_instance_root` | 4 |
| `default_state_dir` / `resolve_managed_codex_home_dir` | 2 |
| `normalize_gemini_acp_command_shell_with_env` | 3 |
| `build_codex_startup_config` | 4 |
| `is_compatible_session` | 1 |
| `session_config_options` | 1 |
| `result_error_message` / `usage_breakdowns_equal` | 3 |
| `render_paperclip_env_note` / `render_api_access_note` | 2 |
| `paperclip_claude_settings_write_with` | 3 |
| `referenced_source_content_signature` | 1 |

## Baseline

- `pc-acpx` lib + integration tests: **253 / 253 green** (R368 was 227; +26).
- `pc-heartbeat` integration tests: **928 / 928 green** (no regression).

## What's Next (R370+)

R369 closes the small-helper gap. The remaining `acpx-engine/execute.ts` work
that still requires porting falls into four buckets:

1. **SubprocessAcpRuntime** — replace `MockAcpRuntime` with a real
   `acpx` JSON-RPC wire (stdin/stdout/stderr). Highest ROI; spans R370-R372.
2. **buildRuntime / createAcpxEngineExecutor / execute** — top-level engine
   entry point that wires every helper together. R372-R373 once
   `SubprocessAcpRuntime` exists.
3. **stageAcpRemoteRuntime / settleRemoteBridgeStarts / cleanupRemoteBridges** —
   runner-backed sandbox staging. Crosses `paperclip/packages/sandbox-utils/`;
   lower priority because the remote lanes are not in the active local path.
4. **Cache lifecycle** (`cleanupIdleHandles`, `scheduleIdleHandleCleanup`,
   `closeWarmHandle`, `discardStagedRuntime`, `withSessionStagingLease`,
   `saveStagedRuntimeAfterCleanTurn`, `warmHandleMatches`,
   `clearWarmHandleTimer`) — the cache-management seams that R368 left out.
   Medium priority; can ship in one round once we decide on the cache shape.

### Completion Estimate

| Area | Status |
|---|---|
| `pc-acpx` helpers + protocol | ~99% (6 modules + 26 tests this round) |
| `pc-acpx` runtime wire (`SubprocessAcpRuntime`) | 0% — blocks `buildRuntime` |
| `pc-acpx` cache lifecycle helpers | 0% |
| Recovery main chain (pc-heartbeat + pc-repos + pc-core) | ~96% |
| Full backend (adapters + plugins) | ~72% |
