//! Pipeline case type — thin delegation to `pc-pipeline-case-type`.
//!
//! R-INTEGRATION-3 (R563): the case_type helpers previously lived in this
//! module as a duplicate implementation. They have been consolidated so that
//! `pc-pipeline-case-type` (R554) is the single source of truth. This module
//! re-exports the canonical API under the `pc_pipelines::case_type::` path so
//! existing callers (and the test suite) continue to compile unchanged.
//!
//! What stays local:
//! - `CaseTypePipelineRef` newtype (kept as a thin wrapper to avoid breaking
//!   the public surface; auto-deref / conversion delegates to the canonical
//!   type)
//! - 3 integration tests that exercise the delegation end-to-end
//!
//! What moved:
//! - `derive_case_type` and `case_type_matches_pipeline` → `pc-pipeline-case-type`

use pc_pipeline_case_type as canonical;

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

    /// Convert to the canonical `pc_pipeline_case_type::CaseTypePipelineRef`.
    fn to_canonical(&self) -> canonical::CaseTypePipelineRef {
        canonical::CaseTypePipelineRef {
            id: self.id.clone(),
            key: self.key.clone(),
        }
    }
}

/// Derive a case's "type" from its pipeline reference.
///
/// Mirrors Node upstream `deriveCaseType`. The pipeline key (trimmed) is the
/// canonical type identifier; we fall back to the pipeline id if a key is
/// somehow absent. Delegates to `pc_pipeline_case_type::derive_case_type`.
pub fn derive_case_type(pipeline: &CaseTypePipelineRef) -> String {
    canonical::derive_case_type(&pipeline.to_canonical())
}

/// Sanity check: does the declared case_type (as ingested) match the
/// pipeline's derived case_type?
///
/// Mirrors Node upstream `caseTypeMatchesPipeline`. Delegates to
/// `pc_pipeline_case_type::case_type_matches_pipeline`.
pub fn case_type_matches_pipeline(
    declared_case_type: Option<&str>,
    pipeline: &CaseTypePipelineRef,
) -> bool {
    canonical::case_type_matches_pipeline(declared_case_type, &pipeline.to_canonical())
}

#[cfg(test)]
mod delegation_tests {
    use super::*;

    #[test]
    fn delegate_derive_uses_key_when_present() {
        let p = CaseTypePipelineRef::new("pln-abc").with_key("support");
        assert_eq!(derive_case_type(&p), "support");
    }

    #[test]
    fn delegate_derive_falls_back_to_id_when_no_key() {
        let p = CaseTypePipelineRef::new("pln-xyz");
        assert_eq!(derive_case_type(&p), "pln-xyz");
    }

    #[test]
    fn delegate_matches_handles_none_empty_some_and_mismatch() {
        let p = CaseTypePipelineRef::new("pln-1").with_key("k");

        // declared None → true (no correction needed)
        assert!(case_type_matches_pipeline(None, &p));
        // declared Some("") → true (no correction needed)
        assert!(case_type_matches_pipeline(Some(""), &p));
        // declared Some("k") → match
        assert!(case_type_matches_pipeline(Some("k"), &p));
        // declared Some("other") → no match
        assert!(!case_type_matches_pipeline(Some("other"), &p));
    }

    #[test]
    fn delegation_produces_same_results_as_canonical_directly() {
        // Verifies the wrapper is faithful (zero semantic drift)
        let p_local = CaseTypePipelineRef::new("pln-abc").with_key(" support ");
        let p_canonical = canonical::CaseTypePipelineRef {
            id: "pln-abc".to_string(),
            key: Some(" support ".to_string()),
        };

        assert_eq!(
            derive_case_type(&p_local),
            canonical::derive_case_type(&p_canonical),
        );
        assert_eq!(
            case_type_matches_pipeline(Some("support"), &p_local),
            canonical::case_type_matches_pipeline(Some("support"), &p_canonical),
        );
    }
}
