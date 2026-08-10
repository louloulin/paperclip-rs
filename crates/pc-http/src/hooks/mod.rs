//! HTTP 层的 service hook 组合。
//!
//! 把 `pc-companies::CompanyHook` 等 service hook trait 与
//! `pc_http::AppState` 中的 ActivityLog / Realtime / PluginEventBus
//! 粘合起来 — 让 service 通过 hook 自动触发业务事件，无需直接依赖
//! AppState。

pub mod agent_activity_hook;
pub mod agent_termination_approval_hook;
pub mod approval_decision_link_hook;
pub mod company_activity_hook;
pub mod company_budget_hook;
pub mod decision_activity_hook;
pub mod issue_activity_hook;

pub use agent_activity_hook::AgentActivityHook;
pub use agent_termination_approval_hook::AgentTerminationApprovalHook;
pub use approval_decision_link_hook::ApprovalDecisionLinkHook;
pub use company_activity_hook::CompanyActivityHook;
pub use company_budget_hook::CompanyBudgetHook;
pub use decision_activity_hook::DecisionActivityHook;
pub use issue_activity_hook::IssueActivityHook;
