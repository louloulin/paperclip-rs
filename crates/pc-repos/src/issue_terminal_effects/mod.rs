//! Issue 终态/停滞状态的跨模块清理。
//!
//! 对齐 Node `summary-slot-finalization.ts`、`status-card-finalization.ts`
//! 以及 `issue-thread-interactions.ts::expirePendingInteractionsForTerminalIssue`。

mod apply;
mod reasons;

pub use apply::apply_issue_terminal_effects;
pub use reasons::{status_card_failure_reason, summary_failure_reason};

use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TerminalEffectIssue<'a> {
    pub id: Uuid,
    pub company_id: Uuid,
    pub identifier: Option<&'a str>,
    pub title: &'a str,
    pub status: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct TerminalEffectActor<'a> {
    pub agent_id: Option<Uuid>,
    pub user_id: Option<&'a str>,
    pub run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalEffectCounts {
    pub summary_slots_failed: u64,
    pub status_cards_released: u64,
    pub status_card_updates_failed: u64,
    pub interactions_expired: u64,
    pub tool_actions_expired: u64,
}

#[cfg(test)]
mod tests;
