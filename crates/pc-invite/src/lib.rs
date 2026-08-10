#![forbid(unsafe_code)]

//! Invite domain service layer.
//!
//! Provides [`InviteService`] — a high-level facade over
//! [`pc_repos::invite::InviteRepo`] that:
//!
//! * Generates URL-safe invite tokens (32+ random bytes) and stores only the
//!   SHA-256 hash on disk
//! * Validates inputs (non-nil company, non-empty invite_type / allowed_join_types,
//!   expires_at in the future)
//! * Routes writes through an [`InviteHook`] chain so callers can layer
//!   notification / authorization side-effects without touching SQL
//! * Translates repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`]
//!
//! An invite is a one-shot join link bound to a (company, allowed_join_types,
//! defaults_payload, expires_at). Once accepted it is marked `accepted_at`;
//! once revoked it is marked `revoked_at`; expired tokens can no longer be
//! accepted (computed from `expires_at`).

mod service;

pub use service::{
    CreatedInvite, InviteHook, InviteHookEvent, InviteRow, InviteService, InviteStatus,
    InviteWithStatus, NewInvite, NoopInviteHook, RecordingInviteHook,
};
