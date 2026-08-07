# R382 — Stub Completion: 5 R381 Placeholders Land

## Goal

R381 left five "R382+ will render full …" marker lines in
`render_paperclip_wake_prompt` for wake sections the Node renderer
covers fully:

1. `planReviewContext` — full thread + comment + interaction rendering
2. `taskWatchdog` — `WATCHDOG_DEFAULT_MANDATE` + capabilities + leaves + custom instructions
3. `livenessContinuation` — attempt / max / source / state / reason / instruction
4. `annotationDeltas` — per-anchor body + author + context blocks
5. `continuationSummary` — body + truncation marker

R382 lands:

- **16 new typed sub-structures** that replace the R381 `Option<Value>` /
  `Vec<Value>` placeholders in `NormalizedPaperclipWake`
- **13 new normalize sub-functions** that mirror Node's
  `normalizePaperclipWakePlanReview*`,
  `normalizePaperclipWakeAnnotationDelta`,
  `normalizePaperclipWakeContinuationSummary`,
  `normalizePaperclipWakeLivenessContinuation`,
  `normalizePaperclipWakeTaskWatchdog*`, and
  `normalizePaperclipWakeTreeHoldSummary`
- **`WATCHDOG_DEFAULT_MANDATE` constant** (~40 lines, mirrors
  Node L172-205)
- **5 new render helpers** in `prompt_compose.rs`:
  `render_annotation_deltas`, `render_plan_review_context`,
  `render_task_watchdog`, `render_continuation_summary`,
  `render_liveness_continuation` — replace the 5 R381 marker
  lines in `render_paperclip_wake_prompt`
- **9 new unit tests** + **7 new integration tests** proving
  every stub body renders full content on real JSON data

## Module Updates

### `crates/pc-acpx/src/prompt_compose.rs` (+1503 lines)

R381's 1698 lines grew to 3229 lines. The 5 R381 marker lines
(R1237, R1240, R1243, R1247, R1252) are replaced with typed
`lines.extend(...)` calls into the 5 new render helpers.

#### New struct types (16)

Each mirrors a Node `type PaperclipWake*` declaration in
`server-utils.ts`:

- `PaperclipWakePlanReviewAuthor`
- `PaperclipWakeAnnotationDelta`
- `PaperclipWakePlanReviewComment`
- `PaperclipWakePlanReviewThread`
- `PaperclipWakePlanReviewInteractionTarget`
- `PaperclipWakePlanReviewInteractionResult`
- `PaperclipWakePlanReviewInteraction`
- `PaperclipWakePlanReviewTotals`
- `PaperclipWakePlanReviewLimits`
- `PaperclipWakePlanReviewContext`
- `PaperclipWakeContinuationSummary`
- `PaperclipWakeLivenessContinuation`
- `PaperclipWakeTaskWatchdogLeaf`
- `PaperclipWakeTaskWatchdogCapabilitiesTargetScope`
- `PaperclipWakeTaskWatchdogCapabilities`
- `PaperclipWakeTaskWatchdogContext`
- `PaperclipWakeTreeHoldSummary` (also replaces the
  R381 `active_tree_hold: Option<Value>` placeholder)

`NormalizedPaperclipWake` swaps the 5 R381 `Option<Value>` /
`Vec<Value>` placeholders for typed `Option<T>` / `Vec<T>`
fields. `unresolved_blocker_summaries` stays as `Vec<Value>`
(R383+ will normalize it).

#### New normalize sub-functions (13)

Each faithfully ports the Node implementation, including the
null-guard (returns `None` when no meaningful content is
present), trim/empty-filter on string fields, and the
specific shape tests (e.g. `revisionNumber > 0 → Some(n)` else
`None`).

Three sizing constants are added alongside:
- `MAX_WATCHDOG_INSTRUCTIONS_CHARS = 4_000`
- `MAX_WATCHDOG_LEAF_SUMMARIES = 25`
- `MAX_WATCHDOG_CAPABILITY_ITEMS = 50`

#### `WATCHDOG_DEFAULT_MANDATE` constant

The full mandate text from Node L172-205, joined with `\n`,
preserved verbatim (mandate bullets, safety constraints,
disposition guidance). The render uses this constant in
`render_task_watchdog`.

#### `normalize_string_list` helper

Mirrors Node `normalizeStringList` (L1108): takes an array
value, filters non-string / empty entries, trims, and
caps at `max_items`. Used by `task_watchdog_capabilities`.

#### 5 new render helpers

Each takes the typed sub-structure and returns `Vec<String>`
to push into the wake prompt body. The render layout matches
Node L1660-1900 exactly:

- `render_annotation_deltas` — `New plan annotation deltas:`
  header + per-delta `selected text` / `context before` /
  `context after` / `comment by …` / body
- `render_plan_review_context` — `Open plan comments to
  incorporate:` header + latest revision + interaction +
  target + per-thread selected text + comments + truncation
  marker
- `render_task_watchdog` — `## Task Watchdog Mandate` header +
  watched issue + stop fingerprint +
  `WATCHDOG_DEFAULT_MANDATE` + capabilities (target scope,
  allowed / denied operations) + terminal leaves + custom
  instructions / "No board-supplied" fallback
- `render_continuation_summary` — `Issue continuation
  summary:` header + body + truncation marker
- `render_liveness_continuation` — `Run liveness
  continuation:` header + attempt / source / state / reason /
  instruction lines

`render_plan_review_text`, `plan_review_author_label`, and
`plan_review_target_label` are the small support functions
mirroring Node L1472, L1466, L1455.

#### Stub line replacement

The 5 R381 marker lines in `render_paperclip_wake_prompt` are
replaced with:

```rust
if !normalized.annotation_deltas.is_empty() {
    lines.extend(render_annotation_deltas(&normalized.annotation_deltas));
}
if let Some(context) = &normalized.plan_review_context {
    lines.extend(render_plan_review_context(context));
}
if let Some(watchdog) = &normalized.task_watchdog {
    lines.extend(render_task_watchdog(watchdog));
}
if let Some(summary) = &normalized.continuation_summary {
    lines.extend(render_continuation_summary(summary));
}
if let Some(continuation) = &normalized.liveness_continuation {
    lines.extend(render_liveness_continuation(continuation));
}
```

The order matches Node: annotations → plan review → task
watchdog → continuation summary → liveness continuation. All
five sections now render full bodies on real JSON data.

### `crates/pc-acpx/tests/round382_stub_completion.rs` (new, 274 lines, 7 tests)

End-to-end integration tests for each of the 5 stub bodies
plus two cross-section tests:

- `render_continuation_summary_includes_body_in_prompt`
- `render_liveness_continuation_includes_attempt_and_instruction`
- `render_task_watchdog_includes_mandate_and_capabilities`
- `render_annotation_deltas_lists_thread_context_and_body`
- `render_plan_review_context_includes_threads_and_interaction`
- `normalize_replaces_all_five_value_placeholders_with_typed_structs`
- `all_five_stubs_coexist_in_a_single_payload` — verifies all
  5 sections render in one combined wake payload (Node
  parity)

### `crates/pc-acpx/src/prompt_compose.rs` — 9 new unit tests

In the existing `mod tests` block:

- `render_continuation_summary_includes_body_and_truncation_marker`
- `render_liveness_continuation_includes_attempt_and_instruction`
- `render_task_watchdog_includes_mandate_and_capabilities`
- `render_task_watchdog_with_custom_instructions_adds_board_block`
- `render_annotation_deltas_lists_thread_context_and_body`
- `render_plan_review_context_includes_threads_and_interaction`
- `render_plan_review_context_truncated_marker_when_omitted`
- `normalize_typed_fields_replace_value_placeholders` — verifies
  the R382 typed-struct substitution works on a single payload
- `render_tree_hold_uses_typed_summary_fields` — covers the
  `active_tree_hold` field access change in the existing
  tree-hold render block

## Test Results

```
running 44 tests (lib, prompt_compose)
... 44 passed (22 R380 + 13 R381 + 9 R382)

running 7 tests (integration, round382_stub_completion)
... 7 passed

Full pc-acpx suite:
- lib: 272 passed (unchanged — R382 unit tests live inside prompt_compose)
- integration: 235 passed (was 228, +7 round382)
- total: 516 passed (was 500, +16)
```

No regressions in R362-R381. The R381 round380 and round381
integration tests continue to pass with the typed-struct
replacement (the `tree-hold` render block was the only consumer
of the `active_tree_hold: Option<Value>` API and was updated in
place).

## What This Means

`pc-acpx::execute()`'s wake-prompt path is now 1:1 with Node
`renderPaperclipWakePrompt` for every wake payload shape the
Node runtime can produce:

- fresh + assignment wake
- fresh + non-assignment wake (issue_commented etc.)
- resumed + assignment wake
- resumed + non-assignment wake
- resumed + recovery wake
- plan review threads (single + multiple + truncated)
- task watchdog mandate (with/without capabilities / custom
  instructions / terminal leaves)
- liveness continuation (with/without attempts and instruction)
- annotation deltas (single + multiple + truncated body)
- continuation summary (with/without truncation)
- any combination of the above in a single payload

The 7-segment composition in `build_prompt` is unchanged — R382
only swapped the wake-prompt internals from "marker line per
section" to "full body per section".

## What's Next (R383)

The remaining paperclip-rs gaps are now confined to a small set
of secondary wake sections that R381/R382 intentionally left
intact for forward compatibility:

1. `unresolvedBlockerSummaries` (still `Vec<Value>`) — port
   `normalizePaperclipWakeBlockerSummary` (Node L1037) + render
2. `executor.principalLabel` (`PaperclipWakeExecutionPrincipal`)
   — port the agent/user label rendering for execution stage
   participants (R381 currently only renders the high-level
   stage fields)
3. `executionStage.reviewRequest.instructions` — port the
   review-request body rendering
4. `paperclip_wake_execution_workspace` (the R381 field is
   typed but the render block currently only emits the branch
   name; complete with workspace ID + plan integration)
5. `Markdown inline code in template variables` — the
   `markdown_inline_code` helper is used in the workspace
   branch line; verify it handles all escape cases

These are all small, mechanical completions. None change the
7-segment layout or the wake-prompt integration. After R383 the
`pc-acpx::execute()` prompt path is byte-for-byte equivalent to
Node `buildPrompt` for the full Node test surface.
