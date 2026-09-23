//! 调度规格（job 名 / 计划时间 / 重试间隔）的格式化与解析。
//!
//! - **写者**：M5-7。
//! - **上游**：`scheduler/spec.go`261（262）—— 两个 `String()`（24 + 132 行的格式化/解析）、
//!   `validate`34、`retryDelay`16、`FloorPlan`8。
//! - **`FloorPlan`(8) 是 `plan_time` 的取整契约**：同一 tick 内的计划必须落到同一个 `plan_time`，
//!   否则 `sys_cron_executions` 的租约键会漂移、重复执行。
//! - **`String()` 是持久化格式**：它出现在租约表里 ⇒ 改格式等于迁移数据（本波**不改**格式）。
