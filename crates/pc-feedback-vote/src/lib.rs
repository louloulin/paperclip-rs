#![forbid(unsafe_code)]

//! Feedback vote domain service layer.
//!
//! Provides [`FeedbackVoteService`] — a high-level facade over
//! [`pc_repos::feedback_vote::FeedbackVoteRepo`] that:
//!
//! * Validates inputs (non-nil company/issue, non-empty target_type,
//!   target_id, author_user_id, vote; vote must be "up" or "down")
//! * Resolves `issue_id → company_id` for compound creates
//! * Routes writes through a [`FeedbackVoteHook`] chain so callers can layer
//!   activity / realtime / scoring side-effects without touching SQL
//! * Translates repo `sqlx::Error` into [`pc_errors::Error`] so HTTP / CLI
//!   layers only need to handle one error type
//!
//! Each feedback vote belongs to exactly one (company, issue). The service
//! provides a thin wrapper that translates `NewFeedbackVote` into repo calls
//! and emits one hook event per successful write.

mod service;

pub use service::{
    FeedbackVoteError, FeedbackVoteHook, FeedbackVoteHookEvent, FeedbackVoteService,
    NewFeedbackVote, NoopFeedbackVoteHook, RecordingFeedbackVoteHook,
};
