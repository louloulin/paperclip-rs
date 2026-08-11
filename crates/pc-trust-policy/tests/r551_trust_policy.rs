//! R551 — pc-trust-policy 综合测试。

#![allow(clippy::doc_markdown)]

use pc_trust_policy::{
    is_low_trust_review, is_low_trust_tool_class, low_trust_review_policy,
    low_trust_tool_classes_set, PromotedByActorType, SourceTrustArtifactKind,
    SourceTrustDisposition, SourceTrustPromotionSource, TrustAuthorizationPolicy, TrustPreset,
    DEFAULT_TRUST_PRESET, LOW_TRUST_REVIEW_PRESET, LOW_TRUST_REVIEW_PRESET_VERSION,
    LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION, LOW_TRUST_TOOL_CLASSES, TRUST_PRESETS,
};

#[test]
fn r551_constants_match_node() {
    assert_eq!(TRUST_PRESETS, ["standard", "low_trust_review"]);
    assert_eq!(DEFAULT_TRUST_PRESET, "standard");
    assert_eq!(LOW_TRUST_REVIEW_PRESET, "low_trust_review");
    assert_eq!(LOW_TRUST_REVIEW_PRESET_VERSION, 1);
    assert_eq!(LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION, "quarantine");
    assert_eq!(
        LOW_TRUST_TOOL_CLASSES,
        ["git.read", "github.pr.read", "tests.local"]
    );
}

#[test]
fn r551_trust_preset_round_trip() {
    for s in TRUST_PRESETS {
        let parsed = TrustPreset::parse(s).unwrap();
        assert_eq!(parsed.as_str(), s);
    }
    assert!(TrustPreset::parse("nope").is_none());
}

#[test]
fn r551_artifact_kind_round_trip() {
    let kinds = [
        SourceTrustArtifactKind::Issue,
        SourceTrustArtifactKind::Comment,
        SourceTrustArtifactKind::Document,
        SourceTrustArtifactKind::WorkProduct,
    ];
    for k in kinds {
        let s = k.as_str();
        assert_eq!(SourceTrustArtifactKind::parse(s), Some(k));
    }
    assert!(SourceTrustArtifactKind::parse("nope").is_none());
}

#[test]
fn r551_disposition_round_trip() {
    assert_eq!(SourceTrustDisposition::Quarantined.as_str(), "quarantined");
    assert_eq!(SourceTrustDisposition::Promoted.as_str(), "promoted");
    assert_eq!(
        SourceTrustDisposition::parse("quarantined"),
        Some(SourceTrustDisposition::Quarantined)
    );
    assert_eq!(
        SourceTrustDisposition::parse("promoted"),
        Some(SourceTrustDisposition::Promoted)
    );
    assert!(SourceTrustDisposition::parse("nope").is_none());
}

#[test]
fn r551_promoted_by_actor_round_trip() {
    assert_eq!(PromotedByActorType::Agent.as_str(), "agent");
    assert_eq!(PromotedByActorType::User.as_str(), "user");
    assert_eq!(PromotedByActorType::System.as_str(), "system");
    for v in ["agent", "user", "system"] {
        assert_eq!(
            PromotedByActorType::parse(v),
            Some(match v {
                "agent" => PromotedByActorType::Agent,
                "user" => PromotedByActorType::User,
                "system" => PromotedByActorType::System,
                _ => unreachable!(),
            })
        );
    }
}

#[test]
fn r551_low_trust_tool_class_set() {
    let set = low_trust_tool_classes_set();
    assert_eq!(set.len(), 3);
    assert!(set.contains("git.read"));
    assert!(set.contains("github.pr.read"));
    assert!(set.contains("tests.local"));
}

#[test]
fn r551_is_low_trust_tool_class() {
    for class in LOW_TRUST_TOOL_CLASSES {
        assert!(is_low_trust_tool_class(class));
    }
    assert!(!is_low_trust_tool_class("github.pr.write"));
    assert!(!is_low_trust_tool_class(""));
}

#[test]
fn r551_low_trust_review_policy_canonical() {
    let policy = low_trust_review_policy();
    assert_eq!(policy.id, LOW_TRUST_REVIEW_PRESET);
    assert_eq!(policy.version, LOW_TRUST_REVIEW_PRESET_VERSION);
    assert_eq!(
        policy.raw_output_disposition,
        LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION
    );
}

#[test]
fn r551_is_low_trust_review_true_when_preset_set() {
    let policy = TrustAuthorizationPolicy {
        trust_preset: Some(TrustPreset::LowTrustReview),
        ..Default::default()
    };
    assert!(is_low_trust_review(&policy));
}

#[test]
fn r551_is_low_trust_review_true_when_review_preset_set() {
    let policy = TrustAuthorizationPolicy {
        review_preset: Some(low_trust_review_policy()),
        ..Default::default()
    };
    assert!(is_low_trust_review(&policy));
}

#[test]
fn r551_is_low_trust_review_false_when_standard() {
    let policy = TrustAuthorizationPolicy {
        trust_preset: Some(TrustPreset::Standard),
        ..Default::default()
    };
    assert!(!is_low_trust_review(&policy));
}

#[test]
fn r551_is_low_trust_review_false_when_empty() {
    let policy = TrustAuthorizationPolicy::default();
    assert!(!is_low_trust_review(&policy));
}

#[test]
fn r551_source_trust_promotion_source() {
    let src = SourceTrustPromotionSource {
        artifact_kind: SourceTrustArtifactKind::Document,
        artifact_id: "doc-1".into(),
        issue_id: Some("iss-1".into()),
    };
    assert_eq!(src.artifact_kind.as_str(), "document");
    assert_eq!(src.artifact_id, "doc-1");
    assert_eq!(src.issue_id.as_deref(), Some("iss-1"));
}
