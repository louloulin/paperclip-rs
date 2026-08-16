# R706 — pc-tool selector_match (2026-08-16)

## 目标

补足 Node `services/tool-access-policy.ts::selectorMatches`。
这是 tool policy decision 的核心 matching logic。

## 设计

- **新 submodule**: `crates/pc-tool/src/selector_match.rs` (275 行)
- **新公开 API**:
  - `ToolAccessContext` struct (13 字段, 与 Node 一致)
  - `ToolAccessSelector` struct (13 single + 13 many 字段)
  - `selector_matches(&ToolAccessSelector, &ToolAccessContext) -> bool`
- **关键设计**:
  - 单一字段 (e.g. agent_id) 精确匹配 — 单值或空都通过
  - 复数字段 (e.g. agent_ids) 包含检查 — 必须非空且 ctx 必须在集合中
  - `tool_name` 特殊: 匹配 toolName OR upstreamToolName 任一
  - 空 selector 匹配所有
  - serde `rename_all = "camelCase"` 镜像 Node wire format

## 算法 parity

### Node `selectorMatches`:
```js
function selectorMatches(selector, ctx) {
  if (!selector || Object.keys(selector).length === 0) return true;
  const match = (singleKey, pluralKey, actual) => {
    const single = typeof s[singleKey] === "string" ? String(s[singleKey]) : null;
    const many = listValues(s[pluralKey]);
    return (!single || actual === single) && (many.length === 0 || (actual && many.includes(actual)));
  };
  const matchAny = (singleKey, pluralKey, actuals) => {
    /* 特殊: tool_name 匹配 ctx.toolName OR ctx.upstreamToolName */
  };
  return match(...) && match(...) && ... ;
}
```js

### Rust `selector_matches`:
```rust
pub fn selector_matches(selector: &ToolAccessSelector, ctx: &ToolAccessContext) -> bool {
    if !match_single(&selector.actor_type, &selector.actor_types, &ctx.actor_type) { return false; }
    if !match_single(&selector.agent_id, &selector.agent_ids, &ctx.agent_id) { return false; }
    /* ... 11 more fields ... */
    if !match_any_tool_name(&selector.tool_name, &selector.tool_names, ctx) { return false; }
    if !match_single(&selector.risk_level, &selector.risk_levels, &ctx.risk_level) { return false; }
    true
}
```

## 测试

### selector_match 模块
```
running 12 tests
test empty_selector_matches_anything ... ok
test single_field_must_match ... ok
test single_mismatch_blocks ... ok
test many_field_includes ... ok
test many_empty_ctx_fails_when_many_set ... ok
test many_set_with_no_actual_value_blocks ... ok
test single_and_many_combined ... ok
test tool_name_match_any ... ok
test tool_names_many_any ... ok
test risk_level_match ... ok
test all_fields_combined ... ok
test selector_serde_camel_case ... ok

test result: ok. 12 passed; 0 failed
```

### pc-tool 全测
```
test result: ok. 65 passed; 0 failed
```

## 关键 parity 验证

- `selector_matches` - 13 字段 (单/复) AND 逻辑 1:1 parity
- `tool_name` 特殊: tool_name OR upstream_tool_name 任一即可
- 空 selector → true (匹配一切)
- many set + null actual → false (与 Node `(actual && many.includes(actual))` 一致)
- serde `rename_all = "camelCase"` 镜像 Node wire format (agentIds, toolNames 等)

## R706 关键交付

- [x] selector_match.rs 模块 + 12 个单测 PASS
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `selectorMatches` 100% parity
- [x] pc-tool 全测 65 PASS (无 regression)

## 累计 R700-R706 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **R703**: pc-tool/connection_health (13 tests)
- **R704**: pc-tool/descriptor_hash (10 tests)
- **R705**: pc-execution-workspace-guards/runtime_service_id (11 tests)
- **R706**: pc-tool/selector_match (12 tests)
- **总计**: 77 个新单测 PASS, ~1240 行新增代码

## 下一步

- R707 — pc-tool argument_condition_matches (Node argumentConditionMatches)
- R708 — pc-tool side_effect_idempotency_key
- R709 — pc-issues issue_thread_interactions pure helpers

