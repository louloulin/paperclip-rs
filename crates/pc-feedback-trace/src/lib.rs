#![forbid(unsafe_code)]
//! Feedback trace service: read and delete issue feedback traces with isolated hooks.
mod service;
pub use pc_repos::feedback_trace::FeedbackTraceRow;
pub use service::{
    FeedbackTraceError, FeedbackTraceHook, FeedbackTraceHookEvent, FeedbackTraceService,
    NoopFeedbackTraceHook, RecordingFeedbackTraceHook,
};
