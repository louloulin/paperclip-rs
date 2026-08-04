//! Task watchdog mutation scope 解析与子树校验（对齐 Node `server/src/services/task-watchdog-scope.ts`，174 行）。
//!
//! 模块拆分（按 `docs/08-RUST-MODULAR-ARCHITECTURE.md` ≥ 300 行 / ≥ 3 类职责门槛）：
//! - [`types`]  ：公开类型（`AgentRunActor` / `IssueScopeTarget` / `TaskWatchdogMutationScope` / 常量）
//! - [`helpers`]：纯助手（`is_plain_record` / `read_string` / `read_task_watchdog_context`）
//! - [`resolver`]：DB IO + 校验（`resolve_task_watchdog_mutation_scope` / `issue_is_in_task_watchdog_subtree` / `task_watchdog_scope_allows_issue_mutation`）
//! - [`tests`]  ：单测（含 3 个 `is_plain_record` / `read_string` 边界 + 4 个 scope 判定）

mod helpers;
mod resolver;
mod types;

#[cfg(test)]
mod tests;

pub use types::{
    AgentRunActor, IssueScopeTarget, TaskWatchdogMutationScope, TaskWatchdogMutationScopeKind,
    TASK_WATCHDOG_ORIGIN_KIND,
};
pub use resolver::{
    issue_is_in_task_watchdog_subtree, resolve_task_watchdog_mutation_scope,
    task_watchdog_scope_allows_issue_mutation, TaskWatchdogScopeAllowsOptions,
};
