# R564 — R-INTEGRATION-4: pc-adapter-type ↔ 各 adapter 集成验证（2026-08-11）

## 1. 动机

`pc-adapter-type`（R555, 130 LOC）独立 crate 包含 `KNOWN_BUILTIN_ADAPTER_TYPES` + `normalize_*` + `validate_*` + `is_builtin_adapter_type`。

但**实际使用情况复杂**：
- 11 个 `pc-adapter-*` crate 每个都定义 `pub const ADAPTER_TYPE: &str = "..."`
- 这些 adapter 用 `_` 格式（"claude_local", "codex_local"）
- 但 pc-adapter-type 的 `KNOWN_BUILTIN_ADAPTER_TYPES` **错用了 `-` 格式**（"claude-local", "codex-local"）

→ 真实的跨 crate 不一致 bug：`is_builtin_adapter_type(ADAPTER_TYPE)` 对所有 11 个 adapter 都返回 false！

## 2. 修复

### 2.1 修复 `KNOWN_BUILTIN_ADAPTER_TYPES`（13 个字符串 hyphen → underscore）
对照 Node 上游 `AGENT_ADAPTER_TYPES`：
```typescript
export const AGENT_ADAPTER_TYPES = [
  "process", "http", "claude_local", "codex_local", "cursor_cloud",
  "gemini_local", "grok_local", "hermes_gateway", "hermes_local",
  "opencode_local", "pi_local", "cursor", "openclaw_gateway",
] as const;
```
Node 用 `_` —— 与所有 adapter crate 一致。

### 2.2 增强 `normalize_agent_adapter_type`（真 normalize）
原版只 trim。现在也转换 `-` → `_`，让调用方传 "claude-local" 与 "claude_local" 都得到同一个 canonical 值。

```rust
pub fn normalize_agent_adapter_type(raw: Option<&str>) -> String {
    let trimmed = raw.map_or("", str::trim);
    if trimmed.is_empty() {
        return DEFAULT_AGENT_ADAPTER_TYPE.to_string();
    }
    trimmed.replace('-', "_")  // ← NEW: normalize convention
}
```

### 2.3 修 4 处现有 tests（hyphen → underscore）

## 3. 新增 R564 集成测试（7 个）

`crates/pc-adapter-type/tests/r564_integration.rs`：

| 测试 | 验证内容 |
|---|---|
| `all_adapter_types_are_non_empty` | 11 个 ADAPTER_TYPE 全部非空 + 非 whitespace |
| `all_adapter_types_use_underscore_convention` | 全部不含 `-`（约定一致性） |
| `all_adapter_types_are_recognized_as_builtin` | `is_builtin_adapter_type` 对所有 11 个 ADAPTER_TYPE 返回 true（**核心一致性验证**） |
| `known_builtin_adapter_types_matches_all_adapters` | 每个 KNOWN_BUILTIN_ADAPTER_TYPES 条目（除 process）都有对应 adapter crate |
| `all_adapter_types_are_unique` | 11 个 ADAPTER_TYPE 之间无重复 |
| `all_adapter_types_recognized_by_canonical_list` | 编译期保证所有 ADAPTER_TYPE 都在 KNOWN_BUILTIN_ADAPTER_TYPES 内 |
| `normalize_accepts_both_conventions` | "claude-local" 与 "claude_local" 都被 normalize 成 "claude_local" |

## 4. 编译期断言

集成测试在顶部 `use pc_adapter_claude_local::ADAPTER_TYPE as CLAUDE_LOCAL;` 等 11 个 import —— 编译时就强制所有 11 个 adapter crate 都暴露 `pub const ADAPTER_TYPE: &str`。任何新增 adapter 但忘了定义这个常量，编译就会失败。

## 5. 验证结果

### 5.1 pc-adapter-type 全测试集
```
running 8 tests   (unit)
running 13 tests  (r555_adapter_type)
running 7 tests   (r564_integration) ← NEW
→ 28 passed / 0 failed
```

### 5.2 clippy
```
cargo clippy -p pc-adapter-type --lib
  → 0 warnings ✅
```

### 5.3 修复前后对比
| 检查 | 修复前 | 修复后 |
|---|---|---|
| `is_builtin_adapter_type("claude_local")` | false ❌ | true ✅ |
| `is_builtin_adapter_type("claude-local")` | true（侥幸） | true（先 normalize） |
| 11 个 adapter 全部 recognized | 0/11 | 11/11 |

## 6. 累计成果（R564 末 / R-INTEGRATION-4）

- **修复跨 crate 一致性 bug**：`KNOWN_BUILTIN_ADAPTER_TYPES` 13 个字符串格式错误（hyphen → underscore）
- **增强 normalize 函数**：从"只 trim" → "trim + normalize format"
- **新 7 个集成测试** 验证 11 个 adapter 与 canonical list 一致
- **编译期断言** 强制所有 11 个 adapter crate 暴露 `ADAPTER_TYPE` 常量
- pc-adapter-type 单元测试 8 + 集成测试 13 + R564 集成测试 7 = **28 tests passing**
- clippy 0 warnings

## 7. R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | pc-pipeline-case-type → pc-pipelines | ✅ R563 |
| 4 | **pc-adapter-type → 各 adapter crate** | ✅ **R564** |
| 5 | pc-portability-fidelity → pc-portability | 待做 |
| 6 | pc-execution-workspace-guards → pc-issues/execution | 待做 |
| 7 | pc-external-objects → pc-issue-references | 待做 |
| 8 | pc-app-definitions → pc-http route generation | 待做 |
| 9 | pc-trust-policy → pc-authz | 待做 |
| 10 | pc-workspace-commands → pc-cli | 待做 |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**R-INTEGRATION-1 + 2 + 3 + 4 完成**：4/12 = 33%

## 8. 下一步

- **R565**: R-INTEGRATION-5 — pc-portability-fidelity → pc-portability 验证
- **R566**: R-INTEGRATION-6 — pc-execution-workspace-guards → pc-issues execution 验证
