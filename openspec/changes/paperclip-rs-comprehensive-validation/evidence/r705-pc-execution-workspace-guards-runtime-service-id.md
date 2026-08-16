# R705 — pc-execution-workspace-guards runtime_service_id (2026-08-16)

## 目标

补足 Node `workspace-runtime.ts::stableStringify` + `stableRuntimeServiceId`。
这是 adapter 上报 runtime service 时生成稳定 ID 的核心 primitive。

## 设计

- **新 submodule**: `crates/pc-execution-workspace-guards/src/runtime_service_id.rs` (193 行)
- **新公开 API**:
  - `RuntimeServiceScope` enum (Run / ProjectWorkspace / ExecutionWorkspace / Agent)
  - `stable_stringify<T: Serialize>(&T) -> String` (Node `stableStringify` 1:1 parity)
  - `RuntimeServiceIdInput` struct (8 字段 input)
  - `stable_runtime_service_id(&RuntimeServiceIdInput) -> String`
- **关键修改**:
  - pc-execution-workspace-guards 新增 `serde`, `serde_json`, `sha2` 依赖

## 算法 parity

### Node `stableStringify(value)`:
```js
function stableStringify(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(",")}]`;
  if (value && typeof value === "object") {
    const rec = value;
    return `{${Object.keys(rec).sort().map(key => `${JSON.stringify(key)}:${stableStringify(rec[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
```js

### Rust `stable_stringify<T>`:
```rust
pub fn stable_stringify<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    stringify_value(&json)
}
fn stringify_value(value: &serde_json::Value) -> String {
    match value {
        Array(arr) => format!("[{}]", arr.iter().map(stringify_value).collect::<Vec<_>>().join(",")),
        Object(map) => { let mut keys: Vec<&String> = map.keys().collect(); keys.sort(); ... },
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
    }
}
```

### Node `stableRuntimeServiceId`:
```js
function stableRuntimeServiceId(input) {
  if (input.reportId) return input.reportId;
  const digest = createHash("sha256")
    .update(stableStringify({...}))
    .digest("hex").slice(0, 32);
  return `${input.adapterType}-${digest}`;
}
```js

### Rust `stable_runtime_service_id`:
```rust
pub fn stable_runtime_service_id(input: &RuntimeServiceIdInput) -> String {
    if let Some(ref id) = input.report_id { return id.clone(); }
    let s = stable_stringify(&payload);
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    let hex = format!("{:x}", result);
    let truncated = hex.chars().take(32).collect::<String>();
    format!("{}-{}", input.adapter_type, truncated)
}
```

## 测试

### runtime_service_id 模块
```
running 11 tests
test stable_stringify_primitive ... ok
test stable_stringify_array ... ok
test stable_stringify_object_sorts_keys ... ok
test stable_stringify_nested ... ok
test stable_stringify_empty ... ok
test stable_id_uses_report_id_when_present ... ok
test stable_id_deterministic ... ok
test stable_id_format ... ok
test stable_id_changes_with_scope ... ok
test stable_id_changes_with_service_name ... ok
test scope_as_str_matches_node ... ok

test result: ok. 11 passed; 0 failed
```

### pc-execution-workspace-guards 全测
```
test result: ok. 38 passed; 0 failed
```

## 关键 parity 验证

- `stable_stringify` - 数组递归 + 对象 key 排序 + 原始值 JSON.stringify
- `stable_runtime_service_id` - 优先使用 report_id, 否则 SHA-256 hash 32 hex chars + adapterType 前缀
- `RuntimeServiceScope` 4 枚举值与 Node RuntimeServiceRef.scopeType 一致
- serde json roundtrip 保证类型一致性

## R705 关键交付

- [x] runtime_service_id.rs 模块 + 11 个单测 PASS
- [x] Cargo.toml 新增 serde/serde_json/sha2
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `stableStringify`/`stableRuntimeServiceId` 100% parity
- [x] pc-execution-workspace-guards 全测 38 PASS (无 regression)

## 累计 R700-R705 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **R703**: pc-tool/connection_health (13 tests)
- **R704**: pc-tool/descriptor_hash (10 tests)
- **R705**: pc-execution-workspace-guards/runtime_service_id (11 tests)
- **总计**: 65 个新单测 PASS, ~960 行新增代码

## 下一步

- R706 — pc-tool selector_match (Node profileEntryMatches + targetMatches)
- R707 — pc-tool argument_condition_matches
- 进一步推进 workspace runtime business logic

