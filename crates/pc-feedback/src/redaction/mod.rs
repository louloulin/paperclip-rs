//! Feedback free-text redaction 业务层。
//!
//! R792A 抽离后：原本位于 `pc-repos/src/feedback_redaction.rs` 的纯函数模块
//! 物理迁移到 `pc-feedback/src/redaction/free_text_pure.rs`。
//! - `redact_free_text` / `sanitize_free_text_value` / `truncate_string_fields` /
//!   `truncate_value` / `RedactionState` —— 纯函数 (位于 `free_text_pure`)
//! - `RedactionService` —— DB 写入层（带 RedactionHook）

pub use free_text_pure::{
    redact_free_text, sanitize_free_text_value, truncate_string_fields, truncate_value,
    RedactionState,
};

pub mod free_text_pure;

mod service;
pub use service::{
    NoopRedactionHook, RecordingRedactionHook, RedactionError, RedactionHook, RedactionHookEvent,
    RedactionService, DEFAULT_MAX_CHARS,
};
pub mod pure;
pub mod redaction_state_pure;
