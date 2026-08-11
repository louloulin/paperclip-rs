#![forbid(unsafe_code)]

//! Approval 业务层。
//!
//! 与 paperclip 上游 `server/src/services/approvals.ts` 思路一致：
//! - 封装 `ApprovalRepo`（pc-repos）作为持久化层
//! - 通过 `ApprovalHook` trait 解耦副作用（hire_agent、budget policy、通知等）
//! - 状态机：`pending → approved | rejected | cancelled | revision_requested`
//!
//! 设计目标：
//! - 高内聚：所有 approval 业务逻辑（决定、状态转换、hook 触发）集中在一处
//! - 低耦合：通过 trait 抽象副作用，调用方按需注入
//! - 可测：trait-based hook 便于测试时替换

pub mod db_ops;
pub mod hire_hook;
pub mod issue_links;
pub mod service;
pub mod state_machine;

pub use db_ops::DbHireAgentOps;
pub use hire_hook::{
    HireAgentApprovalHook, HireAgentApprovalPayload, HireAgentOperations, HireMode,
};
pub use pc_repos::approval::{ApprovalStatus, ApprovalType};

pub use service::{
    ApprovalHook, ApprovalHookOutcome, ApprovalService, ApprovalServiceError,
    ApprovalServiceResult, FailingHook, NoopApprovalHook, RecordingHook,
};
pub use state_machine::{
    can_cancel, can_decide, can_request_revision, can_resubmit, validate_transition,
    DecisionAction, TransitionError,
};
