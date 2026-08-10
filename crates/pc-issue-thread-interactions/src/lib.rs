#![forbid(unsafe_code)]
//! `pc-issue-thread-interactions` — Issue thread interactions 业务服务。
//!
//! 对应 Node `services/issue-thread-interactions.ts`（2226 行 — paperclip 最大 service）。
//!
//! 本 crate 封装 `pc-repos::issue::{IssueThreadInteractionRow, IssueRepo}`，
//! 提供 5 类交互的 CRUD + 状态流转：
//!
//! - **Interaction 类型**：
//!   - `ask_user_questions` —— 问用户问题
//!   - `request_confirmation` —— 请求确认（accept/reject）
//!   - `request_checkbox_confirmation` —— 多选确认
//!   - `request_item_verdicts` —— 批量判定
//!   - `suggest_tasks` —— 建议任务
//! - **状态**：`pending` / `accepted` / `rejected` / `cancelled` / `withdrawn` /
//!   `answered` / `responded` / `blocked` / `done`
//! - **ContinuationPolicy**：`none` / `wake_assignee` / `wake_assignee_on_accept`
//! - **Service 层 API**（`IssueThreadInteractionService`）：
//!   - `create(input)` —— 创建（带 idempotency）
//!   - `list_for_issue(issue_id)` / `list_for_company(company_id, issue_id)` / `list_pending(company_id)`
//!   - `get(id)` / `get_idempotent(...)`
//!   - `accept / reject / cancel / withdraw / submit_verdicts / respond` —— 状态流转
//!   - `resolve_with_result(...)` —— 通用 resolve
//! - **Hook 系统**：`IssueThreadInteractionHook` trait（5 回调）
//!
//! 设计原则：
//! - **高内聚**：所有 thread interaction 业务集中在本 crate。
//! - **低耦合**：上游 HTTP 路由只需构造请求 DTO 并调用 service。
//! - **薄封装**：核心 SQL 走 pc-repos（已有 list / get / create / resolve 系列），
//!   本 crate 只负责编排（状态机 + 校验 + Hook + DTO 转换）。
//! - **真实测试**：e2e 测试打到真实 Postgres。

mod hook;
mod service;
mod types;

// Re-export
pub use hook::{
    IssueThreadInteractionHook, IssueThreadInteractionHookEvent,
    NoopIssueThreadInteractionHook, RecordingIssueThreadInteractionHook,
};
pub use service::{
    accept_interaction, cancel_interaction, create_interaction, get_idempotent_interaction,
    get_interaction, list_interactions, list_interactions_for_company,
    list_pending_interactions_attention, reject_interaction, resolve_interaction,
    respond_interaction, submit_verdicts, withdraw_interaction, IssueThreadInteractionService,
};
pub use types::{
    ContinuationPolicy, CreateIssueThreadInteractionInput, InteractionActor,
    InteractionResolution, InteractionStatus, IssueThreadInteractionError,
    IssueThreadInteractionInfo, ListIssueThreadInteractionsFilter, ResolveInteractionInput,
    SubmitVerdictsInput, INTERACTION_KINDS, INTERACTION_STATUSES, INTERACTION_TERMINAL_STATUSES,
};
