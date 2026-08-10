#![forbid(unsafe_code)]
//! Feedback redaction business service.
mod service;
pub use pc_repos::feedback_redaction::{
    redact_free_text, sanitize_free_text_value, truncate_string_fields, truncate_value,
    RedactionState,
};
pub use service::{
    NoopRedactionHook, RecordingRedactionHook, RedactionError, RedactionHook, RedactionHookEvent,
    RedactionService, DEFAULT_MAX_CHARS,
};
