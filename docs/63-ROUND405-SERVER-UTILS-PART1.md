# R405 — `server_utils` Part 1 (sync pure helpers + types)

## Module overview

| | Node | Rust (R405) |
|--|--|--|
| Source file | `packages/adapter-utils/src/server-utils.ts` (~150 lines of R405 scope, lines 18-128 + 350-440) | `crates/pc-acpx/src/server_utils.rs` (760 lines) |
| Public re-exports | `parseObject`, `asString`, `asNumber`, `asBoolean`, `asStringArray`, `parseJson`, `appendWithCap`, `appendWithByteCap`, `resolvePathValue`, `renderTemplate`, `joinPromptSections`, `signalRunningProcess`, `isPaperclipRuntimeEnvKey`, `isForbiddenConfigEnvKey` | `parse_object`, `as_string`, `as_number`, `as_boolean`, `as_string_array`, `parse_json`, `append_with_cap`, `append_with_byte_cap`, `resolve_path_value`, `render_template`, `join_prompt_sections`, `signal_decision` (decision-only; async kill deferred), `is_paperclip_runtime_env_key`, `is_forbidden_config_env_key` |
| Constants ported | 5 (`UNMANAGED_BACKGROUND_TASK_*`, `MAX_CAPTURE_BYTES`, `MAX_EXCERPT_BYTES`, `REDACTED_LOG_VALUE`, plus 2 private regex sources) | 8 `pub const` (1:1) + 2 regex sources exposed as `pub const *_SRC` |
| Types ported | `RunProcessResult`, `TerminalResultCleanupOptions`, `TerminalResultCleanupEvidence`, `RunningProcess` (private), `SpawnTarget` (private) | All five mirrored |
| Integration tests | — | `crates/pc-acpx/tests/round405_server_utils_part1.rs` (15 tests) |

## Design choices

1. **Decision-only `signal_decision`.** Node's `signalRunningProcess` performs a real `process.kill(-pgid, signal)` syscall + falls back to `child.kill(signal)`. The syscall half is deferred with the async spawn layer (R407+). For R405 we extract the pure decision logic — given `(process_group_id, already_exited, is_windows)`, return the target (`ProcessGroup { pgid }` / `DirectChild` / `None`). Callers wire the actual `kill` when the spawn layer lands.

2. **`spawnTarget.cleanup` as `Arc<dyn Fn() -> BoxFuture>`.** Node's `SpawnTarget.cleanup?: () => Promise<void>` is an async closure. We mirror as `Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>` so callers can register teardown work without forcing pc-acpx to depend on a runtime.

3. **`join_prompt_sections` accepts `Option<S>`.** Node's signature is `Array<string | null | undefined>` — the closest Rust analog is `Vec<Option<&str>>` (or any iterator of `Option<S>` where `S: AsRef<str>`). Dropping `None` first, then trimming / filtering empty strings, matches the Node semantics exactly.

4. **`append_with_byte_cap` preserves UTF-8 boundaries.** Node's byte-cap walks forward past continuation bytes (`0x80 <= byte < 0xC0`). Rust's `str::char_indices()` already gives us char boundaries, so we just `find` the first one at-or-past the requested start. The Node behavior happens to coincide: both stop at the first non-continuation byte, which in well-formed UTF-8 is always a char boundary.

5. **`append_with_cap` counts *chars*.** Node's `combined.length` is UTF-16 code units. For ASCII this equals `chars().count()`. We preserve the distinction: `append_with_cap` counts chars (use `String::chars().count()`), `append_with_byte_cap` counts bytes. Non-ASCII strings will diverge between the two, mirroring Node.

6. **Wire vs runtime types.** `RunProcessResult` and `TerminalResultCleanupEvidence` are wire types (serialized over JSON to adapters), so they get `#[derive(Serialize, Deserialize)]` with `#[serde(rename_all = "camelCase")]`. `SpawnTarget`, `TerminalResultCleanupOptions`, and `RunningProcessSignalInfo` are pure runtime types — no serde, no annotations.

7. **`once_cell::sync::Lazy<Regex>`.** pc-acpx didn't have `once_cell` declared; R405 adds it (`"once_cell" = "1"`). All three regexes (`PATH_SEGMENT_RE`, `SENSITIVE_ENV_KEY_RE`, `TEMPLATE_PLACEHOLDER_RE`) compile once at first use.

## Constants parity

| Node constant | Rust constant | Value |
|--|--|--|
| `UNMANAGED_BACKGROUND_TASK_STOP_REASON` | `UNMANAGED_BACKGROUND_TASK_STOP_REASON` | `"unmanaged_background_task_stopped"` |
| `UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON` | `UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON` | `"unmanaged background task stopped; no durable live path"` |
| `MAX_CAPTURE_BYTES` | `MAX_CAPTURE_BYTES` | `4 * 1024 * 1024` |
| `MAX_EXCERPT_BYTES` | `MAX_EXCERPT_BYTES` | `32 * 1024` |
| `TERMINAL_RESULT_SCAN_OVERLAP_CHARS` | (private const inside `server_utils`) | `64 * 1024` |
| `DEFAULT_PAPERCLIP_INSTANCE_ID` | (private const inside `server_utils`) | `"default"` |
| `REDACTED_LOG_VALUE` | `REDACTED_LOG_VALUE` | `"***REDACTED***"` |
| `PATH_SEGMENT_RE` (source) | `PATH_SEGMENT_RE_SRC` (source exposed) | `"^[a-zA-Z0-9_-]+$"` |
| `SENSITIVE_ENV_KEY` (source) | `SENSITIVE_ENV_KEY_RE_SRC` (source exposed) | `"(?i)(key|token|secret|password|passwd|authorization|cookie)"` |

`TERMINAL_RESULT_SCAN_OVERLAP_CHARS` and `DEFAULT_PAPERCLIP_INSTANCE_ID` are only used inside deferred async functions (R408+), so they stay private until those land.

## Test inventory

### Unit tests (`crates/pc-acpx/src/server_utils.rs::tests`)

25 tests:

- `unmanaged_background_task_constants_match_node` — both stop/liveness strings
- `capture_constants_match_node` — MAX_CAPTURE_BYTES / MAX_EXCERPT_BYTES / SCAN_OVERLAP / DEFAULT_INSTANCE / REDACTED
- `regex_source_constants_match_node` — PATH_SEGMENT_RE_SRC + SENSITIVE_ENV_KEY_RE_SRC
- `runtime_env_key_matches_paperclip_prefix` — prefix match, case-sensitive
- `forbidden_config_env_key_only_blocks_api_key` — narrow API_KEY list
- `path_segment_matches_letters_digits_dash_underscore` — accepted + rejected
- `sensitive_env_key_matches_keywords_case_insensitive` — keyword detection
- `parse_object_returns_object_or_empty_map` — accepts object, rejects others
- `as_string_returns_string_when_non_empty` — empty + non-string → fallback
- `as_number_returns_finite_number` — finite only, non-number → fallback
- `as_boolean_returns_bool_when_present` — only `true`/`false` survive
- `as_string_array_filters_to_strings` — array filter
- `parse_json_returns_value_or_none` — valid + invalid
- `append_with_cap_keeps_trailing_chars` — truncation semantics
- `append_with_byte_cap_keeps_trailing_bytes` — byte-level truncation
- `append_with_byte_cap_respects_utf8_boundaries` — never splits codepoints
- `resolve_path_value_walks_dotted_path` — nested lookup
- `render_template_replaces_placeholders` — substitution
- `render_template_tolerates_whitespace_and_missing_paths` — whitespace + missing
- `join_prompt_sections_trims_and_filters` — trim + filter + join
- `signal_decision_returns_none_when_already_exited` — None on exited
- `signal_decision_returns_process_group_on_posix` — pgid path on POSIX
- `signal_decision_returns_direct_child_on_windows` — no pgid on Windows
- `signal_decision_falls_back_when_pgid_missing_or_zero` — null/0/negative
- `cleanup_evidence_constructor_fills_canonical_fields` — wire shape

### Integration tests (`crates/pc-acpx/tests/round405_server_utils_part1.rs`)

15 tests:

- `env_key_classifiers_partition_paperclip_namespace` — runtime vs forbidden interplay
- `sensitive_env_key_detects_full_keyword_set` — full keyword set (API_KEY, GH_TOKEN, DB_PASSWORD, DB_PASSWD, AUTHORIZATION, USER_COOKIE)
- `path_segment_validator_rejects_special_chars` — dots, slashes, spaces
- `json_coercers_handle_all_value_kinds` — full coercion matrix
- `parse_json_handles_object_and_rejects_invalid` — round-trip + invalid
- `append_with_cap_counts_chars_not_bytes` — char vs byte distinction
- `append_with_byte_cap_never_splits_a_utf8_codepoint` — boundary safety
- `append_with_cap_and_byte_cap_default_to_max_capture` — default cap honored
- `render_template_walks_nested_dotted_paths` — multi-level substitution
- `resolve_path_value_stringifies_complex_leaves` — object leaves
- `join_prompt_sections_handles_optionals_and_separator` — `None` filter
- `join_prompt_sections_default_separator_is_blank_line` — `\n\n` default
- `signal_decision_canonical_matrix` — full pgid / Windows / None matrix
- `cleanup_evidence_wire_shape_matches_node` — serialized camelCase JSON
- `public_constants_match_node_literals` — public surface

## Test counts

| Bucket | Pre-R405 | Post-R405 | Δ |
|--|--|--|--|
| `pc-acpx` lib tests | 748 | **773** | **+25** |
| `pc-acpx` integration test files | 37 | **38** | **+1** |
| `pc-acpx` `pub mod` count | 64 | **65** | **+1** |
| Node source lines ported (cumulative, R396-R405) | ~7 800 | **~8 150** | **+~350** |

## Coverage gaps deferred to R406+ (`server_utils` Part 2-4)

- **Env builders** (`buildPaperclipEnv`, `applyPaperclipWorkspaceEnv`, `shapePaperclipWorkspaceEnvForExecution`, `rewriteWorkspaceCwdEnvVarsForExecution`, `refreshPaperclipWorkspaceEnvForExecution`, `sanitizeInheritedPaperclipEnv`, `sanitizeSshRemoteEnv`, `defaultPathForPlatform`, `ensurePathInEnv`, `buildInvocationEnvForLogs`) — R406
- **Skill entries** (`PaperclipSkillEntry`, `PaperclipDesiredSkillEntry`, `InstalledSkillTarget`, `buildRuntimeMountedSkillSnapshot`, `buildPersistentSkillSnapshot`, `readPaperclipRuntimeSkillEntries`, `readPaperclipSkillMarkdown`, `readInstalledSkillTargets`, `listPaperclipSkillEntries`, `resolvePaperclipSkillsDir`, `resolvePaperclipInstanceRootForAdapter`, `readPaperclipSkillSyncPreference`, `resolvePaperclipDesiredSkillNames`, `writePaperclipSkillSyncPreference`) — R407
- **Wake payload + watchdog** (`PaperclipWakePayload`, `normalizePaperclipWakePayload`, `stringifyPaperclipWakePayload`, `isPaperclipRecoveryWakePayload`, `readPaperclipIssueWorkModeFromContext`, `isAssignmentShapedPaperclipWakeReason`, `selectPaperclipTaskMarkdown`, `renderPaperclipWakePrompt`, `WATCHDOG_DEFAULT_MANDATE`, `DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE`) — R408
- **Async runtime** (`spawnAsync`, `ensureCommandResolvable`, `runChildProcess`, `resolveCommandForLogs`, `ensureAbsoluteDirectory`, `ensurePaperclipSkillSymlink`, `materializePaperclipSkillCopy`, `removeMaintainerOnlySkillSymlinks`) — deferred with full spawn layer wiring
