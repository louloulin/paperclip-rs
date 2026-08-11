#![forbid(unsafe_code)]

//! Budget 业务层。
//!
//! 与 paperclip 上游 `server/src/services/budgets.ts` 思路一致：
//! - 封装 `BudgetRepo`（pc-repos）作为持久化层
//! - 计算 budget window（calendar_month_utc / lifetime）
//! - 推导 status（ok / warning / hard_stop）基于 observed + amount + warn_percent
//! - 通过 `BudgetEnforcementHook` trait 解耦副作用（cancel work for scope）
//!
//! 设计目标：
//! - 纯策略层（无 DB 依赖的核心逻辑独立可测）
//! - 业务层负责：窗口计算 + 状态推导 + 副作用 hook
//! - 调用方注入 enforcement hook（cancelWorkForScope / notify / pause agent）

pub mod quota_windows;
pub mod service;

pub use quota_windows::{
    fetch_all_quota_windows, provider_slug_for_adapter_type, with_quota_timeout, AdapterRegistry,
    ProviderQuotaResult, QuotaAdapter, QUOTA_PROVIDER_TIMEOUT_MS,
};
pub use service::{
    compute_window, infer_status, normalize_scope_name, BudgetEnforcementHook, BudgetEnforcementScope,
    BudgetError, BudgetPolicyStatus, BudgetService, BudgetThresholdType, BudgetWindow, BudgetWindowKind,
    BudgetResult, FullEvaluation, IncidentOutcome, NoopEnforcementHook,
};
