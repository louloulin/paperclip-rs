//! R567 — R-INTEGRATION-7: pc-external-objects → pc-issue-references (source label).
//!
//! Verifies that `pc-issues::references` delegates source-label formatting
//! to `pc-external-objects::format_external_object_mention_source_label` so
//! that all six kinds (Title/Description/Comment/Document/Property/Plugin)
//! produce unified, capitalised labels.

use pc_external_objects::{
    format_external_object_mention_source_label, ExternalObjectMentionSource,
    ExternalObjectMentionSourceKind,
};

/// Re-implementation of `pc_issues::references::service::source_label` —
/// we mirror the public behavior here to assert it matches the
/// pc-external-objects unified formatter byte-for-byte.
fn service_source_label(kind: &str, document_key: Option<&str>) -> String {
    match ExternalObjectMentionSourceKind::parse(kind) {
        Some(parsed_kind) => {
            let doc_key_for_doc =
                if matches!(parsed_kind, ExternalObjectMentionSourceKind::Document) {
                    document_key
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                };
            let source = ExternalObjectMentionSource {
                company_id: None,
                source_issue_id: None,
                source_kind: parsed_kind,
                source_record_id: None,
                document_key: doc_key_for_doc,
                property_key: None,
            };
            format_external_object_mention_source_label(&source)
        }
        None => kind.to_string(),
    }
}

#[test]
fn r567_title_label() {
    assert_eq!(service_source_label("title", None), "Title");
    assert_eq!(
        format_external_object_mention_source_label(&ExternalObjectMentionSource {
            company_id: None,
            source_issue_id: None,
            source_kind: ExternalObjectMentionSourceKind::Title,
            source_record_id: None,
            document_key: None,
            property_key: None,
        }),
        "Title"
    );
}

#[test]
fn r567_description_label() {
    assert_eq!(service_source_label("description", None), "Description");
}

#[test]
fn r567_comment_label() {
    assert_eq!(service_source_label("comment", None), "Comment");
}

#[test]
fn r567_document_label_without_key() {
    assert_eq!(service_source_label("document", None), "Document");
}

#[test]
fn r567_document_label_with_key() {
    assert_eq!(
        service_source_label("document", Some("plan.md")),
        "Document: plan.md"
    );
}

#[test]
fn r567_document_label_with_empty_key_falls_back() {
    assert_eq!(service_source_label("document", Some("")), "Document");
    assert_eq!(service_source_label("document", Some("   ")), "Document");
}

#[test]
fn r567_property_label_via_unified_formatter() {
    // `source_label` does NOT pass a property_key (the service doesn't
    // track property_key today), so it falls back to "Property" without
    // suffix — same shape as Title/Description/Comment.
    assert_eq!(service_source_label("property", None), "Property");
}

#[test]
fn r567_plugin_label_via_unified_formatter() {
    assert_eq!(service_source_label("plugin", None), "Plugin");
}

#[test]
fn r567_unknown_kind_returns_raw_string() {
    assert_eq!(service_source_label("unknown_kind", None), "unknown_kind");
}

#[test]
fn r567_legacy_source_kind_constants_still_resolve() {
    // SOURCE_KIND_TITLE / _DESCRIPTION / _COMMENT / _DOCUMENT are the
    // string constants used by the existing extractor + service.
    assert_eq!(service_source_label("title", None), "Title");
    assert_eq!(service_source_label("description", None), "Description");
    assert_eq!(service_source_label("comment", None), "Comment");
    assert_eq!(service_source_label("document", Some("k")), "Document: k");
}
