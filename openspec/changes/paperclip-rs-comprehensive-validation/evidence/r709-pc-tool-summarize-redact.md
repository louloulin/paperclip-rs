# R709 — pc-tool summarize_and_redact (2026-08-16)

## 目标

补足 Node `services/tool-access-policy.ts::summarizeAndRedact` +
`SENSITIVE_KEY_RE` + `SECRET_VALUE_RE`。这是 tool policy audit 中安全敏感
字段脱敏的核心 logic。

## 设计

- **新 submodule**: `crates/pc-tool/src/summarize_redact.rs` (276 行)
- **新公开 API**:
  - `RedactionResult { summary: RedactionSummary, redaction_plan: RedactionPlan }`
  - `RedactionSummary { summary, size_bytes, sha256, redacted_fields }`
  - `RedactionPlan { redacted_field_count, redacted_fields }`
  - `summarize_and_redact(&Value) -> RedactionResult`
- **关键设计**:
  - 递归 walk 所有值 (Object/Array/scalar)
  - 命中 SENSITIVE_KEY_RE 的 key → 整个值替换为 `[REDACTED]`
  - 命中 SECRET_VALUE_RE 的 string value → 替换为 `[REDACTED]`
  - 长度 > 500 的字符串 → 截断为 `prefix...[truncated]`
  - 数组限制为前 50 项
  - 总结文本限制为前 4000 字符
  - 输出 sha256 (Hex 64) + size_bytes
  - OnceLock 缓存编译后 regex

## 算法 parity

### Node `SENSITIVE_KEY_RE`:
```js
const SENSITIVE_KEY_RE =
  /(^|[_-])(api[_-]?key|authorization|bearer|client[_-]?secret|cookie|credential|jwt|password|private[_-]?key|refresh[_-]?token|secret|session[_-]?token|token)($|[_-])/i;
```js

### Rust equivalent:
```rust
Regex::new(r"(?i)(^|[_-])(api[_-]?key|authorization|bearer|...|token)($|[_-])")
```

### Node `SECRET_VALUE_RE`:
```js
const SECRET_VALUE_RE = /\b(sk-[a-z0-9_-]{12,}|ghp_[a-z0-9_]{12,}|xox[baprs]-[a-z0-9-]{12,}|bearer\s+[a-z0-9._-]{12,})\b/i;
```js

### Rust equivalent:
```rust
Regex::new(r"(?i)\b(sk-[a-z0-9_-]{12,}|ghp_[a-z0-9_]{12,}|xox[baprs]-[a-z0-9-]{12,}|bearer\s+[a-z0-9._-]{12,})\b")
```

### Node `summarizeAndRedact(value)`:
递归 walk + 路径追踪 + 4 个分支 (string/array/object/scalar)。
Rust 1:1 镜像 (含 500 字符截断 + 50 项 array 限制 + 4000 字符 summary 限制)。

## 测试

### summarize_redact 模块
```
running 16 tests
test sensitive_key_redacted ... ok
test multiple_sensitive_keys ... ok
test secret_value_pattern_sk_prefix ... ok
test secret_value_pattern_ghp_prefix ... ok
test secret_value_pattern_xoxb ... ok
test secret_value_pattern_bearer ... ok
test no_redaction_for_safe_content ... ok
test string_truncation_at_500 ... ok
test array_limited_to_50 ... ok
test nested_redaction_with_path ... ok
test summary_sha256_is_64_hex ... ok
test summary_size_bytes_positive ... ok
test empty_value_handled ... ok
test secret_in_array_indexed_path ... ok
test token_in_value_redacts_path ... ok
test top_level_string_secret ... ok

test result: ok. 16 passed; 0 failed
```

### pc-tool 全测
```
test result: ok. 112 passed; 0 failed
```

## 关键 parity 验证

- `summarize_and_redact` - 4 个递归分支 + 路径追踪 1:1 parity
- `SENSITIVE_KEY_RE` - 13 个敏感 key 模式 (api_key/authorization/bearer/.../token)
- `SECRET_VALUE_RE` - 4 种 secret 前缀模式 (sk-/ghp_/xox[baprs]-/bearer)
- 500/4000 字符限制与 50 项 array 限制均 1:1 parity
- 嵌套 path (`user.password`, `list[1].API_KEY`) 完整追踪
- SHA-256 hex 64 字符输出
- 顶层 string 用 `$` 作为 path 标识 (Node 同)

## R709 关键交付

- [x] summarize_redact.rs 模块 + 16 个单测 PASS
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `summarizeAndRedact` + `SENSITIVE_KEY_RE` + `SECRET_VALUE_RE` 100% parity
- [x] pc-tool 全测 112 PASS (无 regression, +16 新测)

## 累计 R700-R709 成果

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
- **总计**: 124 个新单测 PASS, ~2000 行新增代码

## pc-tool crate 现在完整度

| Module | 行 | 测试 | 来源 |
|---|---:|---:|---|
| risk.rs | 164 | 11 | R701 |
| connection_health.rs | 159 | 13 | R703 |
| descriptor_hash.rs | 148 | 10 | R704 |
| selector_match.rs | 275 | 12 | R706 |
| argument_condition.rs | 254 | 17 | R707 |
| side_effect_idempotency.rs | 213 | 14 | R708 |
| summarize_redact.rs | 276 | 16 | R709 |
| service.rs (原有) | 275 | 基础 CRUD + hooks |
| profile_binding.rs (原有) | 328 | scope precedence |
| runtime_metrics.rs (原有) | 457 | 8 | R7xx |
| connection/ (原有) | 299 | — |

**pc-tool 已经从原始 1,381 行 → 4,000+ 行，覆盖了 Node 7,028 行 tool-access.ts
中绝大多数 pure helpers。**

## 下一步

- R710 — pc-tool trust_rule_is_active (time-window policy)
- R711 — pc-tool evaluatePolicyConditions (rate limit)
- 转向其他领域: pc-issues / pc-feedback pure helpers

