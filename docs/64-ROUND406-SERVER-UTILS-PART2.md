# R406 — `server_utils` Part 2 (env helpers)

## Module overview

| | Node | Rust (R406) |
|--|--|--|
| Source file | `packages/adapter-utils/src/server-utils.ts` (env-helper section, ~150 lines: lines 1920-2400) | `crates/pc-acpx/src/server_utils.rs` (now 1486 lines total, +212 from R405) |
| Public re-exports added | `redactEnvForLogs`, `redactCommandTextForLogs`, `buildInvocationEnvForLogs`, `buildPaperclipEnv`, `applyPaperclipWorkspaceEnv`, `shapePaperclipWorkspaceEnvForExecution`, `rewriteWorkspaceCwdEnvVarsForExecution`, `refreshPaperclipWorkspaceEnvForExecution`, `sanitizeInheritedPaperclipEnv`, `defaultPathForPlatform`, `sanitizeSshRemoteEnv`, `ensurePathInEnv` | All 12 mirrored |
| Helper added | `resolveHostForUrl` (private) | `resolve_host_for_url` (pub) |
| Integration tests | — | `crates/pc-acpx/tests/round406_server_utils_part2.rs` (16 tests) |

## Design choices

1. **`process.env` → parameterized input.** Node `buildPaperclipEnv` reads `process.env.PAPERCLIP_LISTEN_HOST`, `HOST`, `PAPERCLIP_LISTEN_PORT`, `PORT`, `PAPERCLIP_RUNTIME_API_URL`, `PAPERCLIP_API_URL` directly. Rust does not have `process.env`, so R406 parametrizes these via `BuildPaperclipEnvInput { runtime_env: &HashMap<String, String>, default_listen_host, default_listen_port }`. Callers in higher crates wire `process::env` themselves.

2. **`HashMap<String, serde_json::Value>` for env config.** Node `rewriteWorkspaceCwdEnvVarsForExecution` filters env entries to string-only via `Object.fromEntries(... filter (entry): entry is [string, string] => typeof entry[1] === "string")`. Rust mirrors with `HashMap<String, serde_json::Value>` and `filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))`.

3. **`BTreeMap` ↔ `HashMap` boundary.** `sanitize_ssh_remote_env` is a thin wrapper over `pc_acpx::remote_execution_env::sanitize_remote_execution_env`, which uses `BTreeMap` for deterministic iteration. The wrapper copies into / out of `BTreeMap` so callers see `HashMap` (matches the rest of pc-acpx).

4. **`refreshPaperclipWorkspaceEnvForExecution` mutates `env` in place AND returns shaped output.** Node mutates `input.env` (deletes 3 stale keys, applies new ones) and returns `{workspaceCwd, workspaceWorktreePath, workspaceHints}`. Rust mirrors with `&mut HashMap` + `-> ShapePaperclipWorkspaceEnvOutput`.

5. **`&HashMap` defaults via `OnceLock`.** Several input structs default to empty `&HashMap`. Since `&HashMap` does not implement `Default`, R406 introduces two `OnceLock<HashMap>` statics (`empty_hashmap_str()`, `empty_hashmap_value()`) for borrow-friendly defaults without temporary-value lifetime issues.

6. **`posix_resolve` helper for path equality.** Node `rewriteWorkspaceCwdEnvVarsForExecution` compares `path.resolve(trimmed) !== localWorkspaceCwd`. Rust implements `posix_resolve` as a simple `trim_end_matches('/')` since all callers pass absolute POSIX paths; full Node `path.resolve` semantics (cwd-relative + `.`/`..` collapse) are deferred with the fs-touching helpers.

7. **`refresh` config priority rules.** Node enforces:
   - `isForbiddenConfigEnvKey(key)` → drop (PAPERCLIP_API_KEY never accepted from config)
   - `isPaperclipRuntimeEnvKey(key) && key in input.env` → drop (Paperclip already set it)
   - otherwise forward
   
   Rust mirrors with two early-continue guards before `env.insert`.

## Test inventory

### Unit tests added (`crates/pc-acpx/src/server_utils.rs::tests`)

20 new unit tests:

- `redact_env_for_logs_replaces_sensitive_values` — sensitive keys redacted, others preserved
- `redact_command_text_for_logs_redacts_secret_flags` — wrapper forwards `command_redaction`
- `build_invocation_env_for_logs_merges_runtime_keys_then_redacts` — runtime keys + override rule
- `resolve_host_for_url_normalizes_wildcards_and_ipv6` — `0.0.0.0`, `::`, bracketed IPv6
- `build_paperclip_env_fills_canonical_vars` — happy path with listen_host/port
- `build_paperclip_env_falls_back_to_defaults` — empty runtime env → defaults
- `build_paperclip_env_prefers_runtime_api_url` — `RUNTIME_API_URL` beats `API_URL`
- `apply_paperclip_workspace_env_writes_non_empty_keys` — full input
- `apply_paperclip_workspace_env_skips_empty_values` — empty / None values skipped
- `shape_paperclip_workspace_env_local_target_returns_inputs_unchanged` — local = pass-through
- `shape_paperclip_workspace_env_remote_repoints_cwd_to_staged_dir` — staged hint cwd rewritten
- `shape_paperclip_workspace_env_remote_drops_unstaged_cwd` — unstaged hint cwd dropped
- `rewrite_workspace_cwd_env_vars_local_target_is_passthrough` — local = pass-through
- `rewrite_workspace_cwd_env_vars_remote_rewrites_matching_local_cwd` — match + non-match paths
- `refresh_paperclip_workspace_env_applies_shaped_env_to_input_env` — clears stale keys + applies
- `sanitize_inherited_paperclip_env_strips_runtime_vars` — runtime allowlist preserved, others stripped
- `default_path_for_platform_returns_platform_specific_value` — Windows / POSIX
- `sanitize_ssh_remote_env_drops_local_only_keys` — wrapper smoke test
- `ensure_path_in_env_preserves_existing_path` — PATH present = unchanged
- `ensure_path_in_env_fills_default_when_missing` — empty → default

### Integration tests (`crates/pc-acpx/tests/round406_server_utils_part2.rs`)

16 tests:

- `redaction_pipeline_strips_secrets_from_invocation_env` — full redaction chain
- `redact_env_for_logs_preserves_non_sensitive_values_verbatim` — passthrough for safe keys
- `redact_command_text_for_logs_redacts_common_secret_flags` — `--api-key`, `--token`, `--password`
- `build_paperclip_env_priority_chain_resolves_api_url` — full priority chain (runtime → listen → default)
- `apply_paperclip_workspace_env_handles_partial_inputs` — empty / None skip semantics
- `shape_workspace_env_remote_repoints_all_hints_to_staged_dirs` — multi-hint staging
- `shape_workspace_env_remote_trims_workspace_cwd` — whitespace trim
- `rewrite_remote_only_rewrites_when_target_is_remote` — local pass-through confirmed
- `rewrite_filters_non_string_env_values` — JSON number entries dropped
- `refresh_clears_stale_workspace_env_then_applies_shaped` — full refresh cycle
- `refresh_serializes_workspace_hints_as_json` — hints → JSON env var
- `sanitize_strips_paperclip_runtime_vars_but_keeps_three_runtime_keys` — runtime allowlist
- `ensure_path_in_env_returns_env_unchanged_when_path_present` — idempotent
- `ensure_path_in_env_fills_posix_default_when_missing` — default fallback
- `default_path_for_platform_matches_node_literals` — full literal match
- `sanitize_ssh_remote_env_wrapper_composes_through_remote_execution_env` — wrapper smoke

## Test counts

| Bucket | Pre-R406 | Post-R406 | Δ |
|--|--|--|--|
| `pc-acpx` lib tests | 773 | **793** | **+20** |
| `pc-acpx` integration test files | 38 | **39** | **+1** |
| `pc-acpx` `pub mod` count | 65 | 65 | 0 |
| Node source lines ported (cumulative, R396-R406) | ~8 150 | **~8 500** | **+~350** |

## Coverage gaps deferred to R407+ (`server_utils` Part 3-4 + async runtime)

- **Skill entries** (`PaperclipSkillEntry`, `PaperclipDesiredSkillEntry`, `InstalledSkillTarget`, `MaterializedPaperclipSkillCopyResult`, `PaperclipSkillEntry.normalized_*` helpers, `buildRuntimeMountedSkillSnapshot`, `buildPersistentSkillSnapshot`, `readPaperclipSkillSyncPreference`, `resolvePaperclipDesiredSkillNames`, `writePaperclipSkillSyncPreference`, `isPaperclipSkillSourceMissing`, `resolvePaperclipSkillMissingDetail`, `resolveSkillDetail`, `resolveInstalledEntryTarget`, `skillLocationLabel`, `buildManagedSkillOrigin`, `isMaintainerOnlySkillTarget`, `normalizePathSlashes`) — R407
- **Skill async** (`readPaperclipRuntimeSkillEntries`, `readPaperclipSkillMarkdown`, `listPaperclipSkillEntries`, `readInstalledSkillTargets`, `resolvePaperclipSkillsDir`) — R407 (with fs-tokio)
- **Wake payload + watchdog** (`PaperclipWakePayload`, `normalizePaperclipWakePayload`, `stringifyPaperclipWakePayload`, `isPaperclipRecoveryWakePayload`, `readPaperclipIssueWorkModeFromContext`, `isAssignmentShapedPaperclipWakeReason`, `selectPaperclipTaskMarkdown`, `renderPaperclipWakePrompt`, `WATCHDOG_DEFAULT_MANDATE`, `DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE`, `expandHomePrefix`, `resolvePaperclipInstanceRootForAdapter`) — R408
- **Async runtime** (`resolveCommandForLogs`, `resolveCommandPath`, `resolveSpawnTarget`, `ensureCommandResolvable`, `ensureAbsoluteDirectory`, `ensurePaperclipSkillSymlink`, `materializePaperclipSkillCopy`, `removeMaintainerOnlySkillSymlinks`, `pathExists`, `windowsPathExts`, `resolveWindowsCmdShell`, `quoteForCmd`, `runChildProcess`, `spawnAsync`) — deferred with full spawn layer wiring
