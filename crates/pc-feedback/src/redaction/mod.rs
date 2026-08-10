//! Feedback free-text redaction 业务层。
//!
//! 与原 `crates/pc-feedback-redaction/src/lib.rs` 等价：
//! - `redact_free_text` / `sanitize_free_text_value` / `truncate_string_fields` /
//!   `truncate_value` / `RedactionState` —— 纯函数（来自 `pc_repos::feedback_redaction`）
//! - `RedactionService` —— DB 写入层（带 RedactionHook）

pub use pc_repos::feedback_redaction::{
    redact_free_text, sanitize_free_text_value, truncate_string_fields, truncate_value,
    RedactionState,
};

mod service;
pub use service::{
    NoopRedactionHook, RecordingRedactionHook, RedactionError, RedactionHook, RedactionHookEvent,
    RedactionService, DEFAULT_MAX_CHARS,
};
