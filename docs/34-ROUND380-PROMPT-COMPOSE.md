# R380 — Prompt Composition Helpers (prompt_compose)

## Goal

R376/R377/R378/R379 wired the executor entry point, result shape,
session options, and resume-retry / terminal cleanup. R380 lands the
**prompt-composition helpers** that back Node `buildPrompt` — the
largest of the remaining pure-function layers in the Node
`acpx-engine/execute.ts` surface. The actual integration of these
helpers into `execute()` is R381 (it will replace the current
`ctx.run_prompt` passthrough with a 7-segment composition). R380 ships:

1. `crates/pc-acpx/src/prompt_compose.rs` — 5 pure functions + 1
   constant, mirroring Node
   `renderTemplate` / `joinPromptSections` /
   `selectPaperclipTaskMarkdown` /
   `isAssignmentShapedPaperclipWakeReason` /
   `isPaperclipRecoveryWakePayload` from
   `adapter-utils/src/server-utils.ts`.
2. 22 unit tests in `prompt_compose.rs` (the `#[cfg(test)] mod tests`
   block) covering template grammar, section joiner, wake reason
   selection, and recovery detection.
3. 12 integration tests in
   `crates/pc-acpx/tests/round380_prompt_compose.rs` that compose the
   5 helpers into a `build_prompt_preview` mirroring Node `buildPrompt`
   7-segment layout and verify end-to-end behavior on real
   `serde_json::Value` data.

## Module Updates

### `crates/pc-acpx/src/lib.rs`

Re-exports added to the top-level `pub use` block:

```rust
pub use prompt_compose::{
    is_assignment_shaped_paperclip_wake_reason,
    is_paperclip_recovery_wake_payload,
    join_prompt_sections,
    join_prompt_sections_with_separator,
    render_template,
    select_paperclip_task_markdown,
    ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS,
    SelectTaskMarkdownOptions,
};
```

### `crates/pc-acpx/src/prompt_compose.rs` (new, 410 lines)

#### `render_template(template: &str, data: &Value) -> String`

Mirrors Node `renderTemplate`. Grammar: `{{ \s* path.segments \s* }}`
where `path` is `[a-zA-Z0-9_.-]+`. Whitespace around the path is
ignored. Resolution semantics match Node `resolvePathValue`:

- missing key or non-object/numeric path → empty string
- object → keep walking
- string → return as-is
- number / boolean → `String(value)`
- array / null → empty string
- other object → `serde_json::to_string` (best-effort)
- malformed placeholder (path grammar violated) → preserve the
  raw `{{...}}` verbatim so callers can detect malformed input

#### `join_prompt_sections(sections: &[Option<&str>]) -> String`

Default-separator wrapper around `join_prompt_sections_with_separator`
with separator `"\n\n"`. Mirrors Node `joinPromptSections`. Trims
non-null/non-empty sections, drops empty, **deduplicates while
preserving order**, and joins with the separator.

#### `select_paperclip_task_markdown(context, options) -> String`

Mirrors Node `selectPaperclipTaskMarkdown`. Returns the right
`paperclipTaskMarkdown` variant for the current run:

- no `paperclipTaskMarkdown` → empty string
- `resumedSession == false` → `paperclipTaskMarkdown` (full brief)
- `resumedSession == true` + assignment-shaped wake
  (`issue_assigned` / `issue_reopened_via_comment` /
  `issue_recovery_action_restored` / `issue_tree_restored`) → full
- `resumedSession == true` + recovery-shaped wake (`recovery` block
  present or `reason == "source_scoped_recovery_action"`) → full
- `resumedSession == true` + any other wake → compact
  (`paperclipTaskMarkdownCompact`, falling back to full when no
  compact was provided)

#### `is_assignment_shaped_paperclip_wake_reason(reason: Option<&str>) -> bool`

Mirrors Node `isAssignmentShapedPaperclipWakeReason`. Case-sensitive
membership check against `ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS`.
`None` and `Some("")` both return `false`.

#### `is_paperclip_recovery_wake_payload(value: Option<&Value>) -> bool`

Mirrors Node `isPaperclipRecoveryWakePayload`. Returns `true` when
the wake payload carries a non-null `recovery` block, or when the
top-level `reason == "source_scoped_recovery_action"`. `None` and
non-object payloads return `false`.

#### `ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS: &[&str]`

```rust
pub const ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS: &[&str] = &[
    "issue_assigned",
    "issue_reopened_via_comment",
    "issue_recovery_action_restored",
    "issue_tree_restored",
];
```

Exact match with Node `ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS` in
`server-utils.ts` (L1379). Asserted by the
`assignment_shaped_reasons_constant_lists_all_four_node_values` test.

#### `SelectTaskMarkdownOptions { resumed_session: bool }`

Builder struct for `select_paperclip_task_markdown` options. Mirrors
the Node options bag with the same field naming.

## Integration Tests

`crates/pc-acpx/tests/round380_prompt_compose.rs` (471 lines, 12 tests).

The integration test introduces a `build_prompt_preview` helper that
mirrors Node `buildPrompt` 7-segment composition **using only the
already-ported pure functions** plus
`render_paperclip_env_note` / `render_api_access_note` from
`session_config_options`. The wake-prompt rendering
(`renderPaperclipWakePrompt`, L1411) is **not yet ported** — R381
will port it. For R380, a deterministic `render_wake_prompt_placeholder`
emits a stable string when the wake would be injected and `""`
otherwise, so the 7-segment join behavior is fully testable without
the wake body.

Composition rule (mirrors Node L2246-2330):

1. `promptInstructionsPrefix` — the loaded `instructionsFilePath`
   contents, but **dropped** when `resumedSession && wakePrompt > ""`
   (the resume delta prompt replaces the heartbeat)
2. `renderedBootstrapPrompt` — `renderTemplate(bootstrapPromptTemplate, …)`
   only on fresh sessions with a non-empty template
3. `wakePrompt` — placeholder for now; R381 will port the full
   Node `renderPaperclipWakePrompt` body
4. `sessionHandoffNote` — `context.paperclipSessionHandoffMarkdown`
5. `taskContextNote` — `select_paperclip_task_markdown(context, …)`
6. `paperclipEnvNote` + `apiAccessNote` — joined as a single
   runtime note (mirrors `promptMetrics.runtimeNoteChars`)
7. `renderedPrompt` — `renderTemplate(promptTemplate, …)`, but
   **dropped** when `resumedSession && wakePrompt > ""`

The 12 integration tests cover:

- `fresh_session_with_assignment_wake_includes_all_seven_sections`
  — happy path: all 7 segments joined, full taskContext, wake
  injected, heartbeat template rendered
- `resumed_session_with_assignment_wake_replaces_prompt_template`
  — `shouldUseResumeDeltaPrompt = true`: heartbeat template
  omitted, instructions prefix dropped, full taskContext
- `resumed_session_with_non_assignment_wake_picks_compact_task_context`
  — `issue_commented` wake → compact variant wins
- `resumed_session_with_recovery_wake_picks_full_task_context`
  — `recovery` block present → full variant
- `missing_task_context_is_filtered_by_join_sections`
  — `join_prompt_sections` filters empty segments, no triple-newline
  gaps
- `malformed_template_keeps_placeholder_verbatim`
  — path with spaces → raw `{{…}}`; missing key → empty
- `assignment_shaped_reasons_constant_lists_all_four_node_values`
  — exact match with Node constant
- `wake_reason_helpers_match_node_behavior_on_real_json`
  — 7 real JSON shapes (4 assignment, 1 normal, 1 recovery, 1
  source-scoped)
- `template_data_dotted_paths_resolve_through_nested_objects`
  — `agent.companyId` style 3-level dotted access
- `render_template_coerces_booleans_and_numbers`
  — `true` → `"true"`, `7` → `"7"`, `null` → `""`
- `join_sections_dedupes_repeated_segments`
  — order-preserving dedup
- `build_prompt_metrics_match_node_field_naming`
  — `promptMetrics` field names + char counts identical to Node

## Test Results

```
running 22 tests (lib)
test prompt_compose::tests::* ... 22 passed

running 12 tests (integration)
test assignment_shaped_reasons_constant_lists_all_four_node_values ... ok
test build_prompt_metrics_match_node_field_naming ... ok
test fresh_session_with_assignment_wake_includes_all_seven_sections ... ok
test join_sections_dedupes_repeated_segments ... ok
test malformed_template_keeps_placeholder_verbatim ... ok
test missing_task_context_is_filtered_by_join_sections ... ok
test render_template_coerces_booleans_and_numbers ... ok
test resumed_session_with_assignment_wake_replaces_prompt_template ... ok
test resumed_session_with_non_assignment_wake_picks_compact_task_context ... ok
test resumed_session_with_recovery_wake_picks_full_task_context ... ok
test template_data_dotted_paths_resolve_through_nested_objects ... ok
test wake_reason_helpers_match_node_behavior_on_real_json ... ok

test result: ok. 12 passed; 0 failed
```

Full `pc-acpx` test counts after R380:

- **lib tests**: 251 (was 229 — added 22 prompt_compose unit tests)
- **integration tests**: 221 (was 209 — added 12 round380 tests)
- **total**: 472 (was 438)

No regressions in earlier R362-R379 suites.

## What's Next (R381)

1. Port `renderPaperclipWakePrompt` (Node L1411, ~85 lines) into a new
   `render_wake_prompt` pure function in `prompt_compose.rs`. The
   current `render_wake_prompt_placeholder` in the integration test
   will be replaced by calling the real port.
2. Move `build_prompt_preview` from the test file into
   `crates/pc-acpx/src/build_prompt.rs` (or a top-level `build_prompt`
   method on `AcpxEngineExecutor`) and call it from
   `acpx_engine_executor.rs` L743-746 in place of
   `text: ctx.run_prompt.clone()`. The wake-prompt body
   (`wakePrompt`), `instructionsFilePath` loading, bootstrap gating,
   and the `commandNotes` array will land in the same commit.
3. Verify the 7-segment composition still passes the existing 12
   integration tests with the real wake renderer in place.
4. Add 5-7 additional integration tests for the wake-prompt body:
   fresh session with non-assignment wake gets description copy;
   assignment wake gets the trigger copy; recovery wake includes
   the recovery cause; `suppressIssueDescription` honored when
   taskContext is present; `commandNotes` populated for
   `instructionsFilePath` load failures.

## Architecture Note

`prompt_compose` is a pure-function module with no I/O, no
`tokio::spawn`, no shared state. It is the third-largest pure layer
in `pc-acpx` after `session_codec` (R377) and `usage` (R378). The
high-cohesion property is preserved: every function in the module
operates on `(template, &Value) → String` or
`(Option<&Value>) → bool`, with no dependency on the executor,
runtime, or filesystem. R381's port of `renderPaperclipWakePrompt`
will land in this same file, keeping all prompt-composition logic
in a single module.
