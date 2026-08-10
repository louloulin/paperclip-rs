#![forbid(unsafe_code)]
//! Feedback trace share business service.
mod service;
pub use pc_telemetry::feedback_share::{
    build_feedback_share_object_key, encode_feedback_share_payload, FeedbackShareConfig,
    FeedbackTraceBundle, FeedbackTraceShareClient, FeedbackTraceShareError,
    HttpFeedbackTraceShareClient, UploadTraceBundleResponse, DEFAULT_FEEDBACK_EXPORT_BACKEND_URL,
    FEEDBACK_SHARE_ENCODING,
};
pub use service::{
    FeedbackShareError, FeedbackShareHook, FeedbackShareHookEvent, FeedbackShareService,
    NoopFeedbackShareHook, RecordingFeedbackShareHook,
};
