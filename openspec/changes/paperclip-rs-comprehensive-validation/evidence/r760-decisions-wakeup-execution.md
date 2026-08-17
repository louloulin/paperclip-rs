# R760 — pc-decisions wakeup/bundle/effect_outcome 集成测试（+16 PASS）

## 目标

补充 pc-decisions 子模块（wakeup_validation_pure / bundle_validation_pure / effect_outcome_pure）的纯函数边缘测试，确保 decision/wakeup/execution 路径的零依赖校验逻辑全覆盖。

## 测试覆盖（+16 PASS）

### wakeup_validation_pure（+6 PASS）

| 测试 | 验证 |
|---|---|
| r760_outcome_from_label_cancelled_canceled_alias | cancelled/canceled 双 alias + uppercase |
| r760_outcome_from_label_invalid_returns_none | unknown/空/done（无 variant）→ None |
| r760_derive_wake_idempotency_key_format | "{agent}-{issue}-{decision}-{outcome}" 格式 |
| r760_same_wake_target_partial_match_false | outcome 不影响 wake target，decision_id 影响 |
| r760_validate_wakeup_source_whitelist | 4 个白名单 + 大小写敏感 + 空 |
| r760_validate_trigger_detail_whitelist | 4 个 trigger detail 白名单 |

### bundle_validation_pure（+5 PASS）

| 测试 | 验证 |
|---|---|
| r760_normalize_bundle_filter_clamps_limit | limit 钳制 [1, 500]（0 → 1, 9999 → 500, 50 保持）|
| r760_normalize_bundle_filter_lowercases_state | "  DONE  " → "done" |
| r760_is_valid_bundle_state_set | 5 个合法 + case-insensitive + whitespace 容忍 + 非法 |
| r760_require_non_nil_catches_nil_uuid | nil UUID 报错 + 真实 UUID 通过 |
| r760_validate_bundle_title | 空/纯空白报错，正常通过 |

### effect_outcome_pure（+5 PASS）

| 测试 | 验证 |
|---|---|
| r760_aggregate_outcomes_empty | 空 → (0, 0, "succeeded") |
| r760_aggregate_outcomes_all_executed | 全部 executed → succeeded |
| r760_aggregate_outcomes_all_failed | 全部 failed → failed |
| r760_aggregate_outcomes_partial | 部分成功 → partial |
| r760_aggregate_outcomes_with_skipped | Executed + Skipped → partial（Skipped 不算 success）|

## 验证

```
cargo test -p pc-decisions r760
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 153 filtered out

cargo test -p pc-decisions --lib
test result: ok. 169 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 全核心 crate 回归检查

| crate | PASS | 增量 |
|---|---:|---:|
| pc-issues | 183 | 0 |
| pc-routines | 193 | 0 |
| pc-heartbeat | 662 | 0 |
| pc-tool | 215 | 0 |
| pc-decisions | **169** | +16 |
| **合计** | **1422** | **+16** |

## R761+ 后续计划

- R761 — 真实 Chromium 浏览器对核心页面完成 mutation 流程
- 浏览器 UI mutation（agent / pipeline / issue / company）
- Adapter 仍按硬约束保持不动
