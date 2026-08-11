//! R569 — R-INTEGRATION-9: pc-trust-policy → pc-authz delegation tests.
//!
//! Verifies that `pc_authz::trust` correctly delegates to `pc_trust_policy`
//! for the `TrustPreset` enum + LOW_TRUST_* constants, eliminating DRY
//! duplication while preserving API backward compatibility.

use pc_trust_policy::{
    TrustPreset as CanonicalTrustPreset, LOW_TRUST_REVIEW_PRESET, LOW_TRUST_REVIEW_PRESET_VERSION,
    LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION, TRUST_PRESETS,
};
use serde_json;

#[test]
fn r569_trust_preset_alias_matches_canonical() {
    // pc-authz::trust::TrustPreset should be the SAME type as pc_trust_policy::TrustPreset
    // (re-export). Type identity check via std::any::type_name.
    assert_eq!(
        std::any::type_name::<pc_authz::trust::TrustPreset>(),
        std::any::type_name::<CanonicalTrustPreset>()
    );
}

#[test]
fn r569_trust_preset_variants_round_trip() {
    let a = pc_authz::trust::TrustPreset::Standard;
    let b = pc_authz::trust::TrustPreset::LowTrustReview;

    assert_eq!(a.as_str(), "standard");
    assert_eq!(b.as_str(), "low_trust_review");
    assert_eq!(CanonicalTrustPreset::parse("standard"), Some(a));
    assert_eq!(CanonicalTrustPreset::parse("low_trust_review"), Some(b));
    assert_eq!(CanonicalTrustPreset::parse("nope"), None);
}

#[test]
fn r569_trust_preset_serializes_snake_case() {
    // pc-trust-policy added Serialize + serde(rename_all = "snake_case")
    // so JSON shape matches Node upstream ("standard" / "low_trust_review").
    let s = serde_json::to_string(&pc_authz::trust::TrustPreset::Standard).unwrap();
    assert_eq!(s, "\"standard\"");
    let l = serde_json::to_string(&pc_authz::trust::TrustPreset::LowTrustReview).unwrap();
    assert_eq!(l, "\"low_trust_review\"");
}

#[test]
fn r569_trust_preset_deserializes_snake_case() {
    let s: pc_authz::trust::TrustPreset = serde_json::from_str("\"standard\"").unwrap();
    assert_eq!(s, pc_authz::trust::TrustPreset::Standard);
    let l: pc_authz::trust::TrustPreset = serde_json::from_str("\"low_trust_review\"").unwrap();
    assert_eq!(l, pc_authz::trust::TrustPreset::LowTrustReview);
}

#[test]
fn r569_low_trust_constants_match() {
    assert_eq!(
        pc_authz::trust::LOW_TRUST_REVIEW_PRESET,
        LOW_TRUST_REVIEW_PRESET
    );
    assert_eq!(
        pc_authz::trust::LOW_TRUST_REVIEW_PRESET_VERSION,
        LOW_TRUST_REVIEW_PRESET_VERSION
    );
    assert_eq!(
        pc_authz::trust::LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION,
        LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION
    );
}

#[test]
fn r569_trust_preset_from_str_opt_compat_alias() {
    // Legacy callers using `from_str_opt` style should resolve via the new
    // delegation helper.
    assert_eq!(
        pc_authz::trust::trust_preset_from_str_opt("standard"),
        Some(pc_authz::trust::TrustPreset::Standard)
    );
    assert_eq!(
        pc_authz::trust::trust_preset_from_str_opt("low_trust_review"),
        Some(pc_authz::trust::TrustPreset::LowTrustReview)
    );
    assert_eq!(pc_authz::trust::trust_preset_from_str_opt("garbage"), None);
}

#[test]
fn r569_low_trust_issue_ancestry_max_depth_preserved() {
    // Authz-specific resolver detail — not delegated to pc-trust-policy.
    assert_eq!(pc_authz::trust::LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH, 12);
}

#[test]
fn r569_trust_presets_constant_in_sync() {
    for preset in TRUST_PRESETS {
        let parsed = CanonicalTrustPreset::parse(preset);
        assert!(
            parsed.is_some(),
            "TRUST_PRESETS contains `{preset}` which is not parseable"
        );
    }
}
