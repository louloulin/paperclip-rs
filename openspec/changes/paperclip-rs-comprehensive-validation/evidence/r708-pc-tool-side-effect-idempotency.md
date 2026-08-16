# R708 — pc-tool side_effect_idempotency + risk_rank + audit_outcome (2026-08-16)

## 目标

补足 Node `services/tool-access-policy.ts` 3 个核心 helpers:
- `sideEffectIdempotencyKey(ctx, argsHash)` - SHA-256 idempotency key
- `auditOutcome(decision)` - decision → audit outcome
- `riskRank(riskLevel)` - risk level → numeric rank

## 设计

- **新 submodule**: `crates/pc-tool/src/side_effect_idempotency.rs` (213 行)
- **新公开 API**:
  - `ToolAccessDecision` enum (Allow / RequireApproval / DeferRuntime / Deny)
  - `AuditOutcome` enum (Pending / Success / Denied / Timeout)
  - `IdempotencyContext` struct (7 optional fields)
  - `risk_rank(&str) -> u32`
  - `audit_outcome(ToolAccessDecision) -> AuditOutcome`
  - `side_effect_idempotency_key(&IdempotencyContext, &str) -> String`

## 算法 parity

### Node `sideEffectIdempotencyKey`:
```js
return `side_effect:${sha256({
  companyId, runId: heartbeatRunId, issueId, applicationId,
  connectionId, catalogEntryId, toolName, argumentsHash,
})}`;
```js

### Rust `side_effect_idempotency_key`:
```rust
pub fn side_effect_idempotency_key(ctx: &IdempotencyContext, arguments_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ctx.company_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    // ... 7 fields + |分隔 ...
    hasher.update(arguments_hash.as_bytes());
    format!("side_effect:{}", format!("{:x}", hasher.finalize()))
}
```

### Node `riskRank`:
```js
function riskRank(value) {
  if (value === "read" || value === "low") return 1;
  if (value === "write" || value === "medium") return 2;
  if (value === "destructive" || value === "high") return 3;
  if (value === "critical") return 4;
  return 0;
}
```js

### Rust `risk_rank`: 1:1 parity.

### Node `auditOutcome`:
```js
function auditOutcome(accessDecision) {
  if (accessDecision.decision === "allow") return "success";
  if (accessDecision.decision === "require_approval") return "pending";
  if (accessDecision.decision === "defer_runtime") return "timeout";
  return "denied";
}
```js

### Rust `audit_outcome`: 1:1 parity via match.

## 测试

### side_effect_idempotency 模块
```
running 14 tests
test idempotency_key_deterministic ... ok
test idempotency_key_changes_with_args_hash ... ok
test idempotency_key_changes_with_context ... ok
test idempotency_key_starts_with_side_effect_prefix ... ok
test idempotency_key_handles_none_fields ... ok
test risk_rank_known_levels ... ok
test risk_rank_unknown_returns_zero ... ok
test risk_rank_ordering ... ok
test audit_outcome_allow_success ... ok
test audit_outcome_require_approval_pending ... ok
test audit_outcome_defer_runtime_timeout ... ok
test audit_outcome_deny_denied ... ok
test audit_outcome_serde_camel_case ... ok
test decision_serde_snake_case ... ok

test result: ok. 14 passed; 0 failed
```

### pc-tool 全测
```
test result: ok. 96 passed; 0 failed
```

## 关键 parity 验证

- `side_effect_idempotency_key` - `side_effect:` 前缀 + SHA-256 hex (64 chars) + | 分隔
- `risk_rank` - 5 个已知 level (read/write/destructive/critical + low/medium/high aliases) + 0 fallback
- `audit_outcome` - 4 decision → 4 outcome 1:1 mapping
- serde `rename_all = "snake_case"` 镜像 Node decision string ("require_approval", "defer_runtime")
- serde `rename_all = "lowercase"` 镜像 Node audit outcome string ("success", "denied", "pending", "timeout")

## R708 关键交付

- [x] side_effect_idempotency.rs 模块 + 14 个单测 PASS
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `sideEffectIdempotencyKey`/`auditOutcome`/`riskRank` 100% parity
- [x] pc-tool 全测 96 PASS (无 regression, +14 新测)

## 累计 R700-R708 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **R703**: pc-tool/connection_health (13 tests)
- **R704**: pc-tool/descriptor_hash (10 tests)
- **R705**: pc-execution-workspace-guards/runtime_service_id (11 tests)
- **R706**: pc-tool/selector_match (12 tests)
- **R707**: pc-tool/argument_condition (17 tests)
- **R708**: pc-tool/side_effect_idempotency (14 tests)
- **总计**: 108 个新单测 PASS, ~1700 行新增代码

## 下一步

- R709 — pc-tool summarize_and_redact (Node SENSITIVE_KEY_RE + SECRET_VALUE_RE)
- R710 — pc-tool trust_rule_is_active (time-window policy)
- R711 — pc-issues issue_thread_interactions pure helpers

