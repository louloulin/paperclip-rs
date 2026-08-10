#![forbid(unsafe_code)]

//! Cost domain service layer.
//!
//! Provides [`CostService`] — a high-level facade over
//! [`pc_repos::cost::CostRepo`] that:
//!
//! * Validates inputs (non-nil company/agent IDs, non-empty provider/model,
//!   non-negative cost, occurred_at not too far in the future)
//! * Routes writes through a [`CostHook`] chain so callers can layer
//!   activity / realtime / budget side-effects without touching SQL
//! * Translates repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`]
//!   so HTTP / CLI layers only need to handle one error type
//!
//! ## Domain
//!
//! `cost_events` records each model invocation (provider, model, tokens,
//! cost_cents). `finance_events` records higher-level debits/credits
//! (biller, amount_cents, direction=debit|credit, currency, estimated).
//! Each cost event may optionally back a finance event via `cost_event_id`.
//!
//! ## Hooks
//!
//! * `CostEventCreated` — emitted after every successful cost event insert
//! * `FinanceEventCreated` — emitted after every successful finance event insert
//! * `MonthlySpendUpdated` — emitted after agent/company `spent_monthly_cents`
//!   have been refreshed following a cost event insert
//!
//! After every `create_cost_event`, the service also invokes the budget
//! service's `evaluate_cost_event` so callers do not need to remember to do
//! that orchestration.

mod service;

pub use service::{
    CostByAgent, CostByAgentModel, CostByBiller, CostByProviderModel, CostByProject,
    CostEventCreatedData, CostEventRow, CostFinanceError, CostHook, CostHookEvent, CostRange,
    CostService, CostSummary, CostWindowSpendRow, CreateCostEvent, FinanceByBiller,
    FinanceByKind, FinanceEventCreatedData, FinanceEventRow, FinanceSummary,
    IssueCostSummaryRow, MonthlySpendUpdatedData, NewFinanceEvent, NoopCostHook,
    RecordingCostHook,
};
