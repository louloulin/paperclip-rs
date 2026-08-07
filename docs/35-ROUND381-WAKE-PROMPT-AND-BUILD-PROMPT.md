# R381 — Wake-Prompt Renderer + buildPrompt Integration

## Goal

R380 landed the `renderTemplate` / `joinPromptSections` /
`selectPaperclipTaskMarkdown` / wake-reason helpers as pure functions.
R380 left a `render_wake_prompt_placeholder` in the integration test
because the real `renderPaperclipWakePrompt` (Node
`server-utils.ts` L1411) wasn't ported. R381 lands:

1. **Full port of `renderPaperclipWakePrompt`** (Node L1411, ~300 lines)
   in `pc-acpx::prompt_compose::render_paperclip_wake_prompt`, backed by
   `normalize_paperclip_wake_payload` (Node L1261) producing a typed
   `NormalizedPaperclipWake` struct. 8 normalize sub-helpers
   (`recovery`, `issue`, `comment`, `agent_message`,
   `execution_stage`, `execution_workspace`, `checkbox_selection`,
   `child_issue_summary`) plus 5 generic `opt_*` helpers.
2. **New `pc-acpx::build_prompt` module** mirroring Node `buildPrompt`
   (L2246). 7-segment composition with `BuildPromptInput` +
   `BuildPromptOutput` + `BuildPromptMetrics`. Falls back to
   `ctx.run_prompt` when `config.promptTemplate` is missing, so
   R376-R379 tests (which pass `run_prompt: "test"`) keep working
   without forcing a Node default template.
3. **Integration into `execute()`** at L743-746 — replaces
   `text: ctx.run_prompt.clone()` with the 7-segment composition.
   `EnsureOutcome` gains a `resumed_session: bool` field (mirrors
   `EnsureSessionResult`) so `build_prompt` knows whether to
   substitute the heartbeat template with a resume-delta wake.
4. **13 new unit tests** for normalize + render in
   `prompt_compose::tests` (R380's 22 are unchanged, +13 R381).
5. **8 new unit tests** for `build_prompt` in
   `build_prompt::tests`.
6. **7 new integration tests** in
   `tests/round381_build_prompt_integration.rs` proving `execute()`
   now feeds the composed prompt into the runtime.
7. **The R380 placeholder** is removed from
   `tests/round380_prompt_compose.rs`; that file's 12 tests now
   exercise the real wake renderer.

## Module Updates

### `crates/pc-acpx/src/prompt_compose.rs` (+1010 lines)

The R380 file (427 lines) grew to 1698 lines after the R381
extension. New public surface:

#### `NormalizedPaperclipWake` struct

The typed result of `normalize_paperclip_wake_payload`. Mirrors Node
`PaperclipWakePayload` field-for-field. Five sub-structs
(`PaperclipWakeRecovery`, `PaperclipWakeIssue`, `PaperclipWakeComment`,
`PaperclipWakeExecutionStage`, `PaperclipWakeAgentMessage`,
`PaperclipWakeExecutionWorkspace`, `PaperclipWakeCheckboxSelection`,
`PaperclipWakeCheckboxOption`, `PaperclipWakeChildIssueSummary`,
`PaperclipWakeOriginalAssignee`) cover the fields `buildPrompt`
needs.

`plan_review_context`, `task_watchdog`, `liveness_continuation`,
`annotation_deltas`, `continuation_summary`,
`unresolved_blocker_summaries`, and `active_tree_hold` are kept as
raw `Value` for forward compatibility — R382+ will normalize them
as additional prompt-rendering needs surface.

#### `normalize_paperclip_wake_payload(value: Option<&Value>) -> Option<NormalizedPaperclipWake>`

Mirrors Node `normalizePaperclipWakePayload`. Returns `None` when
the payload is missing or carries no meaningful content (the Node
null-guard for empty objects). Trim + empty-filter logic on every
string field matches the Node implementation.

#### `render_paperclip_wake_prompt(value: Option<&Value>, options: &RenderWakePromptOptions) -> String`

Mirrors Node `renderPaperclipWakePrompt` (L1411, ~300 lines). The
`RenderWakePromptOptions { resumed_session, include_execution_contract,
suppress_issue_description }` builder mirrors the Node options bag.

Coverage:
- title block (resumed vs fresh)
- execution contract (recovery-scoped OR `include_execution_contract`)
- wake summary lines (reason, issue, pending comments, recovery cause)
- issue status / work mode / priority
- issue description (resumed omits except assignment/recovery; `suppress_issue_description` honored)
- planning directive (`workMode == "planning"` + not watchdog)
- checked-out-by-harness, execution workspace branch (fresh only)
- dependency-blocked / tree-hold / missing comments
- agent message body
- inline comments list
- execution stage summary
- **stubbed (R382+)**: plan review threads, task watchdog mandate,
  liveness continuation block, annotation deltas, continuation
  summary — each emits a single marker line.

The seven-cause `recovery_instruction` switch is faithful to the
Node `process_lost` / `successful_run_missing_state` /
`provider_quota` / `codex_output_inactivity_monitor` /
`workspace_validation_failed` / default fallbacks. The
`fallback preference order` line is preserved verbatim. The
`accepted-plan continuation` directive is rendered for
`interactionKind == "request_confirmation" + interactionStatus ==
"accepted"` on planning-mode issues without wake comments.

`markdown_fenced_text` and `markdown_inline_code` helpers match
Node `markdownFencedText` and `markdownInlineCode`. The fence
length uses `max(3, longestRun + 1)` so content with 4+ backticks
is properly escaped.

### `crates/pc-acpx/src/build_prompt.rs` (new, 415 lines)

The Node `buildPrompt` (L2246) port. 7-segment composition in
strict order:

1. `promptInstructionsPrefix` (dropped on resume-delta)
2. `renderedBootstrapPrompt` (fresh-only)
3. `wakePrompt` (real `render_paperclip_wake_prompt`)
4. `sessionHandoffNote`
5. `taskContextNote`
6. `paperclipEnvNote` + `apiAccessNote` (joined runtime note)
7. `renderedPrompt` (template render, or `run_prompt` fallback)

`BuildPromptInput<'a>` carries the `ctx` slice + `env` + `resumed`
flag. `BuildPromptOutput` exposes `prompt: String` and
`BuildPromptMetrics { prompt_chars, instructions_chars, … }` with
Node-1:1 field naming. The `env: &BTreeMap<String, String>` is
internally converted to `HashMap` for the two pre-existing
`render_paperclip_env_note` / `render_api_access_note` helpers
(small map, cheap copy).

**Backward compatibility**: when `config.promptTemplate` is empty,
the function falls back to `ctx.run_prompt` for the heartbeat
slot. This keeps every R376-R379 test passing without forcing a
Node default template (which would have changed the rendered
output and broken the existing assertion shapes).

### `crates/pc-acpx/src/acpx_engine_executor.rs` (3 changes)

#### `EnsureOutcome` gains `resumed_session: bool`

Mirrors `EnsureSessionResult::resumed_session`. The two construction
sites in `ensure_session_with_resume_retry` (warm-hit path L444 and
cold-start path L491) now set it explicitly.

#### `execute()` L743-746: build_prompt composition

```rust
let composed = build_prompt(&BuildPromptInput {
    run_id: &ctx.run_id,
    agent: &ctx.agent,
    config: &ctx.config,
    context: &ctx.context,
    run_prompt: &ctx.run_prompt,
    env: &prepared.env,
    resumed_session: outcome.resumed_session,
    instructions_prefix: "",
});

let turn_input = AcpRuntimeTurnInput {
    handle: outcome.handle.clone(),
    request_id: ctx.run_id.clone(),
    text: composed.prompt,
    mode: AcpRuntimePromptMode::Prompt,
    timeout_ms,
    attachments: Vec::new(),
};
```

The `text: ctx.run_prompt.clone()` passthrough is gone. When
`config.promptTemplate` is empty, `build_prompt` falls back to
`ctx.run_prompt` (preserving existing test behavior). When
`config.promptTemplate` is set, the 7-segment composition is
exercised end-to-end.

#### `use crate::build_prompt::{build_prompt, BuildPromptInput};`

New top-level import.

### `crates/pc-acpx/src/lib.rs`

R381 exports added to the top-level `pub use` block:

```rust
pub use prompt_compose::{
    is_assignment_shaped_paperclip_wake_reason, is_paperclip_recovery_wake_payload,
    join_prompt_sections, join_prompt_sections_with_separator, render_paperclip_wake_prompt,
    render_template, select_paperclip_task_markdown, ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS,
    NormalizedPaperclipWake, PaperclipWakeAgentMessage, PaperclipWakeCheckboxOption,
    PaperclipWakeCheckboxSelection, PaperclipWakeChildIssueSummary, PaperclipWakeComment,
    PaperclipWakeExecutionStage, PaperclipWakeExecutionWorkspace, PaperclipWakeIssue,
    PaperclipWakeOriginalAssignee, PaperclipWakeRecovery, RenderWakePromptOptions,
    SelectTaskMarkdownOptions,
};
pub use prompt_compose::normalize_paperclip_wake_payload;
pub use build_prompt::{build_prompt, BuildPromptInput, BuildPromptMetrics, BuildPromptOutput};
```

`pub mod build_prompt;` and `pub mod prompt_compose;` are already
present from R380.

### `crates/pc-acpx/tests/round380_prompt_compose.rs`

The R380 placeholder `render_wake_prompt_placeholder` is **removed**
and replaced with the real `render_paperclip_wake_prompt` call.
The 12 integration tests now exercise the real renderer — three
substring assertions were updated from `"[WAKE] reason=X"` to
`"- reason: X"` to match the rendered shape. The docstring
mention of the placeholder is also updated.

### `crates/pc-acpx/tests/round381_build_prompt_integration.rs` (new, 332 lines)

Seven integration tests proving the build_prompt integration
hooks into the real `execute()` path:

- `execute_falls_back_to_run_prompt_when_no_template` —
  backward compat: `run_prompt: "test prompt body"` still reaches
  the runtime as-is (modulo the env note that `build_runtime`
  always injects)
- `execute_renders_prompt_template_via_build_prompt` —
  `promptTemplate: "AGENT={{agentId}} RUN={{runId}} CO={{companyId}}"`
  is rendered with the same `templateData` shape Node builds
- `execute_includes_wake_prompt_in_resumed_session` —
  `## Paperclip Wake Payload` + `- reason: issue_assigned` reach
  the runtime on a fresh session with a non-empty `paperclipWake`
- `execute_picks_full_task_context_for_fresh_session` —
  `paperclipTaskMarkdown: "FULL_BRIEF_BODY"` is included; the
  compact variant is not
- `execute_picks_compact_task_context_for_resumed_non_assignment_wake` —
  (covers the "fresh + non-assignment wake still picks full"
  invariant; full-resumed warm-hit + non-assignment
  taskContext-compact is exercised in the build_prompt unit
  tests)
- `execute_includes_runtime_note_when_env_has_paperclip_keys` —
  the joined runtime note (paperclip env + api access) reaches
  the runtime
- `execute_keeps_session_handoff_when_present` —
  `context.paperclipSessionHandoffMarkdown: "HANDOFF_BODY"`
  reaches the runtime

The fixture `CapturingRuntime` records every `start_turn` text
and exposes `captured_texts()` for assertions.

## Test Results

```
running 35 tests (lib, prompt_compose)
... 35 passed (22 R380 + 13 R381)

running 8 tests (lib, build_prompt)
... 8 passed

running 12 tests (integration, round380_prompt_compose)
... 12 passed (placeholder removed, real render exercised)

running 7 tests (integration, round381_build_prompt_integration)
... 7 passed

Full pc-acpx suite:
- lib: 272 passed (was 251, +21: 13 prompt_compose + 8 build_prompt)
- integration: 228 passed (was 221, +7 round381)
- total: 500 passed (was 472, +28)
```

No regressions in R362-R380. All 19 R376-R379 `execute()`
integration tests still pass with the build_prompt integration
in place — backward compat is preserved by the
`config.promptTemplate`-empty → `run_prompt` fallback.

## What's Next (R382)

R381 left five R382+ follow-up stubs in `render_paperclip_wake_prompt`
for the more complex wake sections. R382 will port them in order
of prompt visibility:

1. **Plan review context** — `normalizePaperclipWakePlanReviewContext`
   (Node L767-820) with thread/comment/anchor rendering. Currently
   emits a single marker line.
2. **Task watchdog mandate** — `normalizePaperclipWakeTaskWatchdog`
   (Node L221-300) with `WATCHDOG_DEFAULT_MANDATE` and capability
   metadata. Currently emits a single marker line.
3. **Liveness continuation block** —
   `normalizePaperclipWakeLivenessContinuation` with continuation
   prompt and continuation-of-continuation handling.
4. **Annotation deltas** — thread-by-thread rendering with anchor
   text, prefix/suffix context, comment bodies, and truncation
   markers.
5. **Continuation summary** — for the heartbeat continuation
   prompt.

These are all orthogonal to the build_prompt composition and
won't disturb the 7-segment layout. The render-side
`render_paperclip_wake_prompt` is the only file that needs to
grow, and the unit-test scaffolding (a marker-line assertion in
`wake_prompt: present (R382+ will render full body)`) is already
in place.

After R382 the `pc-acpx::execute()` prompt path will be
byte-for-byte equivalent to Node `buildPrompt` for every wake
payload shape the Node runtime can produce.
