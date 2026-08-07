# R374 — `build_runtime` 顶层组装

## Goal

Port the top-level `buildRuntime` orchestrator from Node
`acpx-engine/execute.ts` line 1354. The Node function is ~700 lines and
mixes normalization, skill staging, codex-home seeding, Claude settings,
fingerprint hashing, MCP server resolution, the remote-sandbox staging
seam, and the two concurrent sandbox bridges. R374 implements the **pure
assembly path** (no async I/O) so the executor can wire `build_runtime`
into its control flow without re-implementing every helper.

## Module Added

`crates/pc-acpx/src/build_runtime.rs` — pure orchestrator that wires the
helpers in this crate into a single `PreparedRuntime`.

| Symbol | Mirrors |
|---|---|
| `AgentIdentity` | `{ id, companyId }` from `ctx.agent` |
| `WakeContext` | `taskId / wakeReason / wakeCommentId / approvalId / approvalStatus / linkedIssueIds` extraction |
| `WakeContext::apply_to_env` | `PAPERCLIP_TASK_ID` / `PAPERCLIP_WAKE_REASON` / `PAPERCLIP_APPROVAL_*` / `PAPERCLIP_LINKED_ISSUE_IDS` projection |
| `WorkspaceHints` | `context.paperclipWorkspaces` array filter |
| `build_paperclip_env` | `buildPaperclipEnv` (server-utils.ts) |
| `apply_paperclip_workspace_env` | `applyPaperclipWorkspaceEnv` (server-utils.ts) |
| `BuildRuntimeInput` | `input: { ctx, engine, deps, spanParent }` |
| `build_runtime` | Top-level assembly (pure subset) |

## `PreparedRuntime` Extension

| Field | Type | Notes |
|---|---|---|
| `staged_runtime` | `Option<PreparedStagedRuntime>` | Host + (optional) in-sandbox workspace dir |
| `remote_staging_env_delta` | `Option<BTreeMap<String, String>>` | Replayed on compatible resume |
| `remote_managed_home_teardown` | `Option<AsyncCallback>` | Per-run copy-back closure |
| `remote_staging_dispose` | `Option<AsyncCallback>` | One-time staged-temp cleanup |

New type `PreparedStagedRuntime` (with `local()` / `remote()` constructors)
plus 4 builder setters.

## `AsyncCallback` Extension

`crates/pc-acpx/src/cache_lifecycle.rs` — derived `Clone` and a manual
`Debug` impl (so the new `Option<AsyncCallback>` fields compose cleanly
with `#[derive(Debug, Clone)]` on `PreparedRuntime`). `Arc<dyn Fn>` is
already `Clone`; the manual `Debug` is the only addition.

## Assembly Order (R374 Pure Subset)

```
BuildRuntimeInput
   │
   ├─ normalize_agent / mode / permission / model / thinkingEffort / fastMode
   ├─ build_paperclip_env(agent, process_env)
   │     → PAPERCLIP_AGENT_ID, PAPERCLIP_COMPANY_ID, PAPERCLIP_API_URL
   ├─ env.insert("PAPERCLIP_RUN_ID", run_id)
   ├─ WakeContext::from_context(ctx).apply_to_env(env)
   │     → PAPERCLIP_TASK_ID, *_WAKE_*, *_APPROVAL_*, *_LINKED_ISSUE_IDS
   ├─ apply_paperclip_workspace_env(env, …)
   │     → PAPERCLIP_WORKSPACE_*, AGENT_HOME
   ├─ env.insert("PAPERCLIP_API_KEY", auth_token?) when non-empty
   ├─ if acpx_agent == "codex":
   │     build_codex_startup_config → env.CODEX_CONFIG
   ├─ if acpx_agent == "claude" and model non-empty:
   │     env.ANTHROPIC_MODEL ??= model
   ├─ state_dir = input.state_dir ?? default_state_dir(company, agent)
   ├─ TimeoutResolution { timeout_sec, source, note }
   ├─ fingerprint = short_hash(json!({…acpxAgent, cwd, mode, …}))
   ├─ session_key = paperclip:<co>:<agent>:<taskKey|ws|default>:<fingerprint>
   └─ PreparedRuntime { … }
```

## Async I/O Parts (Deferred to R375)

- `prepare_claude_skill_runtime` (writes skill copy to state dir)
- `prepare_codex_skill_runtime` (seeds managed home + reconciles skills)
- `prepare_gemini_skill_runtime` (copies gemini skills)
- `write_paperclip_claude_settings` (writes settings.json)
- `apply_codex_startup_config_to_disk` (writes config.toml)
- `stage_acp_remote_runtime` (ships workspace + managed home)
- `start_adapter_execution_target_paperclip_bridge`
- `start_adapter_execution_target_process_session_bridge`
- `run_acpx_engine_executor` (top-level executor that consumes PreparedRuntime)

These will be lifted into `build_runtime` (or a sibling
`create_acpx_engine_executor` factory) once the corresponding
`SubprocessAcpRuntime` integration lands.

## Tests

| File | Tests |
|---|---|
| `crates/pc-acpx/src/prepared_runtime.rs::staged_runtime_tests` | 4 (new) |
| `crates/pc-acpx/src/build_runtime.rs::tests` | 26 (new) |
| `crates/pc-acpx/tests/round374_build_runtime_top_level.rs` | 24 (new) |

Total: **54 new tests**, all green.

| pc-acpx | Before R374 | After R374 |
|---|---|---|
| Total tests | 303 | **357** |
| Modules | 31 | **32** (+ build_runtime) |
| Public types / re-exports | — | +5 (`BuildRuntimeInput`, `AgentIdentity`, `WakeContext`, `WorkspaceHints`, `PreparedStagedRuntime`) |

## Coverage of Node `buildRuntime`

| Concern | Node | Rust (R374) |
|---|---|---|
| normalize_agent / mode / permission / model / thinkingEffort | ✅ | ✅ |
| build_paperclip_env (API URL resolution) | ✅ | ✅ |
| apply_paperclip_workspace_env (9 keys) | ✅ | ✅ |
| wake / approval / linked-issue env vars | ✅ | ✅ |
| PAPERCLIP_API_KEY (auth token) | ✅ | ✅ |
| ANTHROPIC_MODEL pre-set for claude | ✅ | ✅ |
| CODEX_CONFIG merge via `build_codex_startup_config` | ✅ | ✅ |
| state_dir resolution (override + default) | ✅ | ✅ |
| TimeoutResolution (source + note) | ✅ | ✅ |
| fingerprint (short_hash over config inputs) | ✅ | ✅ |
| session_key (paperclip:co:agent:task:fingerprint) | ✅ | ✅ |
| MCP server normalization | ⚠️ skipped (caller passes resolved list) | ✅ (passthrough) |
| skill runtime prep (claude / codex / gemini) | ✅ | ❌ deferred (R375) |
| write_paperclip_claude_settings | ✅ | ❌ deferred (R375) |
| codex managed home seeding | ✅ | ❌ deferred (R375) |
| resolve_built_in_agent_command + shell | ✅ | ❌ deferred (R375) |
| gemini command shell normalize | ✅ | ❌ deferred (R375) |
| fingerprint MCP identity block | ✅ | ✅ |
| additionalSourcesIdentity (referenced projects) | ✅ | ❌ deferred (R375+ — sandbox-utils seam) |
| secretManifestHash | ✅ | ❌ deferred (R375+) |
| stagedRuntime + remote staging seam | ✅ | partial (data fields wired, no stage call) |
| paperclip / process-session bridges | ✅ | ❌ deferred (R375) |
| hostSpawnCwd | ✅ | ❌ deferred (R375) |
| agentRegistry | ✅ | ❌ deferred (R375) |
| skillPromptInstructions + skillsIdentity + commandNotes | ✅ | ❌ deferred (R375) |
| childStderrLogPath | ✅ | ❌ deferred (R375) |
| mcpServers / mcpIdentity | ✅ | partial (mcpServers passthrough; mcpIdentity skipped) |
| loggedEnv (secret redaction) | ✅ | partial (mirror of env, no redaction in R374) |
| childStderrLogPath | ✅ | ❌ deferred (R375) |

## Summary

R374 closes the gap between "every helper exists in Rust" and "the
helper graph wires into a single `build_runtime` function". The
remaining gap (skill prep, claude settings, codex home seeding, the
remote-sandbox staging seam, and the two concurrent bridges) belongs
to R375 (factory + executor wiring).

`build_runtime` is **pure and synchronous** by design: every async I/O
concern is left to the caller, which feeds the resolved values into
`BuildRuntimeInput`. This makes the function trivially testable and
lets the executor control the order of I/O steps without forking the
crate's API surface.
