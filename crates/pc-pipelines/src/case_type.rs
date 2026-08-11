//! Pipeline case type derivation (port of `packages/shared/src/pipeline-case-type.ts`).
//!
//! A case's "type" is **not** a field anyone fills in — it is simply *which
//! pipeline the case lives in* (one pipeline per kind of thing). We derive it
//! from the pipeline so it can be used internally for display and ingest
//! sanity-checks without any new user-facing field or lifecycle machinery.
//!
//! The pipeline key is a stable slug and is the canonical type identifier; we
//! fall back to the pipeline id if a key is somehow absent.

/// Minimal reference shape — anything with at least an `id` and optional `key`.
#[derive(Debug, Clone)]
pub struct CaseTypePipelineRef {
    pub id: String,
    pub key: Option<String>,
}

impl CaseTypePipelineRef {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            key: None,
        }
    }

    #[must_use]
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

/// Derive the canonical case type for a pipeline.
///
/// Returns `pipeline.key.trim()` if non-empty, otherwise `pipeline.id`.
///
/// Mirrors Node upstream `deriveCaseType`.
#[must_use]
pub fn derive_case_type(pipeline: &CaseTypePipelineRef) -> String {
    let key = pipeline.key.as_deref().unwrap_or("").trim();
    if key.is_empty() {
        pipeline.id.clone()
    } else {
        key.to_string()
    }
}

/// Ingest sanity-check: a case being ingested into a pipeline must match that
/// pipeline's derived type.
///
/// Returns true when the (optional) declared type is absent or already agrees
/// with the pipeline — i.e. nothing to correct.
///
/// Mirrors Node upstream `caseTypeMatchesPipeline`.
#[must_use]
pub fn case_type_matches_pipeline(
    declared_case_type: Option<&str>,
    pipeline: &CaseTypePipelineRef,
) -> bool {
    match declared_case_type {
        None => true,
        Some(t) if t.is_empty() => true,
        Some(t) => t == derive_case_type(pipeline),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r531_derive_uses_key_when_present() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("support");
        assert_eq!(derive_case_type(&p), "support");
    }

    #[test]
    fn r531_derive_falls_back_to_id_when_key_missing() {
        let p = CaseTypePipelineRef::new("pln-123");
        assert_eq!(derive_case_type(&p), "pln-123");
    }

    #[test]
    fn r531_derive_falls_back_to_id_when_key_empty_string() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("");
        assert_eq!(derive_case_type(&p), "pln-123");
    }

    #[test]
    fn r531_derive_falls_back_to_id_when_key_whitespace() {
        // Node: `key.trim()` then fallback. Whitespace-only counts as empty.
        let p = CaseTypePipelineRef::new("pln-123").with_key("   ");
        assert_eq!(derive_case_type(&p), "pln-123");
    }

    #[test]
    fn r531_derive_trims_key_whitespace() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("  support  ");
        assert_eq!(derive_case_type(&p), "support");
    }

    #[test]
    fn r531_derive_preserves_key_with_internal_whitespace() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("support urgent");
        assert_eq!(derive_case_type(&p), "support urgent");
    }

    #[test]
    fn r531_matches_returns_true_when_declared_is_none() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("support");
        assert!(case_type_matches_pipeline(None, &p));
    }

    #[test]
    fn r531_matches_returns_true_when_declared_is_empty() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("support");
        assert!(case_type_matches_pipeline(Some(""), &p));
    }

    #[test]
    fn r531_matches_returns_true_when_declared_equals_key() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("support");
        assert!(case_type_matches_pipeline(Some("support"), &p));
    }

    #[test]
    fn r531_matches_returns_false_when_declared_differs_from_key() {
        let p = CaseTypePipelineRef::new("pln-123").with_key("support");
        assert!(!case_type_matches_pipeline(Some("billing"), &p));
    }

    #[test]
    fn r531_matches_uses_id_fallback_when_key_missing() {
        // When pipeline has no key, derive uses id; declared must match id.
        let p = CaseTypePipelineRef::new("pln-123");
        assert!(case_type_matches_pipeline(Some("pln-123"), &p));
        assert!(!case_type_matches_pipeline(Some("support"), &p));
    }

    #[test]
    fn r531_matches_uses_id_fallback_when_key_empty() {
        // Same as above but with explicit empty key.
        let p = CaseTypePipelineRef::new("pln-123").with_key("");
        assert!(case_type_matches_pipeline(Some("pln-123"), &p));
        assert!(!case_type_matches_pipeline(Some("support"), &p));
    }

    #[test]
    fn r531_matches_exact_string_comparison() {
        // Not case-insensitive: "Support" ≠ "support"
        let p = CaseTypePipelineRef::new("pln-123").with_key("support");
        assert!(!case_type_matches_pipeline(Some("Support"), &p));
    }

    #[test]
    fn r531_matches_no_trim_on_declared() {
        // Node doesn't trim declared, only the derived value (key).
        let p = CaseTypePipelineRef::new("pln-123").with_key("support");
        // declared " support" has leading space; derive gives "support"
        assert!(!case_type_matches_pipeline(Some(" support"), &p));
    }
}
