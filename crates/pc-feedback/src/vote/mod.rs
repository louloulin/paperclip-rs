//! Feedback vote 业务层（per-issue thumbs up/down with hooks）。
//!
//! 与原 `crates/pc-feedback-vote/src/lib.rs` 等价。

pub use pc_repos::feedback_vote::{FeedbackVoteRow, NewFeedbackVote};

mod service;
pub use service::{
    FeedbackVoteError, FeedbackVoteHook, FeedbackVoteHookEvent, FeedbackVoteService,
    NoopFeedbackVoteHook, RecordingFeedbackVoteHook,
};
