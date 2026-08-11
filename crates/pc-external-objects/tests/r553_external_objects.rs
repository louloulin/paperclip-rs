//! R553 — pc-external-objects 综合测试。

#![allow(clippy::doc_markdown)]

use pc_external_objects::{
    format_external_object_mention_source_label, CanonicalScheme, ExternalObjectCanonicalIdentity,
    ExternalObjectMentionSource, ExternalObjectMentionSourceKind,
    ExternalObjectUrlCanonicalizationOptions, ExternalObjectUrlMatch,
};

#[test]
fn r553_source_kind_round_trip() {
    for kind in ExternalObjectMentionSourceKind::all() {
        let s = kind.as_str();
        assert_eq!(ExternalObjectMentionSourceKind::parse(s), Some(*kind));
    }
    assert!(ExternalObjectMentionSourceKind::parse("nope").is_none());
}

#[test]
fn r553_all_six_kinds_listed() {
    assert_eq!(ExternalObjectMentionSourceKind::all().len(), 6);
    let expected = [
        ExternalObjectMentionSourceKind::Title,
        ExternalObjectMentionSourceKind::Description,
        ExternalObjectMentionSourceKind::Comment,
        ExternalObjectMentionSourceKind::Document,
        ExternalObjectMentionSourceKind::Property,
        ExternalObjectMentionSourceKind::Plugin,
    ];
    assert_eq!(ExternalObjectMentionSourceKind::all(), &expected);
}

#[test]
fn r553_scheme_as_str() {
    assert_eq!(CanonicalScheme::Http.as_str(), "http");
    assert_eq!(CanonicalScheme::Https.as_str(), "https");
}

#[test]
fn r553_format_title() {
    let s = make_source(ExternalObjectMentionSourceKind::Title, None, None);
    assert_eq!(format_external_object_mention_source_label(&s), "Title");
}

#[test]
fn r553_format_description() {
    let s = make_source(ExternalObjectMentionSourceKind::Description, None, None);
    assert_eq!(
        format_external_object_mention_source_label(&s),
        "Description"
    );
}

#[test]
fn r553_format_comment() {
    let s = make_source(ExternalObjectMentionSourceKind::Comment, None, None);
    assert_eq!(format_external_object_mention_source_label(&s), "Comment");
}

#[test]
fn r553_format_document_with_key() {
    let s = make_source(
        ExternalObjectMentionSourceKind::Document,
        Some("design.md"),
        None,
    );
    assert_eq!(
        format_external_object_mention_source_label(&s),
        "Document: design.md"
    );
}

#[test]
fn r553_format_document_without_key_falls_back() {
    let s = make_source(ExternalObjectMentionSourceKind::Document, None, None);
    assert_eq!(format_external_object_mention_source_label(&s), "Document");
}

#[test]
fn r553_format_document_with_empty_key_falls_back() {
    let s = make_source(ExternalObjectMentionSourceKind::Document, Some(""), None);
    assert_eq!(format_external_object_mention_source_label(&s), "Document");
}

#[test]
fn r553_format_property_with_key() {
    let s = make_source(
        ExternalObjectMentionSourceKind::Property,
        None,
        Some("priority"),
    );
    assert_eq!(
        format_external_object_mention_source_label(&s),
        "Property: priority"
    );
}

#[test]
fn r553_format_property_without_key_falls_back() {
    let s = make_source(ExternalObjectMentionSourceKind::Property, None, None);
    assert_eq!(format_external_object_mention_source_label(&s), "Property");
}

#[test]
fn r553_format_plugin() {
    let s = make_source(ExternalObjectMentionSourceKind::Plugin, None, None);
    assert_eq!(format_external_object_mention_source_label(&s), "Plugin");
}

#[test]
fn r553_url_match_struct() {
    let m = ExternalObjectUrlMatch {
        index: 5,
        length: 12,
        matched_text: "https://x.com".to_string(),
    };
    assert_eq!(m.index, 5);
    assert_eq!(m.length, 12);
    assert_eq!(m.matched_text, "https://x.com");
}

#[test]
fn r553_canonical_identity() {
    let id = ExternalObjectCanonicalIdentity {
        scheme: CanonicalScheme::Https,
        host: "github.com".into(),
        path: "/paperclip".into(),
        query_param_hashes: None,
    };
    assert_eq!(id.scheme, CanonicalScheme::Https);
    assert_eq!(id.host, "github.com");
    assert_eq!(id.path, "/paperclip");
    assert!(id.query_param_hashes.is_none());
}

#[test]
fn r553_url_canonicalization_options() {
    let opts = ExternalObjectUrlCanonicalizationOptions {
        identity_query_params: vec!["ref".into(), "utm_source".into()],
    };
    assert_eq!(opts.identity_query_params.len(), 2);
}

#[test]
fn r553_mention_source_full() {
    let s = ExternalObjectMentionSource {
        company_id: Some("co-1".into()),
        source_issue_id: Some("iss-2".into()),
        source_kind: ExternalObjectMentionSourceKind::Document,
        source_record_id: Some("rec-3".into()),
        document_key: Some("spec.md".into()),
        property_key: None,
    };
    assert_eq!(s.company_id.as_deref(), Some("co-1"));
    assert_eq!(s.source_issue_id.as_deref(), Some("iss-2"));
    assert_eq!(
        format_external_object_mention_source_label(&s),
        "Document: spec.md"
    );
}

fn make_source(
    kind: ExternalObjectMentionSourceKind,
    document_key: Option<&str>,
    property_key: Option<&str>,
) -> ExternalObjectMentionSource {
    ExternalObjectMentionSource {
        company_id: None,
        source_issue_id: None,
        source_kind: kind,
        source_record_id: None,
        document_key: document_key.map(str::to_string),
        property_key: property_key.map(str::to_string),
    }
}
