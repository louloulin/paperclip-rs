#![forbid(unsafe_code)]
//! `pc-issue-recovery-actions` — Issue recovery action 业务服务。
//!
//! 对应 Node `services/issue-recovery-actions.ts`（307 行）。本 crate 封装
//! `pc-repos::issue::IssueRepo` 的 recovery action 函数，提供：
//!
//! - **Service 层 API**（`IssueRecoveryActionService`）：
//!   - `upsert`：含 per-(company, source) 串行化
//!   - `get_active_for_issue`：取 active action
//!   - `list_active_for_issues`：批量取
//!   - `list_for_issue`：取所有（不限状态）
//!   - `resolve`：resolve active action（4 种 lookup 方式）
//!   - `to_info`：DB row → DTO 转换
//! - **Hook 系统**：`IssueRecoveryActionHook` trait（4 个回调）
//! - **DTO 转换**：`IssueRecoveryActionInfo::from_row`（与 Node `toReadModel` 1:1 对齐）
//! - **常量**：`ACTIVE_RECOVERY_ACTION_STATUSES`、`MAX_UPSERT_RETRIES`、合法值集合
//!
//! 设计原则：
//! - **高内聚**：所有 recovery action 业务集中在本 crate。
//! - **低耦合**：上游 HTTP 路由只需构造请求 DTO 并调用 service。
//! - **薄封装**：所有 SQL 走 pc-repos（已有 list / get / upsert / resolve 系列），
//!   本 crate 只负责编排（并发串行化）+ DTO 转换 + Hook。
//! - **真实测试**：e2e 测试打到真实 Postgres。

mod hook;
mod service;
mod types;

pub use hook::{
    IssueRecoveryActionHook, IssueRecoveryActionHookEvent,
    NoopIssueRecoveryActionHook, RecordingIssueRecoveryActionHook,
};
pub use service::IssueRecoveryActionService;
pub use types::{
    ActiveRecoveryActionsByIssue, IssueRecoveryActionError, IssueRecoveryActionInfo,
    IssueRecoveryActionResult, ResolveIssueRecoveryActionRequest,
    UpsertIssueRecoveryActionRequest, ACTIVE_RECOVERY_ACTION_STATUSES, MAX_UPSERT_RETRIES,
    VALID_RECOVERY_ACTION_KINDS, VALID_RECOVERY_ACTION_OUTCOMES,
    VALID_RECOVERY_ACTION_OWNER_TYPES, VALID_RECOVERY_ACTION_STATUSES,
};
