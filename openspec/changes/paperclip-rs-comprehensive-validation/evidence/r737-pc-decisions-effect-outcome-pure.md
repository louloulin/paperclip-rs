# R737 — pc-decisions/src/effect_outcome_pure.rs

## 目标

补足 Node paperclip/server/src/services/decisions.ts 中 effect execution outcome 聚合逻辑，
用强类型 enum 替代 stringly-typed 状态判断。

## 新增 helpers (5 个)

| Node 语义 | Rust 函数 |
|---|---|
| aggregateExecutionOutcomes(rows) | aggregate_outcomes(rows) → (successful, total, status) |
| EffectExecutionStatus enum (4 variants) | EffectExecutionStatus { Executed, Failed, Skipped, Pending } |
| status → string label | EffectExecutionStatus::as_str() |
| string → status parse | EffectExecutionStatus::from_str(s) |
| status → is_successful | EffectExecutionStatus::is_successful() |
| 最终成功判定 | is_final_success(status_label) |
| 部分成功判定 | is_partial_success(status_label) |

## 测试结果

cargo test -p pc-decisions --lib effect_outcome_pure
test result: ok. 14 passed; 0 failed

## 关键设计

- EffectExecutionStatus enum 替代 effect_executor.rs 中的字符串字面量
- aggregate_outcomes 逻辑与 Node aggregateExecutionOutcomes 1:1 对齐：
  - empty → succeeded
  - successful == total → succeeded
  - successful == 0 → failed
  - 部分 → partial
- is_successful 仅 Executed → true（Skipped / Pending 不算成功）

## 文件

- 新增：crates/pc-decisions/src/effect_outcome_pure.rs (6065 bytes)
- 修改：crates/pc-decisions/src/lib.rs (+1 行 pub mod effect_outcome_pure;)
