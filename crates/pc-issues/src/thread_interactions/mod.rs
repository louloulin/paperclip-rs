//! Issue 业务子模块（原 `pc-issue-thread-interactions` 已下沉到 `pc-issues::thread_interactions`）。
//!
//! 对应 Node `server/src/services/issue-thread_interactions.ts`。

mod hook;
mod service;
mod types;

// Re-export
pub use hook::{
    IssueThreadInteractionHook, IssueThreadInteractionHookEvent, NoopIssueThreadInteractionHook,
    RecordingIssueThreadInteractionHook,
};
pub use service::{
    accept_interaction, cancel_interaction, create_interaction, get_idempotent_interaction,
    get_interaction, list_interactions, list_interactions_for_company,
    list_pending_interactions_attention, reject_interaction, resolve_interaction,
    respond_interaction, submit_verdicts, withdraw_interaction, IssueThreadInteractionService,
};
pub use types::{
    ContinuationPolicy, CreateIssueThreadInteractionInput, InteractionActor, InteractionResolution,
    InteractionStatus, IssueThreadInteractionError, IssueThreadInteractionInfo,
    ListIssueThreadInteractionsFilter, ResolveInteractionInput, SubmitVerdictsInput,
    INTERACTION_KINDS, INTERACTION_STATUSES, INTERACTION_TERMINAL_STATUSES,
};
pub mod pure;
