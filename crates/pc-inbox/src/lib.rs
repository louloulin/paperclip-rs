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

mod service;

pub use service::{
    InboxAgentPolicy, InboxAgentPolicyMode, InboxAgentPolicyService, InboxDismissalRow,
    InboxError, InboxHook, InboxHookEvent, InboxService, NewDismissal, NoopInboxHook,
    RecordingInboxHook, UpdateInboxAgentPolicyInput,
};
