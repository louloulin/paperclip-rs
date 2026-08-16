# R707 — pc-tool argument_condition (2026-08-16)

## 目标

补足 Node `services/tool-access-policy.ts::readPath` + `argumentFiltersMatch`。
这是 tool policy decision 中 trust rule 验证的核心 logic。

## 设计

- **新 submodule**: `crates/pc-tool/src/argument_condition.rs` (254 行)
- **新公开 API**:
  - `read_path(&Value, &str) -> Option<Value>` (Node `readPath` 1:1 parity)
  - `ArgumentFilters` struct (8 filter 类型)
  - `argument_filters_match(&ArgumentFilters, &str, &Value) -> bool`
- **关键设计**:
  - `read_path` 支持 dot-path + 数组索引 (`tags.0`)
  - 8 种 filter 全部支持: allowAny / exactHash / allowedHashes / fieldEquals /
    fieldNotEquals / fieldIn / fieldMatches (regex) / fieldExists / fieldAbsent
  - 空 filters → false (Node `Boolean(...)` 末尾检查)
  - serde `rename_all = "camelCase"` 镜像 Node wire format

## 算法 parity

### Node `readPath(value, path)`:
```js
function readPath(value, path) {
  if (!path) return undefined;
  return path.split(".").reduce((current, segment) => {
    if (!isRecord(current) && !Array.isArray(current)) return undefined;
    if (Array.isArray(current)) {
      const index = Number(segment);
      return Number.isInteger(index) ? current[index] : undefined;
    }
    return current[segment];
  }, value);
}
```js

### Rust `read_path`:
```rust
pub fn read_path(value: &Value, path: &str) -> Option<Value> {
    if path.is_empty() { return None; }
    let mut current = value.clone();
    for segment in path.split('.') {
        match &current {
            Value::Object(map) => match map.get(segment) { ... },
            Value::Array(arr) => match segment.parse::<usize>() { ... },
            _ => return None,
        }
    }
    Some(current)
}
```

### Node `argumentFiltersMatch` 7 filter 类型:
1. `allowAny === true` → true (short circuit)
2. `exactHash` → exact match
3. `allowedHashes.length && !includes(ctx.argumentsHash)` → false
4. `fieldEquals` → path 读取 + stableStringify 比较
5. `fieldNotEquals` → 反向
6. `fieldIn` → path 读取 + 包含检查
7. `fieldMatches` → path 读取 + regex 匹配 (invalid regex → false)
8. `fieldExists` → path 必须存在
9. `fieldAbsent` → path 必须不存在
10. 末尾 `Boolean(... || ...)` → 至少一个 filter 实际设了值

### Rust `argument_filters_match`:
完全镜像上述 10 条规则, regex crate 替代 new RegExp()。

## 测试

### argument_condition 模块
```
running 17 tests
test read_path_simple ... ok
test read_path_array_index ... ok
test read_path_missing ... ok
test read_path_empty_returns_none ... ok
test allow_any_short_circuits ... ok
test exact_hash_match ... ok
test allowed_hashes_includes ... ok
test field_equals_match ... ok
test field_not_equals ... ok
test field_in_match ... ok
test field_matches_regex ... ok
test field_matches_invalid_regex_fails ... ok
test field_exists ... ok
test field_absent ... ok
test no_filters_returns_false ... ok
test multiple_filters_all_must_match ... ok
test stable_stringify_handles_null ... ok

test result: ok. 17 passed; 0 failed
```

### pc-tool 全测
```
test result: ok. 82 passed; 0 failed
```

## 关键 parity 验证

- `read_path` - 完整 dot-path walk + array index 支持
- `argument_filters_match` - 8 filter 类型 1:1 parity
- 末尾 Boolean 检查 (空 filter → false) 一致
- serde `rename_all = "camelCase"` 镜像 Node wire format
- `regex` crate 处理 invalid regex (返回 false)

## R707 关键交付

- [x] argument_condition.rs 模块 + 17 个单测 PASS
- [x] Cargo.toml 新增 regex dependency
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `readPath` + `argumentFiltersMatch` 100% parity
- [x] pc-tool 全测 82 PASS (无 regression, +17 新测)

## 累计 R700-R707 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **R703**: pc-tool/connection_health (13 tests)
- **R704**: pc-tool/descriptor_hash (10 tests)
- **R705**: pc-execution-workspace-guards/runtime_service_id (11 tests)
- **R706**: pc-tool/selector_match (12 tests)
- **R707**: pc-tool/argument_condition (17 tests)
- **总计**: 94 个新单测 PASS, ~1500 行新增代码

## 下一步

- R708 — pc-tool side_effect_idempotency_key + audit_outcome
- R709 — pc-tool risk_rank (Node riskRank)
- R710 — pc-tool summarize_and_redact (Node summarizeAndRedact)

