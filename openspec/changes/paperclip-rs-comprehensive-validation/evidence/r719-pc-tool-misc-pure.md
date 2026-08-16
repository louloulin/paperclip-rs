# R719 — pc-tool/src/misc_pure.rs

## 目标

补足 Node `services/tool-access.ts` 中零 DB pure helpers。

## 新增 helpers（9 个）

| Node 函数 | Rust 函数 |
|---|---|
| `schemaHasInputProperties` | `schema_has_input_properties(schema: Option<&Value>)` |
| `numberValue` | `number_value(value: Option<&Value>)` |
| `percent` | `percent(numerator, denominator)` |
| `percentile` | `percentile(values, p)` |
| `normalizeKey` | `normalize_key(input)` |
| `connectionUid` | `connection_uid(namespace, name, connection_id)` |
| `isToolConnectionForeignKeyViolation` | `is_tool_connection_foreign_key_violation(error)` |
| `oauthActorType` | `oauth_actor_type(value)` |
| `userFallbackName` | `user_fallback_name(user_id)` |
| `denialReasonForDecision` | `denial_reason_for_decision(decision)` |

## 测试结果

```
cargo test -p pc-tool --lib misc_pure
running 10 tests
...
test result: ok. 10 passed; 0 failed
```

## 关键设计

- `normalize_key` 严格按 Node：keep `[a-z0-9._:-]` → 其他转 `-` → trim dash → cap 160 chars → 默认 tool
- `percentile` 用 ceiling-based indexing（与 Node Math.ceil((p/100)*len)-1 一致）
- `is_tool_connection_foreign_key_violation` 走 4 层 cause 链，找 code=23503 + constraint/message 包含 tool_connections

## 累计

pc-tool crate 业务逻辑 pure helpers：137 PASS（原 127 + R719 新增 10）。
