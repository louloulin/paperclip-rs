//! R556 — pc-feature-catalog 综合测试。

#![allow(clippy::doc_markdown)]

use pc_feature_catalog::{
    build_feature_catalog_artifact, instance_feature_keys, lookup_feature,
    render_feature_catalog_artifact, FeatureTier, FEATURE_TIERS, INSTANCE_FEATURE_CATALOG,
};
use serde_json::Value;

#[test]
fn r556_feature_tiers_constants() {
    assert_eq!(FEATURE_TIERS, ["preference", "managed", "floor"]);
}

#[test]
fn r556_feature_tier_round_trip() {
    for t in [
        FeatureTier::Preference,
        FeatureTier::Managed,
        FeatureTier::Floor,
    ] {
        let s = t.as_str();
        assert_eq!(FeatureTier::parse(s), Some(t));
    }
    assert!(FeatureTier::parse("nope").is_none());
}

#[test]
fn r556_catalog_size() {
    assert_eq!(INSTANCE_FEATURE_CATALOG.len(), 26);
}

#[test]
fn r556_lookup_each_entry() {
    for (key, entry) in INSTANCE_FEATURE_CATALOG {
        let looked_up = lookup_feature(key).expect(key);
        assert_eq!(looked_up.title, entry.title);
        assert_eq!(looked_up.tier, entry.tier);
        assert_eq!(looked_up.cloud_default, entry.cloud_default);
        assert_eq!(looked_up.self_hosted_default, entry.self_hosted_default);
    }
}

#[test]
fn r556_lookup_unknown_returns_none() {
    assert!(lookup_feature("enableUnknownFlag").is_none());
    assert!(lookup_feature("").is_none());
}

#[test]
fn r556_known_flags_present() {
    for key in [
        "enableEnvironments",
        "enableApps",
        "enablePipelines",
        "enableCases",
        "enableConferenceRoomChat",
        "enableIssueGraphLivenessAutoRecovery",
        "enableWorktreeRunExecution",
    ] {
        assert!(lookup_feature(key).is_some(), "missing {key}");
    }
}

#[test]
fn r556_keys_sorted_alphabetically() {
    let keys = instance_feature_keys();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    assert!(keys.contains(&"enableEnvironments"));
    assert!(keys.contains(&"enableWorktreeRunExecution"));
}

#[test]
fn r556_build_artifact_rejects_empty_version() {
    assert!(build_feature_catalog_artifact("").is_err());
    assert!(build_feature_catalog_artifact("   ").is_err());
}

#[test]
fn r556_build_artifact_contains_all_keys() {
    let artifact = build_feature_catalog_artifact("v1").unwrap();
    assert_eq!(artifact["catalogVersion"], "v1");
    let features = artifact["features"].as_object().unwrap();
    for key in instance_feature_keys() {
        assert!(features.contains_key(key), "missing {key}");
        let entry = lookup_feature(key).unwrap();
        assert_eq!(
            features[key]["tier"],
            Value::String(entry.tier.as_str().into())
        );
    }
}

#[test]
fn r556_build_artifact_keys_count_matches() {
    let artifact = build_feature_catalog_artifact("v1").unwrap();
    let features = artifact["features"].as_object().unwrap();
    assert_eq!(features.len(), INSTANCE_FEATURE_CATALOG.len());
}

#[test]
fn r556_render_artifact_is_deterministic() {
    let a = render_feature_catalog_artifact("v1").unwrap();
    let b = render_feature_catalog_artifact("v1").unwrap();
    assert_eq!(a, b);
    assert!(a.ends_with('\n'));
}

#[test]
fn r556_render_artifact_rejects_empty_version() {
    assert!(render_feature_catalog_artifact("").is_err());
}

#[test]
fn r556_render_artifact_is_valid_json() {
    let rendered = render_feature_catalog_artifact("v1").unwrap();
    let parsed: Value = serde_json::from_str(rendered.trim_end()).unwrap();
    assert_eq!(parsed["catalogVersion"], "v1");
}

#[test]
fn r556_cloud_self_hosted_defaults_consistency() {
    for (key, entry) in INSTANCE_FEATURE_CATALOG {
        // Node catalog has selfHostedDefault equal to schema default
        // We don't have schema here, but we document expected defaults.
        if matches!(
            key,
            &"enableStreamlinedLeftNavigation"
                | &"enableWorkspaceBranchReconcileForward"
                | &"enableWorkspaceDirtyQuarantineRepair"
        ) {
            assert!(entry.cloud_default, "{key} should have cloud_default=true");
            assert!(
                entry.self_hosted_default,
                "{key} should have self_hosted_default=true"
            );
        }
    }
}

#[test]
fn r556_all_tiers_are_known() {
    for (key, entry) in INSTANCE_FEATURE_CATALOG {
        assert!(
            FEATURE_TIERS.contains(&entry.tier.as_str()),
            "{key} has unknown tier {:?}",
            entry.tier
        );
    }
}
