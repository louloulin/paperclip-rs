#![forbid(unsafe_code)]

//! Inbox domain service layer.
//!
//! Provides two service structs that share a hook surface:
//!
//! * [`InboxService`] — wraps [`pc_repos::inbox::InboxRepo`]. Owns the
//!   dismiss / snooze / restore state machine for inbox items.
//! * [`InboxAgentPolicyService`] — wraps
//!   [`pc_repos::inbox_agent_policy::InboxAgentPolicyRepo`]. Owns the
//!   per-(company, user) inbox routing policy (open vs allowlist).
//!
//! Both services validate inputs, dispatch lifecycle hooks, and translate
//! repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`].

pub mod agent_policy;
pub mod dismissals;
mod service;

pub use agent_policy::{
    compute_allowed_agent_ids, dedup_agent_ids, default_inbox_agent_policy, find_invalid_agent_ids,
    find_invalid_agent_ids_from_map,
};
pub use dismissals::{compute_snoozed_until, InboxDismissalKind};
pub use service::{
    InboxAgentPolicy, InboxAgentPolicyMode, InboxAgentPolicyService, InboxDismissalRow, InboxError,
    InboxHook, InboxHookEvent, InboxService, NewDismissal, NoopInboxHook, RecordingInboxHook,
    UpdateInboxAgentPolicyInput,
};
