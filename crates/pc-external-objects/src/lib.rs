#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! External object mention source label formatting and canonical URL types.
//!
//! R553: Direct port of `paperclip/packages/shared/src/external-objects.ts` (52 LOC).
//! Pure type definitions + the `formatExternalObjectMentionSourceLabel` helper.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalObjectMentionSourceKind {
    Title,
    Description,
    Comment,
    Document,
    Property,
    Plugin,
}

impl ExternalObjectMentionSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Description => "description",
            Self::Comment => "comment",
            Self::Document => "document",
            Self::Property => "property",
            Self::Plugin => "plugin",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "title" => Some(Self::Title),
            "description" => Some(Self::Description),
            "comment" => Some(Self::Comment),
            "document" => Some(Self::Document),
            "property" => Some(Self::Property),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }

    /// All known source kinds — used to drive UI dropdowns / filters.
    pub fn all() -> &'static [Self] {
        &[
            Self::Title,
            Self::Description,
            Self::Comment,
            Self::Document,
            Self::Property,
            Self::Plugin,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct ExternalObjectUrlMatch {
    pub index: usize,
    pub length: usize,
    pub matched_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalScheme {
    Http,
    Https,
}

impl CanonicalScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExternalObjectCanonicalIdentity {
    pub scheme: CanonicalScheme,
    pub host: String,
    pub path: String,
    pub query_param_hashes: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct ExternalObjectUrlCanonicalizationOptions {
    pub identity_query_params: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExternalObjectCanonicalUrl {
    pub sanitized_canonical_url: String,
    pub sanitized_display_url: String,
    pub canonical_identity: ExternalObjectCanonicalIdentity,
    pub canonical_identity_hash: String,
    pub redacted_matched_text: String,
}

#[derive(Debug, Clone)]
pub struct ExternalObjectMentionSource {
    pub company_id: Option<String>,
    pub source_issue_id: Option<String>,
    pub source_kind: ExternalObjectMentionSourceKind,
    pub source_record_id: Option<String>,
    pub document_key: Option<String>,
    pub property_key: Option<String>,
}

/// Build a human-readable label for an external object mention source.
/// Mirrors the `formatExternalObjectMentionSourceLabel` switch.
pub fn format_external_object_mention_source_label(source: &ExternalObjectMentionSource) -> String {
    match source.source_kind {
        ExternalObjectMentionSourceKind::Title => "Title".to_string(),
        ExternalObjectMentionSourceKind::Description => "Description".to_string(),
        ExternalObjectMentionSourceKind::Comment => "Comment".to_string(),
        ExternalObjectMentionSourceKind::Document => match &source.document_key {
            Some(key) if !key.is_empty() => format!("Document: {key}"),
            _ => "Document".to_string(),
        },
        ExternalObjectMentionSourceKind::Property => match &source.property_key {
            Some(key) if !key.is_empty() => format!("Property: {key}"),
            _ => "Property".to_string(),
        },
        ExternalObjectMentionSourceKind::Plugin => "Plugin".to_string(),
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn source_kind_round_trip() {
        for k in ExternalObjectMentionSourceKind::all() {
            let s = k.as_str();
            assert_eq!(ExternalObjectMentionSourceKind::parse(s), Some(*k));
        }
        assert!(ExternalObjectMentionSourceKind::parse("nope").is_none());
    }

    #[test]
    fn all_includes_six_kinds() {
        assert_eq!(ExternalObjectMentionSourceKind::all().len(), 6);
    }

    #[test]
    fn format_title() {
        let s = ExternalObjectMentionSource {
            company_id: None,
            source_issue_id: None,
            source_kind: ExternalObjectMentionSourceKind::Title,
            source_record_id: None,
            document_key: None,
            property_key: None,
        };
        assert_eq!(format_external_object_mention_source_label(&s), "Title");
    }

    #[test]
    fn format_document_with_key() {
        let s = ExternalObjectMentionSource {
            company_id: None,
            source_issue_id: None,
            source_kind: ExternalObjectMentionSourceKind::Document,
            source_record_id: None,
            document_key: Some("design.md".into()),
            property_key: None,
        };
        assert_eq!(
            format_external_object_mention_source_label(&s),
            "Document: design.md"
        );
    }

    #[test]
    fn format_document_without_key_falls_back() {
        let s = ExternalObjectMentionSource {
            company_id: None,
            source_issue_id: None,
            source_kind: ExternalObjectMentionSourceKind::Document,
            source_record_id: None,
            document_key: Some(String::new()),
            property_key: None,
        };
        assert_eq!(format_external_object_mention_source_label(&s), "Document");
    }

    #[test]
    fn format_property_with_key() {
        let s = ExternalObjectMentionSource {
            company_id: None,
            source_issue_id: None,
            source_kind: ExternalObjectMentionSourceKind::Property,
            source_record_id: None,
            document_key: None,
            property_key: Some("priority".into()),
        };
        assert_eq!(
            format_external_object_mention_source_label(&s),
            "Property: priority"
        );
    }

    #[test]
    fn format_plugin() {
        let s = ExternalObjectMentionSource {
            company_id: None,
            source_issue_id: None,
            source_kind: ExternalObjectMentionSourceKind::Plugin,
            source_record_id: None,
            document_key: None,
            property_key: None,
        };
        assert_eq!(format_external_object_mention_source_label(&s), "Plugin");
    }
}
