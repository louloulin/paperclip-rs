#![forbid(unsafe_code)]
//! `pc-issue-execution-policy` — Issue 执行策略状态机业务服务。
//!
//! 对应 Node `services/issue-execution-policy.ts`（~1226 行）。本 crate
//! **封装** `pc-core::issue_execution_transitions` 的纯函数，提供：
//!
//! - **服务层 API**（`IssueExecutionPolicyService`）：
//!   - `apply_transition`：高阶 API，stage + monitor 组合
//!   - `apply_monitor_only`：仅 monitor 转换
//!   - `build_initial_monitor`：新 issue 创建时构造 monitor 字段
//!   - `trigger_monitor`：monitor 被触发时构造 patch
//!   - `clear_monitor`：monitor 被清除时构造 patch
//! - **Hook 系统**：`IssueExecutionPolicyHook` trait（before/after × transition/monitor）
//! - **DB 集成**：`apply_to_row` 把 patch 应用到 `IssueRow`
//! - **类型**：与 Node 1:1 对齐的 input/output DTO
//!
//! 设计原则：
//! - **高内聚**：所有 execution policy 业务集中在本 crate。
//! - **低耦合**：上游 HTTP 路由只需构造 `ApplyTransitionRequest` 并调用 service。
//! - **薄封装**：所有计算走 pc-core 纯函数（已 2250 行），本 crate 只负责编排 + hook。
//! - **真实测试**：e2e 测试打到真实 Postgres，加载真实 issues + 应用 transition。

mod hook;
mod service;
mod types;

pub use hook::{
    IssueExecutionPolicyHook, IssueExecutionPolicyHookEvent, NoopIssueExecutionPolicyHook,
    RecordingIssueExecutionPolicyHook,
};
pub use service::IssueExecutionPolicyService;
pub use types::{
    ApplyTransitionOutcome, ApplyTransitionRequest, ClearMonitorRequest,
    IssueExecutionPolicyError, IssueExecutionPolicyResult,
    MonitorPatchOutcome, RequestedAssigneePatchDto, TriggerMonitorRequest,
    ExecutionPolicyActor, InitialMonitorRequest,
};

// Re-export pc-core key constants for convenience
pub use pc_core::{
    DEFAULT_MAX_REVIEW_ROUNDS, MONITOR_BOUNDS_EXHAUSTED_MESSAGE, MONITOR_INVALID_MESSAGE,
    STAGE_DECISION_COMMENT_HINT,
};
