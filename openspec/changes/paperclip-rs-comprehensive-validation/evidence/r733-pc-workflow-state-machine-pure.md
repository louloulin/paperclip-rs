# R733 — pc-workflow/src/state_machine_pure.rs

## 目标

补足 Node paperclip/server/src/services/workflow/state-machine.ts 中的
state transition 校验逻辑（合法 / 非法转移矩阵 + 可重试判定）。

## 新增 helpers (3 个)

| Node 语义 | Rust 函数 |
|---|---|
| run state transition 合法性 | is_valid_run_state_transition(from, to) |
| step status transition 合法性 | is_valid_step_status_transition(from, to) |
| step 是否可重试 | is_retryable_step_status(from) |

## 测试结果

cargo test -p pc-workflow --lib state_machine_pure
test result: ok. 16 passed; 0 failed

## 关键设计

- 用 use ... as alias 在 match 表达式内避免命名冲突（StepStatus 和 WorkflowRunState 都有 Pending/Running/Succeeded/Failed）
- is_valid_run_state_transition 严格按 Node state-machine.ts：
  - Pending → {Queued, Running, Cancelled}
  - Queued → {Running, Cancelled}
  - Running → {Succeeded, Failed, Cancelled}
  - 终态 idempotent (Succeeded → Succeeded, Failed → Failed, Cancelled → Cancelled)
- is_retryable_step_status 仅 Failed → true（其他状态不可重试）

## 文件

- 新增：crates/pc-workflow/src/state_machine_pure.rs (6440 bytes)
- 修改：crates/pc-workflow/src/lib.rs (+1 行 pub mod state_machine_pure;)
