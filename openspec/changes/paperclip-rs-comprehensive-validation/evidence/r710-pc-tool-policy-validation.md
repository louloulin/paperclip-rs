# R710 — pc-tool policy_validation (2026-08-16)

## 目标

补足 Node `services/tool-access-policy.ts` 3 个 helpers:
- `isoDateOrNull(value)` - 解析 ISO date
- `trustRuleIsActive(policy, now)` - trust rule 时间窗口检查
- `rateLimitRule(policy)` - rate limit 配置提取

## 设计

- **新 submodule**: `crates/pc-tool/src/policy_validation.rs` (213 行)
- **新公开 API**:
  - `iso_date_or_null(&Value) -> Option<String>`
  - `TrustRuleConfig` struct (7 字段, serde camelCase)
  - `trust_rule_config(&Value) -> Option<TrustRuleConfig>`
  - `trust_rule_is_active(&TrustRuleConfig, DateTime<Utc>) -> bool`
  - `RateLimitRule` struct (3 字段, serde camelCase)
  - `rate_limit_rule(&Value) -> Option<RateLimitRule>`
- **关键设计**:
  - chrono crate 处理 RFC3339 date parsing
  - revoked_at 优先级 > expires_at (revoked 立即 inactive)
  - expires_at 解析失败时按 Node 行为 (Number.isNaN → false 但 continue)
  - rate_limit 验证: limit/windowSeconds 必须 > 0
  - key_by 字段过滤只保留 string 类型

## 算法 parity

### Node `trustRuleIsActive`:
```js
function trustRuleIsActive(policy, now = new Date()) {
  const rule = trustRuleConfig(policy);
  if (!rule) return false;
  if (rule.revokedAt) return false;
  if (rule.expiresAt) {
    const expiresAt = new Date(rule.expiresAt);
    if (!Number.isNaN(expiresAt.getTime()) && expiresAt.getTime() <= now.getTime()) return false;
  }
  return true;
}
```js

### Rust `trust_rule_is_active`:
```rust
pub fn trust_rule_is_active(config: &TrustRuleConfig, now: DateTime<Utc>) -> bool {
    if config.revoked_at.is_some() { return false; }
    if let Some(ref expires) = config.expires_at {
        match DateTime::parse_from_rfc3339(expires) {
            Ok(expires_at) => {
                if expires_at.with_timezone(&Utc) <= now { return false; }
            }
            Err(_) => {} // invalid date, fall through
        }
    }
    true
}
```

### Node `rateLimitRule`:
```js
function rateLimitRule(policy) {
  const config = isRecord(policy.config) ? policy.config : {};
  const raw = isRecord(config.rateLimit) ? config.rateLimit : config;
  const limit = typeof raw.limit === "number" ? raw.limit : null;
  const windowSeconds = typeof raw.windowSeconds === "number" ? raw.windowSeconds : null;
  if (!limit || !windowSeconds || limit <= 0 || windowSeconds <= 0) return null;
  return { limit, windowSeconds, keyBy: ... };
}
```js

### Rust `rate_limit_rule`: 1:1 parity.

## 测试

### policy_validation 模块
```
running 15 tests
test iso_date_or_null_valid ... ok
test iso_date_or_null_invalid_returns_none ... ok
test trust_rule_active_when_no_revoked_or_expires ... ok
test trust_rule_revoked_is_inactive ... ok
test trust_rule_expired_is_inactive ... ok
test trust_rule_not_yet_expired_is_active ... ok
test trust_rule_revoked_takes_priority ... ok
test trust_rule_config_extraction ... ok
test trust_rule_config_no_trust_rule_returns_none ... ok
test rate_limit_rule_basic ... ok
test rate_limit_rule_with_key_by ... ok
test rate_limit_rule_flat_config ... ok
test rate_limit_rule_invalid_returns_none ... ok
test trust_rule_config_serde_camel_case ... ok
test rate_limit_rule_serde_camel_case ... ok

test result: ok. 15 passed; 0 failed
```

### pc-tool 全测
```
test result: ok. 127 passed; 0 failed
```

## 关键 parity 验证

- `trust_rule_is_active` - revoked 优先于 expired 1:1 parity
- `rate_limit_rule` - rateLimit wrapper + flat config + keyBy 数组过滤
- `iso_date_or_null` - RFC3339 parse, invalid → None
- serde `rename_all = "camelCase"` 镜像 Node wire format

## R710 关键交付

- [x] policy_validation.rs 模块 + 15 个单测 PASS
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `isoDateOrNull`/`trustRuleIsActive`/`rateLimitRule` 100% parity
- [x] pc-tool 全测 127 PASS (无 regression, +15 新测)

## 累计 R700-R710 成果

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
- **总计**: 139 个新单测 PASS, ~2200 行新增代码

## pc-tool 现在完整度

| 模块 | 行 | 测试 | 来源 |
|---|---:|---:|---|
| risk.rs | 164 | 11 | R701 |
| connection_health.rs | 159 | 13 | R703 |
| descriptor_hash.rs | 148 | 10 | R704 |
| selector_match.rs | 275 | 12 | R706 |
| argument_condition.rs | 254 | 17 | R707 |
| side_effect_idempotency.rs | 213 | 14 | R708 |
| summarize_redact.rs | 276 | 16 | R709 |
| policy_validation.rs | 213 | 15 | R710 |
| service.rs (原有) | 275 | 基础 CRUD + hooks |
| profile_binding.rs (原有) | 328 | scope precedence |
| runtime_metrics.rs (原有) | 457 | 8 | R708 期间 |
| connection/ (原有) | 299 | — |
| **总计 src** | **~3,061** | **127** | |

## 下一步

- R711 — 评估整体方向: 继续 pc-tool 或转向 pc-issues / pc-feedback
- R712 — 阶段 L UI 收尾：mutation 真实流通验证
- R713 — 阶段 K Adapter：等待硬约束 #2 解除

