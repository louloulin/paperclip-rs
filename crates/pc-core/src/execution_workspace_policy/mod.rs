//! Execution workspace policy 域模块（与 Node
//! `server/src/services/execution-workspace-policy.ts` 1:1 对齐）。
//!
//! ## 职责
//! - `types`：所有类型定义 + 字符串字面量常量
//! - `parse`：3 个 parser（strategy / policy / settings）+ environment 投影
//! - `resolve`：mode / strategy / environment 解析 + 默认值计算
//! - `guard`：worktree 不可运行守卫 + 错误码常量
//! - `gate`：project policy 开关闸
//! - `build`：adapter config 构造
//!
//! ## 设计原则
//! - `mod.rs` 仅做 facade 聚合（无业务逻辑）
//! - 全部逻辑是 **pure**：输入 `&serde_json::Value` / typed inputs，输出 typed outputs
//! - 不持任何状态；不依赖 IO
//! - HTTP / DB 层仅 `use execution_workspace_policy::*;`

pub mod build;
pub mod gate;
pub mod guard;
pub mod parse;
pub mod resolve;
pub mod types;

// ============================================================================
// Public re-exports
// ============================================================================

pub use build::{
    build_execution_workspace_adapter_config, BuildExecutionWorkspaceAdapterConfigInput,
};
pub use gate::gate_project_execution_workspace_policy;
pub use guard::{
    has_reusable_execution_workspace_binding, is_unrunnable_worktree_combo,
    IsUnrunnableWorktreeComboInput, WORKSPACE_WORKTREE_REQUIRES_PROJECT_CODE,
    WORKSPACE_WORKTREE_REQUIRES_PROJECT_MESSAGE, WORKSPACE_WORKTREE_REQUIRES_PROJECT_REMEDIATION,
};
pub use parse::{
    as_string as parse_as_string, parse_execution_workspace_strategy,
    parse_issue_execution_workspace_settings, parse_issue_execution_workspace_settings_with_options,
    parse_object as parse_value_object, parse_project_execution_workspace_policy,
    select_environment_execution_workspace_settings, ParseIssueExecutionWorkspaceSettingsOptions,
};
pub use resolve::{
    default_issue_execution_workspace_settings_for_project,
    issue_execution_workspace_mode_for_persisted_workspace,
    resolve_effective_workspace_strategy_type, resolve_execution_workspace_environment_id,
    resolve_execution_workspace_mode, resolve_pinned_issue_workspace_strategy_type,
};
pub use types::{
    default_mode, environment_source, is_parsed_mode, mode, strategy_type,
    ExecutionWorkspaceEnvironmentResolution, ExecutionWorkspaceStrategy,
    IssueExecutionWorkspaceSettings, NetworkEgress, ParsedExecutionWorkspaceMode,
    ProjectExecutionWorkspacePolicy, UnrunnableWorktreeIssueRef,
};
