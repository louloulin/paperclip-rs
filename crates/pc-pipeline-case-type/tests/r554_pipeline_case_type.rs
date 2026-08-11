//! R554 — pc-pipeline-case-type 综合测试。

#![allow(clippy::doc_markdown)]

use pc_pipeline_case_type::{case_type_matches_pipeline, derive_case_type, CaseTypePipelineRef};

fn ref_with(id: &str, key: Option<&str>) -> CaseTypePipelineRef {
    CaseTypePipelineRef {
        id: id.into(),
        key: key.map(str::to_string),
    }
}

#[test]
fn r554_derive_prefers_key() {
    let p = ref_with("pl-1", Some("release-notes"));
    assert_eq!(derive_case_type(&p), "release-notes");
}

#[test]
fn r554_derive_falls_back_to_id_when_key_none() {
    let p = ref_with("pl-1", None);
    assert_eq!(derive_case_type(&p), "pl-1");
}

#[test]
fn r554_derive_falls_back_to_id_when_key_empty() {
    let p = ref_with("pl-1", Some(""));
    assert_eq!(derive_case_type(&p), "pl-1");
}

#[test]
fn r554_derive_trims_whitespace() {
    let p = ref_with("pl-1", Some("   release-notes   "));
    assert_eq!(derive_case_type(&p), "release-notes");
}

#[test]
fn r554_derive_whitespace_key_falls_back() {
    let p = ref_with("pl-1", Some("   "));
    assert_eq!(derive_case_type(&p), "pl-1");
}

#[test]
fn r554_match_none_returns_true() {
    let p = ref_with("pl-1", Some("release-notes"));
    assert!(case_type_matches_pipeline(None, &p));
}

#[test]
fn r554_match_empty_returns_true() {
    let p = ref_with("pl-1", Some("release-notes"));
    assert!(case_type_matches_pipeline(Some(""), &p));
}

#[test]
fn r554_match_agreeing_returns_true() {
    let p = ref_with("pl-1", Some("release-notes"));
    assert!(case_type_matches_pipeline(Some("release-notes"), &p));
}

#[test]
fn r554_match_disagreeing_returns_false() {
    let p = ref_with("pl-1", Some("release-notes"));
    assert!(!case_type_matches_pipeline(Some("bugs"), &p));
}

#[test]
fn r554_match_derived_matches_id_fallback() {
    // When key is missing, derive_case_type returns the id; declared type
    // matching the id should be accepted as agreeing.
    let p = ref_with("pl-1", None);
    assert!(case_type_matches_pipeline(Some("pl-1"), &p));
    assert!(!case_type_matches_pipeline(Some("anything-else"), &p));
}
