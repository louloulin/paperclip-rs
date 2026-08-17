#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Derived case type from pipeline reference.
//!
//! R554: Direct port of `paperclip/packages/shared/src/pipeline-case-type.ts`.
//! A case's "type" is not a user-facing field — it is derived from which
//! pipeline the case lives in. Used internally for display + ingest checks.

/// Minimal reference to a pipeline needed to derive its case type.
#[derive(Debug, Clone)]
pub struct CaseTypePipelineRef {
    pub id: String,
    pub key: Option<String>,
}

/// Derive the case type from a pipeline reference. The pipeline key is the
/// canonical type identifier; we fall back to the pipeline id if `key` is
/// missing or empty.
pub fn derive_case_type(pipeline: &CaseTypePipelineRef) -> String {
    let trimmed = pipeline.key.as_deref().map_or("", str::trim);
    if trimmed.is_empty() {
        pipeline.id.clone()
    } else {
        trimmed.to_string()
    }
}

/// Ingest sanity-check: a case being ingested into a pipeline must match that
/// pipeline's derived type. Returns true when the declared type is absent
/// or already agrees with the pipeline — i.e. nothing to correct.
pub fn case_type_matches_pipeline(
    declared_case_type: Option<&str>,
    pipeline: &CaseTypePipelineRef,
) -> bool {
    match declared_case_type {
        None | Some("") => true,
        Some(s) => s == derive_case_type(pipeline),
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn ref_with(id: &str, key: Option<&str>) -> CaseTypePipelineRef {
        CaseTypePipelineRef {
            id: id.into(),
            key: key.map(str::to_string),
        }
    }

    #[test]
    fn derive_prefers_key() {
        let p = ref_with("pl-1", Some("release-notes"));
        assert_eq!(derive_case_type(&p), "release-notes");
    }

    #[test]
    fn derive_falls_back_to_id() {
        let p = ref_with("pl-1", None);
        assert_eq!(derive_case_type(&p), "pl-1");
    }

    #[test]
    fn derive_falls_back_when_key_empty() {
        let p = ref_with("pl-1", Some(""));
        assert_eq!(derive_case_type(&p), "pl-1");
    }

    #[test]
    fn derive_trims_key_whitespace() {
        let p = ref_with("pl-1", Some("   release-notes   "));
        assert_eq!(derive_case_type(&p), "release-notes");
    }

    #[test]
    fn derive_falls_back_when_key_is_whitespace() {
        let p = ref_with("pl-1", Some("   "));
        assert_eq!(derive_case_type(&p), "pl-1");
    }

    // ----- R773 edge cases for case_type_matches_pipeline -----

    #[test]
    fn r773_case_type_matches_returns_true_when_declared_is_none() {
        let p = ref_with("pl-1", Some("release-notes"));
        assert!(case_type_matches_pipeline(None, &p));
    }

    #[test]
    fn r773_case_type_matches_returns_true_when_declared_is_empty() {
        let p = ref_with("pl-1", Some("release-notes"));
        assert!(case_type_matches_pipeline(Some(""), &p));
    }

    #[test]
    fn r773_case_type_matches_returns_true_when_declared_matches_derived_key() {
        let p = ref_with("pl-1", Some("release-notes"));
        assert!(case_type_matches_pipeline(Some("release-notes"), &p));
    }

    #[test]
    fn r773_case_type_matches_returns_true_when_declared_matches_derived_id_fallback() {
        let p = ref_with("pl-1", None);
        assert!(case_type_matches_pipeline(Some("pl-1"), &p));
    }

    #[test]
    fn r773_case_type_matches_returns_false_when_declared_mismatches() {
        let p = ref_with("pl-1", Some("release-notes"));
        assert!(!case_type_matches_pipeline(Some("hotfix"), &p));
    }

    #[test]
    fn r773_case_type_matches_handles_whitespace_only_key_fallback() {
        let p = ref_with("pl-1", Some("   "));
        assert!(case_type_matches_pipeline(Some("pl-1"), &p));
    }
}
