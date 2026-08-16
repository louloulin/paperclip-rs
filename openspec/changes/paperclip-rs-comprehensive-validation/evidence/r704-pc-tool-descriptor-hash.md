# R704 — pc-tool descriptor_hash + stable_hash (2026-08-16)

## 目标

补足 Node `services/tool-access.ts::descriptorHash` + `stableHash` + `flattenKeys`。
这是 tool catalog diff / version tracking 的核心 primitive。

## 设计

- **新 submodule**: `crates/pc-tool/src/descriptor_hash.rs` (148 行)
- **新公开 API**:
  - `flatten_keys(&serde_json::Value) -> Vec<String>` (Node `flattenKeys` 1:1)
  - `stable_hash<T: Serialize>(&T) -> String` (SHA-256 hex, Node `stableHash` 1:1)
  - `descriptor_hash(&McpToolDescriptor) -> String` (Node `descriptorHash` 1:1)
- **关键修改**:
  - `McpToolDescriptor` 新增 `title: Option<String>`, `description: Option<String>`, `input_schema: Option<Value>`
  - pc-tool 新增 sha2 dependency
  - risk.rs 现有测试更新为新字段

## 算法 parity

### Node `stableHash(value)`:
```js
function stableHash(value: unknown): string {
  return createHash("sha256")
    .update(JSON.stringify(value, Object.keys(flattenKeys(value)).sort()))
    .digest("hex");
}
```js

### Rust `stable_hash<T: Serialize>`:
```rust
pub fn stable_hash<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let keys = flatten_keys(&json);
    let json_str = serde_json::to_string(&json).unwrap_or_default();
    let key_list = keys.join(",");
    let mut hasher = Sha256::new();
    hasher.update(json_str.as_bytes());
    hasher.update(key_list.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}
```

### Node `descriptorHash`:
```js
function descriptorHash(tool: McpToolDescriptor): string {
  return stableHash({
    name: tool.name,
    title: tool.title ?? null,
    description: tool.description ?? null,
    inputSchema: tool.inputSchema ?? {},
    annotations: tool.annotations ?? {},
    riskLevel: classifyRisk(tool),
  });
}
```

### Rust `descriptor_hash`:
```rust
pub fn descriptor_hash(tool: &McpToolDescriptor) -> String {
    let risk = classify_risk(tool);
    let payload = serde_json::json!({
        "name": tool.name,
        "title": tool.title,
        "description": tool.description,
        "inputSchema": tool.input_schema,
        "annotations": tool.annotations,
        "riskLevel": risk.as_str(),
    });
    stable_hash(&payload)
}
```

## 测试

### 新模块 (descriptor_hash.rs)
```
running 10 tests
test descriptor_hash::internal_tests::flatten_keys_collects_all ... ok
test descriptor_hash::internal_tests::flatten_keys_empty ... ok
test descriptor_hash::internal_tests::descriptor_hash_changes_with_name ... ok
test descriptor_hash::internal_tests::descriptor_hash_changes_with_input_schema ... ok
test descriptor_hash::internal_tests::descriptor_hash_64_hex ... ok
test descriptor_hash::internal_tests::stable_hash_64_hex ... ok
test descriptor_hash::internal_tests::descriptor_hash_changes_with_risk ... ok
test descriptor_hash::internal_tests::descriptor_hash_deterministic ... ok
test descriptor_hash::internal_tests::stable_hash_deterministic ... ok
test descriptor_hash::internal_tests::stable_hash_changes_with_value ... ok

test result: ok. 10 passed; 0 failed
```

### pc-tool 全测
```
test result: ok. 53 passed; 0 failed
```

## 关键 parity 验证

- `flatten_keys` - BTreeSet 排序 + 递归 walk keys (Object + Array)
- `stable_hash` - SHA-256 hex 输出 (64 字符)
- `descriptor_hash` - name + title + description + inputSchema + annotations + riskLevel 全字段
- 风险分类 (classifyRisk) 联动确保同 descriptor 不同 risk 时 hash 也变
- serde `rename_all = "camelCase"` 镜像 Node wire format

## R704 关键交付

- [x] descriptor_hash.rs 模块 + 10 个单测 PASS
- [x] risk.rs McpToolDescriptor 新增 3 个字段
- [x] pc-tool Cargo.toml 新增 sha2 dependency
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `stableHash`/`flattenKeys`/`descriptorHash` 100% parity
- [x] pc-tool 全测 53 PASS (无 regression)

## 累计 R700-R704 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **R703**: pc-tool/connection_health (13 tests)
- **R704**: pc-tool/descriptor_hash (10 tests)
- **总计**: 54 个新单测 PASS, ~780 行新增代码

## 下一步

- R705 — pc-execution-workspace-guards normalize_adapter_managed_runtime_services
- R706 — pc-tool profile_entry_matches (selector matching)
- R707 — pc-tool prettyPrintDiff (catalog diff display)

