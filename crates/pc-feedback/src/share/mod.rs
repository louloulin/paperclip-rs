//! Feedback trace share 业务层。
//!
//! 底层 HTTP 上传实现位于 `pc_telemetry::feedback_share`；
//! 本模块提供 `FeedbackShareService` 业务编排 + hook 钩子。

pub use pc_telemetry::feedback_share::{
    build_feedback_share_object_key, encode_feedback_share_payload, FeedbackShareConfig,
    FeedbackTraceBundle, FeedbackTraceShareClient, FeedbackTraceShareError,
    HttpFeedbackTraceShareClient, UploadTraceBundleResponse, DEFAULT_FEEDBACK_EXPORT_BACKEND_URL,
    FEEDBACK_SHARE_ENCODING,
};

mod service;
pub use service::{
    FeedbackShareError, FeedbackShareHook, FeedbackShareHookEvent, FeedbackShareService,
    NoopFeedbackShareHook, RecordingFeedbackShareHook,
};
