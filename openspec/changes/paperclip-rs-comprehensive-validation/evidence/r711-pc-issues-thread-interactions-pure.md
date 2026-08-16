# R711 — pc-issues/thread_interactions/pure (2026-08-16)

## 目标

补足 Node `services/issue-thread-interactions.ts` 6 个核心 pure helpers。
此模块是 issue thread interaction (confirm/checkbox/suggest tasks 等)
业务层的核心 utility。

## 设计

- **新 submodule**: `crates/pc-issues/src/thread_interactions/pure.rs` (192 行)
- **新公开 API**:
  - `InteractionActorKind` enum (Agent / User / System)
  - `InteractionKind` enum (RequestConfirmation / RequestCheckboxConfirmation /
    RequestItemVerdicts / AskUserQuestions / SuggestTasks / Other) + `from_str`
  - `is_terminal_issue_status(&str) -> bool` (Node `isTerminalIssueStatus`)
  - `non_negative_integer(f64) -> u32` (Node `nonNegativeInteger`)
  - `resolve_actor_kind(Option<&str>, Option<&str>) -> InteractionActorKind` (Node `resolveActorKind`)
  - `resolve_creator_kind(Option<&str>, Option<&str>) -> Option<InteractionActorKind>` (Node `resolveCreatorKind`)
  - `derive_target_type(InteractionKind, Option<&str>) -> String` (Node `deriveTargetType`)
  - `should_supersede_interaction_on_user_comment(bool) -> bool` (Node `shouldSupersedeInteractionOnUserComment`)

## 算法 parity

### Node `resolveActorKind`:
```js
function resolveActorKind(interaction) {
  if (interaction.resolvedByAgentId) return "agent";
  if (interaction.resolvedByUserId) return "user";
  return "system";
}
```js

### Rust `resolve_actor_kind`: 1:1 parity (agent 优先于 user)。

### Node `resolveCreatorKind`:
```js
function resolveCreatorKind(interaction) {
  if (interaction.createdByAgentId) return "agent";
  if (interaction.createdByUserId) return "user";
  return undefined;
}
```js

### Rust `resolve_creator_kind`: 1:1 parity (system 时返回 None 而非 system)。

### Node `deriveTargetType`:
```js
switch (interaction.kind) {
  case "request_confirmation":
  case "request_checkbox_confirmation":
  case "request_item_verdicts":
    return interaction.payload.target?.type ?? "none";
  default:
    return "none";
}
```js

### Rust `derive_target_type`: 1:1 parity.

### Node `nonNegativeInteger(value)`:
```js
function nonNegativeInteger(value) {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.trunc(value));
}
```js

### Rust `non_negative_integer`:
```rust
pub fn non_negative_integer(value: f64) -> u32 {
    if !value.is_finite() { return 0; }
    let truncated = value.trunc() as i64;
    if truncated < 0 { 0 } else { truncated as u32 }
}
```

## 测试

### pure 模块
```
running 18 tests
test terminal_status_done ... ok
test terminal_status_cancelled ... ok
test terminal_status_open_not_terminal ... ok
test non_negative_integer_basic ... ok
test non_negative_integer_handles_nan_and_inf ... ok
test resolve_actor_kind_agent ... ok
test resolve_actor_kind_user ... ok
test resolve_actor_kind_agent_priority ... ok
test resolve_actor_kind_system_fallback ... ok
test resolve_creator_kind_returns_none_for_system ... ok
test resolve_creator_kind_agent ... ok
test resolve_creator_kind_user ... ok
test derive_target_type_request_confirmation_with_target ... ok
test derive_target_type_request_confirmation_no_target ... ok
test derive_target_type_other_kinds_return_none ... ok
test should_supersede_basic ... ok
test interaction_kind_parsing ... ok
test actor_kind_serde ... ok

test result: ok. 18 passed; 0 failed
```

### pc-issues 全测
```
test result: ok. 115 passed; 0 failed
```

## 关键 parity 验证

- `is_terminal_issue_status` - 仅 "done" 和 "cancelled" 为 terminal (与 Node 一致)
- `non_negative_integer` - 处理 NaN/Infinity + 截断 + 钳制到非负
- `resolve_actor_kind` - agent 优先于 user, 无则 system
- `resolve_creator_kind` - agent 优先于 user, 无则 None (与 Node `undefined)`)
- `derive_target_type` - 3 种 confirmation kind 走 target, 其他 none
- `should_supersede` - 简单 bool
- serde `rename_all = "snake_case"` 镜像 Node wire format

## R711 关键交付

- [x] pure.rs 模块 + 18 个单测 PASS
- [x] mod.rs 接入
- [x] Node 6 个 helpers 100% parity
- [x] pc-issues 全测 115 PASS (无 regression, +18 新测)

## 累计 R700-R711 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **R703**: pc-tool/connection_health (13 tests)
- **R704**: pc-tool/descriptor_hash (10 tests)
- **R705**: pc-execution-workspace-guards/runtime_service_id (11 tests)
- **R706**: pc-tool/selector_match (12 tests)
- **R707**: pc-tool/argument_condition (17 tests)
- **R708**: pc-tool/side_effect_idempotency (14 tests)
- **R709**: pc-tool/summarize_redact (16 tests)
- **R710**: pc-tool/policy_validation (15 tests)
- **R711**: pc-issues/thread_interactions/pure (18 tests)
- **总计**: 157 个新单测 PASS, ~2400 行新增代码

## 下一步

- R712 — pc-feedback pure helpers (Node feedback.ts + feedback-redaction.ts)
- R713 — pc-companies pure helpers
- R714 — pc-routines pure helpers

