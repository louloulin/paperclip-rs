# R407 — `server_utils` Part 3 (skill entries)

## Module overview

| | Node | Rust (R407) |
|--|--|--|
| Source file | `packages/adapter-utils/src/server-utils.ts` skill section (~150 lines: types + sync helpers + snapshot builders, lines 128-340 + 2440-2900) | `crates/pc-acpx/src/server_utils.rs` (now 3167 lines total, +581 from R406) |
| Public re-exports added | 25+ (types + pure helpers + snapshot builders) | 25 mirrored |
| Constants ported | `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES`, `MATERIALIZED_SKILL_SENTINEL`, `MATERIALIZED_SKILL_LOCK_OWNER`, `MATERIALIZED_SKILL_LOCK_STALE_MS` | All 4 mirrored |
| Integration tests | — | `crates/pc-acpx/tests/round407_server_utils_part3.rs` (20 tests) |

## Design choices

1. **`SkillDetail<'a>` enum for string-or-callback.** Node's `resolveSkillDetail` takes `string | ((entry) => string | null)`. Rust mirrors with `enum SkillDetail<'a> { Literal(&'a str), Callback(&'a dyn Fn(&PaperclipSkillEntry) -> Option<String>) }`. The `Debug` derive is hand-written (the `dyn Fn` variant cannot derive `Debug` automatically).

2. **`AvailableSkillRef<'a>` borrowed view for canonicalization.** Node's `canonicalizeDesiredPaperclipSkillReference` only needs `{ key, runtimeName }`. Rust uses a borrowed struct to avoid forcing callers to clone the full `PaperclipSkillEntry` just for the lookup. Mirrors Node's inline array parameter shape `{ key: string; runtimeName?: string | null }[]`.

3. **`Vec + HashSet` for first-wins dedup.** Node uses `Map<string, PaperclipDesiredSkillEntry>` which preserves insertion order. Rust's std `HashMap` does not, so R407 uses `Vec + HashSet` (`seen.insert(entry.key.clone())`) to preserve Node's "first wins, insertion order" semantics. Verified by `read_paperclip_skill_sync_preference_parses_string_and_object_entries` test.

4. **`RuntimeMountedSkillSnapshotOptions` + `PersistentSkillSnapshotOptions` accept borrowed slices + Option references.** Mirrors Node's `{ availableEntries, desiredSkills, ... }` input shape without forcing allocations. `PersistentSkillSnapshotOptions.installed` is `Option<&HashMap>` to allow None + a static empty default.

5. **Manual `Default` impl for `RuntimeMountedSkillSnapshotOptions`.** The `SkillDetail<'a>` field doesn't implement `Default`, so the struct uses a manual `impl Default` that initializes `configured_detail` to `SkillDetail::Literal("")`. Keeps `..Default::default()` ergonomics for callers.

6. **`posix_join` / `posix_dirname` helpers.** Node's `path.join`, `path.dirname`, `path.resolve` are runtime calls. R407 implements simple POSIX-only versions that handle the cases the snapshot builders need: `posix_join(parent, child)` joins with a single `/`; `posix_dirname(p)` returns the part before the final `/`; `posix_resolve_v2(p)` trims a trailing `/`. Node's host-cwd-relative resolution is not needed because all callers pass absolute POSIX paths.

7. **`expand_home_prefix` parametrizes `home_dir`.** Node reads `os.homedir()` directly; Rust cannot. The Rust signature accepts `home_dir: &str` so callers (in higher crates) pass `dirs::home_dir().unwrap_or("/")` or similar.

8. **`resolve_paperclip_instance_root_for_adapter` returns `".../invalid"` on invalid `PAPERCLIP_INSTANCE_ID`.** Node throws; Rust pc-acpx prefers non-panicking. The helper falls back to the canonical path with `"invalid"` as the segment when the regex rejects the id. Higher crates can detect the `"invalid"` sentinel and surface the error upstream.

9. **`write_paperclip_skill_sync_preference` returns the updated config.** Node mutates `config` in place; Rust returns `serde_json::Value` so the function composes with the rest of pc-acpx's value-typed patterns. Tests verify `paperclipSkillSync` is added without disturbing other top-level keys.

10. **Snapshot sort order is stable.** Both `buildRuntimeMountedSkillSnapshot` and `buildPersistentSkillSnapshot` sort entries by `key` ascending, matching Node's `entries.sort((l, r) => l.key.localeCompare(r.key))`. Test `runtime_mounted_snapshot_canonical_state_matrix` verifies the order.

## Test inventory

### Unit tests added (`crates/pc-acpx/src/server_utils.rs::tests`)

25 new unit tests:

- `normalize_path_slashes_replaces_backslashes` — Windows → POSIX
- `is_maintainer_only_skill_target_detects_agents_skills_path` — `.agents/skills` path detection
- `skill_location_label_trims_and_returns_none` — label trim semantics
- `build_managed_skill_origin_returns_company_managed` — origin tuple
- `is_paperclip_skill_source_missing_handles_optional_status` — missing detection
- `resolve_paperclip_skill_missing_detail_falls_back_when_blank` — fallback string
- `resolve_skill_detail_picks_callback_over_literal` — string vs callback
- `resolve_installed_entry_target_resolves_symlink_to_absolute` — symlink resolution
- `expand_home_prefix_expands_tilde` — `~` / `~/x` / absolute / relative
- `resolve_paperclip_instance_root_for_adapter_builds_canonical_path` — caller args
- `resolve_paperclip_instance_root_for_adapter_falls_back_to_default_home` — no-arg fallback
- `resolve_paperclip_instance_root_reads_env_fallbacks` — env vars honored
- `read_paperclip_skill_sync_preference_returns_default_when_absent` — empty cfg
- `read_paperclip_skill_sync_preference_parses_string_and_object_entries` — mixed entries, dedup
- `write_paperclip_skill_sync_preference_emits_string_array_when_no_versions` — plain array
- `write_paperclip_skill_sync_preference_emits_object_array_when_versions_present` — object array (k1 → `{key, versionId: null}`)
- `canonicalize_resolves_key_runtime_name_and_slug` — all 3 resolution paths
- `resolve_paperclip_desired_skill_names_returns_empty_when_not_explicit` — non-explicit
- `resolve_paperclip_desired_skill_names_canonicalizes_and_dedups` — full canonicalization
- `build_runtime_mounted_skill_snapshot_marks_configured_when_desired` — `Configured` state
- `build_runtime_mounted_skill_snapshot_marks_available_when_not_desired` — `Available` state
- `build_runtime_mounted_skill_snapshot_warns_for_unavailable_desired` — warning + extra entry
- `build_persistent_skill_snapshot_marks_installed_when_target_matches_source` — `Installed`
- `build_persistent_skill_snapshot_marks_external_when_target_mismatch` — `External`
- `normalize_configured_paperclip_runtime_skills_filters_invalid_entries` — strict validation

### Integration tests (`crates/pc-acpx/tests/round407_server_utils_part3.rs`)

20 tests:

- `path_helpers_normalize_windows_paths_and_detect_maintainer_root` — full path helper matrix
- `skill_location_label_round_trips_through_trim` — trim semantics
- `expand_home_prefix_supports_tilde_and_absolute_paths` — 4 input shapes
- `resolve_instance_root_priority_chain` — caller > env > default chain
- `skill_sync_preference_round_trip_preserves_desired_keys` — read + write cycle
- `write_skill_sync_with_no_versions_emits_string_array` — plain array form
- `write_skill_sync_preserves_other_config_keys` — non-target keys untouched
- `resolve_desired_skill_names_canonicalizes_across_modes` — full canonicalization
- `resolve_desired_skill_names_returns_empty_when_not_explicit` — non-explicit short-circuit
- `source_missing_and_detail_fallback` — missing detection + detail fallback
- `resolve_skill_detail_literal_vs_callback` — string-or-callback resolution
- `resolve_installed_entry_target_symlink_resolves_relative_path` — symlink / directory
- `runtime_mounted_snapshot_canonical_state_matrix` — full state matrix
- `persistent_snapshot_marks_installed_external_stale_and_missing` — full state matrix
- `normalize_configured_runtime_skills_handles_alternate_field_names` — `name` fallback
- `normalize_configured_runtime_skills_handles_non_array` — defensive guard
- `canonicalize_handles_ambiguous_runtime_names_by_falling_through` — ambiguity → pass-through
- `canonicalize_unique_slug_match_wins` — slug-only match path
- `adapter_skill_snapshot_serializes_to_camelcase_wire_shape` — JSON wire format
- `skill_constants_match_node` — public constants

## Test counts

| Bucket | Pre-R407 | Post-R407 | Δ |
|--|--|--|--|
| `pc-acpx` lib tests | 793 | **818** | **+25** |
| `pc-acpx` integration test files | 39 | **40** | **+1** |
| `pc-acpx` `pub mod` count | 65 | 65 | 0 |
| Node source lines ported (cumulative, R396-R407) | ~8 500 | **~8 950** | **+~450** |

## Coverage gaps deferred to R408 (`server_utils` Part 4) + async runtime

- **Wake payload types** (`PaperclipWakePayload`, `PaperclipWakeIssue`, `PaperclipWakeExecutionPrincipal`, `PaperclipWakeTaskWatchdogLeaf`, `PaperclipWakeTaskWatchdogCapabilities`, `PaperclipWakeTaskWatchdogContext`) — R408
- **Wake payload normalizers** (`normalizePaperclipWakePayload`, `stringifyPaperclipWakePayload`, `isPaperclipRecoveryWakePayload`, `readPaperclipIssueWorkModeFromContext`, `isAssignmentShapedPaperclipWakeReason`) — R408
- **Wake prompt** (`selectPaperclipTaskMarkdown`, `renderPaperclipWakePrompt`) — R408
- **Prompt templates** (`DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE`, `WATCHDOG_DEFAULT_MANDATE`) — R408
- **Async skill helpers** (`resolvePaperclipSkillsDir`, `listPaperclipSkillEntries`, `readInstalledSkillTargets`, `readPaperclipRuntimeSkillEntries`, `readPaperclipSkillMarkdown`, `ensurePaperclipSkillSymlink`, `materializePaperclipSkillCopy`, `removeMaintainerOnlySkillSymlinks`) — deferred with full fs layer
