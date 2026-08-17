# R738 — pc-decisions/src/wakeup_validation_pure.rs

## 目标

补足 Node paperclip/server/src/services/decision-wakeup.ts 中 wake input 校验 + outcome 字符串转换 + source/trigger detail 白名单 + uuid 校验。

## 新增 helpers (10 个)

| Node 语义 | Rust 函数 |
|---|---|
| wake input 字段非空校验 | WakeOriginInput::validate() |
| wakeup source 白名单校验 | validate_wakeup_source(source) |
| trigger detail 白名单校验 | validate_trigger_detail(detail) |
| DecisionOutcome label | outcome_label(o) |
| DecisionOutcome parse | outcome_from_label(s) |
| 唤醒 target 等价判定 | same_wake_target(left, right) |
| idempotency key 派生 | derive_wake_idempotency_key(input) |
| uuid 合法性 | is_valid_uuid(s) |

## 常量

- ALLOWED_WAKEUP_SOURCES = [timer, assignment, on_demand, automation]
- ALLOWED_TRIGGER_DETAILS = [manual, ping, callback, system]

## 测试结果

cargo test -p pc-decisions --lib wakeup_validation_pure
test result: ok. 19 passed; 0 failed

## 关键设计

- WakeOriginInput 自定义 struct（避免依赖 wakeup/mod.rs 复杂闭包类型）
- outcome_from_label 兼容 American spelling（canceled → Cancelled）
- same_wake_target 不比较 outcome（outcome 不同但同 target 视为同一唤醒）
- is_valid_uuid 用 Uuid::parse_str 校验

## 文件

- 新增：crates/pc-decisions/src/wakeup_validation_pure.rs (7251 bytes)
- 修改：crates/pc-decisions/src/wakeup/mod.rs (+2 行 pub mod types + pub use types::DecisionOutcome)
- 修改：crates/pc-decisions/src/lib.rs (+1 行 pub mod wakeup_validation_pure)
