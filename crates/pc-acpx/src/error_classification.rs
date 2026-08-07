//! `pc-acpx` error classification — pure helpers that mirror
//! `describeErrorDiagnostics`, `classifyError`, and `isResumeFailure` from
//! Node `acpx-engine/execute.ts`.
//!
//! The module is the single source of truth for the runtime-error taxonomy
//! the engine exposes through `AdapterExecutionResult.errorCode` /
//! `errorMeta`. Callers feed in any error value that satisfies
//! `std::error::Error + 'static` and the helpers return a stable
//! `{errorCode, errorMeta}` shape. There is no I/O and no async; the
//! module is deterministic and trivially testable.

use serde_json::{Map, Value};

// ============================================================================
// Public types
// ============================================================================

/// Which phase the engine was in when the error was raised. Drives the
/// default `errorCode` when no ACP protocol code overrides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpxExecutionPhase {
    EnsureSession,
    ConfigureSession,
    Turn,
}

impl AcpxExecutionPhase {
    /// Stable lowercase label used as the `phase` field of `errorMeta`.
    pub fn as_str(&self) -> &'static str {
        match self {
            AcpxExecutionPhase::EnsureSession => "ensure_session",
            AcpxExecutionPhase::ConfigureSession => "configure_session",
            AcpxExecutionPhase::Turn => "turn",
        }
    }
}

/// Structural diagnostic view of an error. Mirrors the
/// `{errorName, acpCode, causeMessage, retryable, stackPreview}` shape
/// from the Node implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpxErrorDiagnostics {
    pub error_name: String,
    pub acp_code: Option<String>,
    pub cause_message: Option<String>,
    pub retryable: Option<bool>,
    pub stack_preview: Option<String>,
}

/// The classification result. Mirrors `Pick<AdapterExecutionResult,
/// "errorCode" | "errorMeta">` from the Node implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedError {
    pub error_code: String,
    pub error_meta: Map<String, Value>,
}

// ============================================================================
// describeErrorDiagnostics
// ============================================================================

/// Extract the structural diagnostic view of `err`. `error_name` is the
/// Rust type name (mirrors `err.name || err.constructor.name` / the
/// `typeof err` fallback in Node).
pub fn describe_error_diagnostics(err: &(dyn std::error::Error + 'static)) -> AcpxErrorDiagnostics {
    let error_name = extract_error_name(err);
    let acp_code = extract_acp_code(err);
    let cause_message = walk_cause_message(err);
    let retryable = extract_retryable(err);
    let stack_preview = build_stack_preview(err);
    AcpxErrorDiagnostics {
        error_name,
        acp_code,
        cause_message,
        retryable,
        stack_preview,
    }
}

/// Compute the diagnostic-friendly name. Rust has no `name` property;
/// for a concrete type we use `std::any::type_name_of_val` (Rust 1.76+),
/// for a trait object we fall back to the first line of the Display text
/// or `"Error"` when even that is empty.
fn extract_error_name(err: &(dyn std::error::Error + 'static)) -> String {
    let raw = std::any::type_name_of_val(err).to_string();
    let trimmed = raw
        .rsplit_once("<")
        .map(|(head, _)| head)
        .unwrap_or(raw.as_str());
    if trimmed.starts_with("dyn ") {
        // Trait object — Display text is the only signal we have.
        let display = format!("{err}");
        let first_line = display.lines().next().unwrap_or("").trim();
        if first_line.is_empty() {
            "Error".to_string()
        } else {
            // Truncate to a sensible cap.
            first_line.chars().take(64).collect::<String>()
        }
    } else {
        trimmed.to_string()
    }
}

/// Try to extract an `ACP_*` code from the error. The convention is that
/// the Display text begins with `code: <CODE>:` (matches the Node
/// foreign-error convention used by acpx).
fn extract_acp_code(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let display = format!("{err}");
    let prefix = "code: ";
    let rest = display.strip_prefix(prefix)?;
    let end = rest
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(rest.len());
    let code = &rest[..end];
    if code.starts_with("ACP_") {
        Some(code.to_string())
    } else {
        None
    }
}

/// Walk the error chain for a cause message. First look for a `cause:`
/// line in Display (foreign-error convention); fall back to the Rust
/// `Error::source` chain.
fn walk_cause_message(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let display = format!("{err}");
    let marker = "cause: ";
    if let Some(idx) = display.find(marker) {
        let after = &display[idx + marker.len()..];
        let line = after.find('\n').map(|end| &after[..end]).unwrap_or(after);
        if !line.is_empty() {
            return Some(line.to_string());
        }
    }
    while let Some(src) = err.source() {
        return Some(src.to_string());
    }
    None
}

/// Try to extract a `retryable=<bool>` marker from Display (foreign-error
/// convention).
fn extract_retryable(err: &(dyn std::error::Error + 'static)) -> Option<bool> {
    let display = format!("{err}");
    let needle = "retryable=";
    let idx = display.find(needle)?;
    let after = &display[idx + needle.len()..];
    if after.starts_with("true") {
        Some(true)
    } else if after.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Try to extract a 6-line stack preview from Display. The convention is
/// `stack: <line1>\n<line2>\n...`.
fn build_stack_preview(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let display = format!("{err}");
    let marker = "stack: ";
    let idx = display.find(marker)?;
    let after = &display[idx + marker.len()..];
    let preview: String = after
        .lines()
        .take(6)
        .map(|line| format!("{line}\n"))
        .collect();
    let preview = preview.trim_end_matches('\n').to_string();
    if preview.is_empty() {
        None
    } else {
        Some(preview)
    }
}

// ============================================================================
// classifyError
// ============================================================================

/// Classify `err` into a stable `{errorCode, errorMeta}` pair. `phase` is
/// optional and only used to pick the default `errorCode` when the error
/// carries no ACP protocol code.
pub fn classify_error(
    err: &(dyn std::error::Error + 'static),
    phase: Option<AcpxExecutionPhase>,
) -> ClassifiedError {
    let message = err.to_string();
    let diagnostics = describe_error_diagnostics(err);
    let acp_code = diagnostics.acp_code.clone();

    let mut base_meta = Map::new();
    base_meta.insert("errorName".into(), Value::String(diagnostics.error_name));
    if let Some(code) = &acp_code {
        base_meta.insert("acpCode".into(), Value::String(code.clone()));
    }
    if let Some(cause) = &diagnostics.cause_message {
        base_meta.insert("causeMessage".into(), Value::String(cause.clone()));
    }
    if let Some(retryable) = diagnostics.retryable {
        base_meta.insert("retryable".into(), Value::Bool(retryable));
    }
    if let Some(stack) = &diagnostics.stack_preview {
        base_meta.insert("stackPreview".into(), Value::String(stack.clone()));
    }
    if let Some(phase) = phase {
        base_meta.insert("phase".into(), Value::String(phase.as_str().to_string()));
    }

    let lower = message.to_lowercase();
    let auth_like =
        lower.contains("auth") || lower.contains("login") || lower.contains("credential");

    if auth_like {
        let mut meta = Map::new();
        meta.insert("category".into(), Value::String("auth".into()));
        merge_meta(&mut meta, base_meta);
        return ClassifiedError {
            error_code: "acpx_auth_required".into(),
            error_meta: meta,
        };
    }

    let phase_code = if acp_code.as_deref() == Some("ACP_SESSION_INIT_FAILED") {
        Some("acpx_session_init_failed")
    } else if acp_code.as_deref() == Some("ACP_TURN_FAILED") {
        Some("acpx_turn_failed")
    } else if acp_code.as_deref() == Some("ACP_BACKEND_MISSING") {
        Some("acpx_backend_missing")
    } else if acp_code.as_deref() == Some("ACP_BACKEND_UNAVAILABLE") {
        Some("acpx_backend_unavailable")
    } else if phase == Some(AcpxExecutionPhase::EnsureSession) {
        Some("acpx_session_init_failed")
    } else if phase == Some(AcpxExecutionPhase::ConfigureSession) {
        Some("acpx_session_config_failed")
    } else if phase == Some(AcpxExecutionPhase::Turn) {
        Some("acpx_turn_failed")
    } else {
        None
    };

    if let Some(code) = phase_code {
        let mut meta = Map::new();
        let category = if acp_code.is_some() {
            "protocol"
        } else {
            "runtime"
        };
        meta.insert("category".into(), Value::String(category.into()));
        merge_meta(&mut meta, base_meta);
        return ClassifiedError {
            error_code: code.into(),
            error_meta: meta,
        };
    }

    if acp_code.is_some() {
        let mut meta = Map::new();
        meta.insert("category".into(), Value::String("protocol".into()));
        merge_meta(&mut meta, base_meta);
        return ClassifiedError {
            error_code: "acpx_protocol_error".into(),
            error_meta: meta,
        };
    }

    let mut meta = Map::new();
    meta.insert("category".into(), Value::String("runtime".into()));
    merge_meta(&mut meta, base_meta);
    ClassifiedError {
        error_code: "acpx_runtime_error".into(),
        error_meta: meta,
    }
}

fn merge_meta(target: &mut Map<String, Value>, source: Map<String, Value>) {
    for (k, v) in source {
        target.insert(k, v);
    }
}

// ============================================================================
// isResumeFailure
// ============================================================================

/// Return `true` if `err` looks like a resume-style failure: any of the
/// keywords `resume`, `load`, `not found`, `no session`, `unknown
/// session`, `conversation` appears in the error message
/// (case-insensitive).
pub fn is_resume_failure(err: &(dyn std::error::Error + 'static)) -> bool {
    let message = err.to_string().to_lowercase();
    message.contains("resume")
        || message.contains("load")
        || message.contains("not found")
        || message.contains("no session")
        || message.contains("unknown session")
        || message.contains("conversation")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    /// Error that exposes `code: <CODE>: <msg>` in its Display — mirrors
    /// the convention used by Node foreign errors that carry a `code`
    /// property.
    #[derive(Debug)]
    struct CodedError {
        code: &'static str,
        message: &'static str,
    }

    impl fmt::Display for CodedError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "code: {}: {}", self.code, self.message)
        }
    }

    impl std::error::Error for CodedError {}

    #[derive(Debug)]
    struct PlainError(&'static str);

    impl fmt::Display for PlainError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for PlainError {}

    fn err_static(err: impl std::error::Error + 'static) -> Box<dyn std::error::Error + 'static> {
        Box::new(err)
    }

    #[test]
    fn auth_required_message_yields_acpx_auth_required() {
        let err = err_static(PlainError("Authentication required: please login"));
        let classified = classify_error(&*err, Some(AcpxExecutionPhase::Turn));
        assert_eq!(classified.error_code, "acpx_auth_required");
        assert_eq!(
            classified.error_meta.get("category"),
            Some(&Value::String("auth".into()))
        );
        assert_eq!(
            classified.error_meta.get("phase"),
            Some(&Value::String("turn".into()))
        );
    }

    #[test]
    fn auth_credential_keyword_also_triggers_auth_category() {
        let err = err_static(PlainError("missing credential for account"));
        let classified = classify_error(&*err, None);
        assert_eq!(classified.error_code, "acpx_auth_required");
    }

    #[test]
    fn acp_session_init_failed_phase_yields_protocol_code() {
        let err = err_static(CodedError {
            code: "ACP_SESSION_INIT_FAILED",
            message: "session boot failed",
        });
        let classified = classify_error(&*err, Some(AcpxExecutionPhase::EnsureSession));
        assert_eq!(classified.error_code, "acpx_session_init_failed");
        assert_eq!(
            classified.error_meta.get("category"),
            Some(&Value::String("protocol".into()))
        );
        assert_eq!(
            classified.error_meta.get("acpCode"),
            Some(&Value::String("ACP_SESSION_INIT_FAILED".into()))
        );
    }

    #[test]
    fn acp_turn_failed_phase_yields_turn_failed() {
        let err = err_static(CodedError {
            code: "ACP_TURN_FAILED",
            message: "boom",
        });
        let classified = classify_error(&*err, Some(AcpxExecutionPhase::Turn));
        assert_eq!(classified.error_code, "acpx_turn_failed");
    }

    #[test]
    fn ensure_session_phase_without_acp_code_maps_to_session_init() {
        let err = err_static(PlainError("boom"));
        let classified = classify_error(&*err, Some(AcpxExecutionPhase::EnsureSession));
        assert_eq!(classified.error_code, "acpx_session_init_failed");
        assert_eq!(
            classified.error_meta.get("category"),
            Some(&Value::String("runtime".into()))
        );
    }

    #[test]
    fn configure_session_phase_maps_to_session_config() {
        let err = err_static(PlainError("boom"));
        let classified = classify_error(&*err, Some(AcpxExecutionPhase::ConfigureSession));
        assert_eq!(classified.error_code, "acpx_session_config_failed");
    }

    #[test]
    fn turn_phase_maps_to_turn_failed() {
        let err = err_static(PlainError("boom"));
        let classified = classify_error(&*err, Some(AcpxExecutionPhase::Turn));
        assert_eq!(classified.error_code, "acpx_turn_failed");
    }

    #[test]
    fn unknown_phase_returns_acpx_runtime_error() {
        let err = err_static(PlainError("boom"));
        let classified = classify_error(&*err, None);
        assert_eq!(classified.error_code, "acpx_runtime_error");
        assert_eq!(
            classified.error_meta.get("category"),
            Some(&Value::String("runtime".into()))
        );
    }

    #[test]
    fn non_acp_code_returns_protocol_error_when_phase_missing() {
        let err = err_static(CodedError {
            code: "ACP_RANDOM_THING",
            message: "x",
        });
        let classified = classify_error(&*err, None);
        assert_eq!(classified.error_code, "acpx_protocol_error");
    }

    #[test]
    fn non_acp_string_field_is_ignored() {
        let err = err_static(CodedError {
            code: "ENOENT",
            message: "x",
        });
        let classified = classify_error(&*err, None);
        assert_eq!(classified.error_code, "acpx_runtime_error");
        assert!(classified.error_meta.get("acpCode").is_none());
    }

    #[test]
    fn stack_preview_truncated_to_six_lines() {
        #[derive(Debug)]
        struct StackedError;
        impl fmt::Display for StackedError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("stack: line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n")
            }
        }
        impl std::error::Error for StackedError {}
        let err = err_static(StackedError);
        let diagnostics = describe_error_diagnostics(&*err);
        let preview = diagnostics.stack_preview.expect("stack preview expected");
        let lines: Vec<&str> = preview.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[5], "line6");
    }

    #[test]
    fn is_resume_failure_matches_conversation_keyword() {
        let err = err_static(PlainError("Could not resume conversation: not found"));
        assert!(is_resume_failure(&*err));
    }

    #[test]
    fn is_resume_failure_matches_unknown_session() {
        let err = err_static(PlainError("unknown session id"));
        assert!(is_resume_failure(&*err));
    }

    #[test]
    fn is_resume_failure_returns_false_for_unrelated_errors() {
        let err = err_static(PlainError("rate limit exceeded"));
        assert!(!is_resume_failure(&*err));
    }

    #[test]
    fn retryable_field_round_trips_through_meta() {
        #[derive(Debug)]
        struct RetryableError;
        impl fmt::Display for RetryableError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("retryable=true: backend overloaded")
            }
        }
        impl std::error::Error for RetryableError {}
        let err = err_static(RetryableError);
        let classified = classify_error(&*err, None);
        assert_eq!(
            classified.error_meta.get("retryable"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn cause_message_extracted_from_display() {
        #[derive(Debug)]
        struct CausableError;
        impl fmt::Display for CausableError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("wrapper error\ncause: inner boom\nmore context")
            }
        }
        impl std::error::Error for CausableError {}
        let err = err_static(CausableError);
        let diagnostics = describe_error_diagnostics(&*err);
        assert_eq!(diagnostics.cause_message.as_deref(), Some("inner boom"));
    }
}
