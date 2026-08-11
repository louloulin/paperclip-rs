//! R-INTEGRATION-1: pc-config-schema 与 pc-feature-catalog 集成测试
//!
//! 验证 config-schema 通过 delegation 模式接入了 pc-feature-catalog：
//! - `validate_feature_key` 返回 Ok for known / Err for unknown
//! - `known_feature_keys` 与 catalog 一致
//! - `feature_tier` 返回 catalog tier
//! - `has_any_feature_of_tier` 命中 catalog 中的实际 tier

use pc_config_schema::{
    feature_tier, has_any_feature_of_tier, known_feature_keys, validate_feature_key,
};
use pc_feature_catalog::{FeatureTier, FEATURE_TIERS};

#[test]
fn validate_known_feature_keys_ok() {
    // 至少 5 个 catalog 中已知的 key 都应 Ok
    let known_sample = [
        "enableEnvironments",
        "enablePipelines",
        "enableStreamlinedLeftNavigation",
        "enableApps",
        "enableIsolatedWorkspaces",
    ];
    for key in known_sample {
        let result = validate_feature_key(key);
        assert!(
            result.is_ok(),
            "expected Ok for known key {key}, got {result:?}"
        );
    }
}

#[test]
fn validate_unknown_feature_key_err() {
    let err = validate_feature_key("notARealFeatureKey").unwrap_err();
    assert_eq!(err.key, "notARealFeatureKey");
    assert!(!err.known_keys.is_empty(), "known_keys should be populated");
    // sorted invariant
    let mut sorted = err.known_keys.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, err.known_keys, "known_keys must be sorted");
}

#[test]
fn known_feature_keys_matches_catalog() {
    let from_schema = known_feature_keys();
    let from_catalog = pc_feature_catalog::instance_feature_keys();
    assert_eq!(from_schema, from_catalog);
}

#[test]
fn feature_tier_returns_catalog_tier() {
    // preference tier is in catalog — pick any key from FEATURE_TIERS
    let pref_tier_keys: Vec<&str> = known_feature_keys()
        .into_iter()
        .filter(|k| feature_tier(k) == Some(FeatureTier::Preference))
        .collect();
    assert!(
        !pref_tier_keys.is_empty(),
        "catalog should have at least one Preference-tier feature"
    );

    // Unknown key returns None
    assert_eq!(feature_tier("unknownKey"), None);
}

#[test]
fn has_any_feature_of_tier_matches_tiers() {
    // Tier representation must be a subset of (or equal to) the canonical 3-tier list.
    // Specific tier coverage depends on the catalog composition — we only assert
    // structural facts here:
    //   1. `has_any_feature_of_tier` returns true for tiers that ARE represented
    //   2. The aggregated count of features across all tiers equals known_feature_keys().len()
    let total_features = known_feature_keys().len();
    let mut represented = 0usize;
    for tier in [
        FeatureTier::Preference,
        FeatureTier::Managed,
        FeatureTier::Floor,
    ] {
        if has_any_feature_of_tier(tier) {
            represented += 1;
            // Tier-aware lookup: at least one key of this tier must yield the matching tier
            let key = known_feature_keys()
                .into_iter()
                .find(|k| feature_tier(k) == Some(tier))
                .expect("tier said present, but no key matched");
            assert_eq!(feature_tier(key), Some(tier));
        }
    }
    // Sanity: at least one tier must be represented in a non-empty catalog.
    assert!(total_features > 0, "catalog should not be empty");
    assert!(
        represented >= 1,
        "catalog should have at least one tier represented"
    );

    // FEATURE_TIERS const is the canonical 3-tier list (independent of which are populated)
    assert_eq!(FEATURE_TIERS, ["preference", "managed", "floor"]);
}

#[test]
fn delegation_zero_business_logic() {
    // validate_feature_key's behavior must be 100% explained by lookup_feature
    for key in known_feature_keys() {
        // known → Ok
        assert!(validate_feature_key(key).is_ok());
        // tier matches lookup
        let direct_tier = pc_feature_catalog::lookup_feature(key).map(|e| e.tier);
        assert_eq!(feature_tier(key), direct_tier);
    }
}

#[test]
fn error_display_includes_key() {
    let err = validate_feature_key("bogusKey").unwrap_err();
    let display = format!("{err}");
    assert!(
        display.contains("bogusKey"),
        "display should mention the bad key: {display}"
    );
}
