//! Feedback trace 业务层。
//!
//! 与原 `crates/pc-feedback-trace/src/lib.rs` 等价。

mod pure;
mod service;
pub use service::{
    FeedbackTraceError, FeedbackTraceHook, FeedbackTraceHookEvent, FeedbackTraceService,
    NoopFeedbackTraceHook, RecordingFeedbackTraceHook,
};
