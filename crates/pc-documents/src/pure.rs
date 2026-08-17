#![forbid(unsafe_code)]
//! Pure validation and normalization helpers for the document service.
//!
//! R782: extracted from `service.rs` so the rules can be tested in isolation
//! without spinning up a database. The methods on the input structs in
//! `service.rs` delegate to these free functions so the public API is unchanged.
//!
//! Aligned with `paperclip/server/src/services/documents.ts` (the Node original).

use pc_errors::{unprocessable, validation, Result};
use uuid::Uuid;

pub const ALLOWED_FORMATS: &[&str] = &["markdown", "plain", "html"];
pub const DEFAULT_FORMAT: &str = "markdown";
pub const ANNOTATION_AUTHOR_TYPES: &[&str] = &["user", "agent", "system"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedCreate {
    pub title: Option<String>,
    pub format: String,
}

pub fn normalize_document_key(key: &str) -> String {
    key.trim().to_lowercase()
}

pub fn is_allowed_format(format: &str) -> bool {
    ALLOWED_FORMATS.contains(&format)
}

pub fn is_allowed_author_type(author_type: &str) -> bool {
    ANNOTATION_AUTHOR_TYPES.contains(&author_type)
}

pub fn normalize_create_document(
    company_id: Uuid,
    body: &str,
    format: Option<&str>,
    title: Option<String>,
) -> Result<NormalizedCreate> {
    if company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if body.is_empty() {
        return Err(validation("document body must not be empty"));
    }
    let format = format
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_FORMAT.to_string());
    if !is_allowed_format(&format) {
        return Err(validation(format!(
            "format must be one of markdown/plain/html, got {format}"
        )));
    }
    Ok(NormalizedCreate { title, format })
}

pub fn validate_document_patch(format: Option<&str>, body: Option<&str>) -> Result<()> {
    if let Some(f) = format {
        if !is_allowed_format(f) {
            return Err(validation(format!(
                "format must be one of markdown/plain/html, got {f}"
            )));
        }
    }
    if let Some(b) = body {
        if b.is_empty() {
            return Err(validation("document body must not be empty"));
        }
    }
    Ok(())
}

pub fn validate_annotation_thread(
    company_id: Uuid,
    issue_id: Uuid,
    document_id: Uuid,
    document_key: &str,
    selected_text: &str,
    normalized_start: i32,
    normalized_end: i32,
    markdown_start: i32,
    markdown_end: i32,
) -> Result<()> {
    if company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if issue_id.is_nil() {
        return Err(validation("issueId is required"));
    }
    if document_id.is_nil() {
        return Err(validation("documentId is required"));
    }
    if document_key.trim().is_empty() {
        return Err(validation("documentKey must not be empty"));
    }
    if selected_text.is_empty() {
        return Err(validation("selectedText must not be empty"));
    }
    if normalized_end < normalized_start {
        return Err(unprocessable("normalizedEnd must be >= normalizedStart"));
    }
    if markdown_end < markdown_start {
        return Err(unprocessable("markdownEnd must be >= markdownStart"));
    }
    Ok(())
}

pub fn validate_annotation_comment(
    company_id: Uuid,
    thread_id: Uuid,
    issue_id: Uuid,
    document_id: Uuid,
    body: &str,
    author_type: &str,
) -> Result<()> {
    if company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if thread_id.is_nil() {
        return Err(validation("threadId is required"));
    }
    if issue_id.is_nil() {
        return Err(validation("issueId is required"));
    }
    if document_id.is_nil() {
        return Err(validation("documentId is required"));
    }
    if body.trim().is_empty() {
        return Err(validation("comment body must not be empty"));
    }
    if !is_allowed_author_type(author_type) {
        return Err(validation(format!(
            "authorType must be user/agent/system, got {author_type}"
        )));
    }
    Ok(())
}

pub fn validate_upsert_issue_document(
    company_id: Uuid,
    issue_id: Uuid,
    key: &str,
    body: &str,
    format: Option<&str>,
) -> Result<()> {
    if company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if issue_id.is_nil() {
        return Err(validation("issueId is required"));
    }
    if key.trim().is_empty() {
        return Err(validation("document key must not be empty"));
    }
    if body.is_empty() {
        return Err(validation("document body must not be empty"));
    }
    if let Some(f) = format {
        if !is_allowed_format(f) {
            return Err(validation(format!(
                "format must be one of markdown/plain/html, got {f}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn uuid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    #[test]
    fn r782_is_allowed_format_accepts_three_canonical() {
        assert!(is_allowed_format("markdown"));
        assert!(is_allowed_format("plain"));
        assert!(is_allowed_format("html"));
    }

    #[test]
    fn r782_is_allowed_format_rejects_others() {
        assert!(!is_allowed_format("xml"));
        assert!(!is_allowed_format(""));
        assert!(!is_allowed_format("MARKDOWN"));
    }

    #[test]
    fn r782_is_allowed_author_type_three_values() {
        assert!(is_allowed_author_type("user"));
        assert!(is_allowed_author_type("agent"));
        assert!(is_allowed_author_type("system"));
        assert!(!is_allowed_author_type("admin"));
        assert!(!is_allowed_author_type(""));
    }

    #[test]
    fn r782_normalize_document_key_trims_and_lowercases() {
        assert_eq!(normalize_document_key("  HELLO  "), "hello");
        assert_eq!(normalize_document_key("MixedCase"), "mixedcase");
        assert_eq!(normalize_document_key("kebab-case"), "kebab-case");
        assert_eq!(normalize_document_key(""), "");
    }

    #[test]
    fn r782_normalize_create_valid_inputs() {
        let r = normalize_create_document(
            uuid(1),
            "body",
            Some("markdown"),
            Some("Title".to_string()),
        ).unwrap();
        assert_eq!(r.format, "markdown");
        assert_eq!(r.title.as_deref(), Some("Title"));
    }

    #[test]
    fn r782_normalize_create_defaults_format_to_markdown() {
        let r = normalize_create_document(uuid(1), "body", None, None).unwrap();
        assert_eq!(r.format, "markdown");
        assert_eq!(r.title, None);
    }

    #[test]
    fn r782_normalize_create_rejects_nil_company_id() {
        let err = normalize_create_document(Uuid::nil(), "body", None, None).unwrap_err();
        assert!(err.to_string().contains("companyId"));
    }

    #[test]
    fn r782_normalize_create_rejects_empty_body() {
        let err = normalize_create_document(uuid(1), "", None, None).unwrap_err();
        assert!(err.to_string().contains("body must not be empty"));
    }

    #[test]
    fn r782_normalize_create_rejects_unknown_format() {
        let err = normalize_create_document(uuid(1), "body", Some("xml"), None).unwrap_err();
        assert!(err.to_string().contains("format must be one of"));
    }

    #[test]
    fn r782_validate_document_patch_no_changes_ok() {
        validate_document_patch(None, None).unwrap();
        validate_document_patch(Some("plain"), None).unwrap();
        validate_document_patch(None, Some("body")).unwrap();
    }

    #[test]
    fn r782_validate_document_patch_rejects_empty_body() {
        let err = validate_document_patch(None, Some("")).unwrap_err();
        assert!(err.to_string().contains("body must not be empty"));
    }

    #[test]
    fn r782_validate_document_patch_rejects_bad_format() {
        let err = validate_document_patch(Some("xml"), None).unwrap_err();
        assert!(err.to_string().contains("format must be one of"));
    }

    #[test]
    fn r782_validate_annotation_thread_happy_path() {
        validate_annotation_thread(
            uuid(1), uuid(2), uuid(3),
            "key", "selected", 0, 5, 0, 5,
        ).unwrap();
    }

    #[test]
    fn r782_validate_annotation_thread_rejects_nil_uuids() {
        let err = validate_annotation_thread(
            Uuid::nil(), uuid(2), uuid(3), "key", "selected", 0, 5, 0, 5,
        ).unwrap_err();
        assert!(err.to_string().contains("companyId"));

        let err = validate_annotation_thread(
            uuid(1), Uuid::nil(), uuid(3), "key", "selected", 0, 5, 0, 5,
        ).unwrap_err();
        assert!(err.to_string().contains("issueId"));

        let err = validate_annotation_thread(
            uuid(1), uuid(2), Uuid::nil(), "key", "selected", 0, 5, 0, 5,
        ).unwrap_err();
        assert!(err.to_string().contains("documentId"));
    }

    #[test]
    fn r782_validate_annotation_thread_rejects_empty_key_or_text() {
        let err = validate_annotation_thread(
            uuid(1), uuid(2), uuid(3), "  ", "selected", 0, 5, 0, 5,
        ).unwrap_err();
        assert!(err.to_string().contains("documentKey"));

        let err = validate_annotation_thread(
            uuid(1), uuid(2), uuid(3), "key", "", 0, 5, 0, 5,
        ).unwrap_err();
        assert!(err.to_string().contains("selectedText"));
    }

    #[test]
    fn r782_validate_annotation_thread_rejects_inverted_ranges() {
        let err = validate_annotation_thread(
            uuid(1), uuid(2), uuid(3), "key", "sel", 5, 0, 0, 5,
        ).unwrap_err();
        assert!(err.to_string().contains("normalizedEnd"));

        let err = validate_annotation_thread(
            uuid(1), uuid(2), uuid(3), "key", "sel", 0, 5, 5, 0,
        ).unwrap_err();
        assert!(err.to_string().contains("markdownEnd"));
    }

    #[test]
    fn r782_validate_annotation_thread_allows_zero_length_range() {
        validate_annotation_thread(
            uuid(1), uuid(2), uuid(3), "key", "sel", 5, 5, 5, 5,
        ).unwrap();
    }

    #[test]
    fn r782_validate_annotation_comment_happy() {
        validate_annotation_comment(uuid(1), uuid(2), uuid(3), uuid(4), "body", "user").unwrap();
    }

    #[test]
    fn r782_validate_annotation_comment_trims_whitespace_body() {
        let err = validate_annotation_comment(uuid(1), uuid(2), uuid(3), uuid(4), "   ", "user").unwrap_err();
        assert!(err.to_string().contains("comment body must not be empty"));
    }

    #[test]
    fn r782_validate_annotation_comment_rejects_invalid_author_type() {
        let err = validate_annotation_comment(uuid(1), uuid(2), uuid(3), uuid(4), "body", "admin").unwrap_err();
        assert!(err.to_string().contains("authorType must be user/agent/system"));
    }

    #[test]
    fn r782_validate_upsert_issue_document_happy() {
        validate_upsert_issue_document(uuid(1), uuid(2), "key", "body", None).unwrap();
        validate_upsert_issue_document(uuid(1), uuid(2), "key", "body", Some("html")).unwrap();
    }

    #[test]
    fn r782_validate_upsert_issue_document_rejects_blank_key() {
        let err = validate_upsert_issue_document(uuid(1), uuid(2), "  ", "body", None).unwrap_err();
        assert!(err.to_string().contains("document key"));
    }

    #[test]
    fn r782_validate_upsert_issue_document_rejects_empty_body() {
        let err = validate_upsert_issue_document(uuid(1), uuid(2), "key", "", None).unwrap_err();
        assert!(err.to_string().contains("document body must not be empty"));
    }

    #[test]
    fn r782_validate_upsert_issue_document_rejects_bad_format() {
        let err = validate_upsert_issue_document(uuid(1), uuid(2), "key", "body", Some("xml")).unwrap_err();
        assert!(err.to_string().contains("format must be one of"));
    }
}
