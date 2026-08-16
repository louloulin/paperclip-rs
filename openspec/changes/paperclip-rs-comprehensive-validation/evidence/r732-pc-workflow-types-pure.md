# R732 — pc-workflow/src/types_pure.rs

## 目标

补足 Node paperclip/server/src/services/workflow 中 workflow 类型枚举的字符串转换
与触发器类型判定 pure helpers。

## 新增 helpers (6 个)

| Node 语义 | Rust 函数 |
|---|---|
| WorkflowKind 序列化标签 | workflow_kind_label(k) |
| RoutineKind 序列化标签 | routine_kind_label(k) |
| StepStatus 序列化标签 | step_status_label(s) |
| WorkflowRunState 序列化标签 | workflow_run_state_label(s) |
| run state 是否终态 | is_terminal_run_state(s) |
| step status 是否终态 | is_terminal_step_status(s) |
| trigger 是否 cron | is_cron_trigger(t) |
| 从 trigger 提取 cron expression | cron_expression_of(t) |

## 测试结果

cargo test -p pc-workflow --lib types_pure
test result: ok. 11 passed; 0 failed

## 关键设计

- 所有 label 函数返回 &'static str — 零分配
- is_terminal_* 函数复用 types.rs 已有的 is_terminal() 模式
- is_cron_trigger / cron_expression_of 用 match 表达式，零成本

## 文件

- 新增：crates/pc-workflow/src/types_pure.rs (5782 bytes)
- 修改：crates/pc-workflow/src/lib.rs (+1 行 pub mod types_pure;)
