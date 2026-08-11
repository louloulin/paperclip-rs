#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Trust / low-trust-review policy types and constants.
//!
//! R551: Direct port of `paperclip/packages/shared/src/trust-policy.ts` (67 LOC).
//! Pure type definitions + constants + a few helpers.

use std::collections::HashSet;

// ---------- constants ----------

pub const TRUST_PRESETS: [&str; 2] = ["standard", "low_trust_review"];
pub const DEFAULT_TRUST_PRESET: &str = "standard";
pub const LOW_TRUST_REVIEW_PRESET: &str = "low_trust_review";
pub const LOW_TRUST_REVIEW_PRESET_VERSION: u32 = 1;
pub const LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION: &str = "quarantine";

pub const LOW_TRUST_TOOL_CLASSES: [&str; 3] = ["git.read", "github.pr.read", "tests.local"];

// ---------- enums ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPreset {
    Standard,
    LowTrustReview,
}

impl TrustPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::LowTrustReview => "low_trust_review",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            "low_trust_review" => Some(Self::LowTrustReview),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceTrustArtifactKind {
    Issue,
    Comment,
    Document,
    WorkProduct,
}

impl SourceTrustArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Comment => "comment",
            Self::Document => "document",
            Self::WorkProduct => "work_product",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "issue" => Some(Self::Issue),
            "comment" => Some(Self::Comment),
            "document" => Some(Self::Document),
            "work_product" => Some(Self::WorkProduct),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceTrustDisposition {
    Quarantined,
    Promoted,
}

impl SourceTrustDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quarantined => "quarantined",
            Self::Promoted => "promoted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "quarantined" => Some(Self::Quarantined),
            "promoted" => Some(Self::Promoted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromotedByActorType {
    Agent,
    User,
    System,
}

impl PromotedByActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

// ---------- structs ----------

#[derive(Debug, Clone)]
pub struct LowTrustOutputPromotionTarget {
    pub r#type: LowTrustPromotionTargetType, // 'type' is reserved
    pub issue_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LowTrustPromotionTargetType {
    Issue,
}

impl LowTrustPromotionTargetType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LowTrustBoundary {
    pub mode: Option<String>,
    pub company_id: Option<String>,
    pub project_ids: Vec<String>,
    pub root_issue_id: Option<String>,
    pub issue_ids: Vec<String>,
    pub allowed_agent_ids: Vec<String>,
    pub allowed_secret_binding_ids: Vec<String>,
    pub allowed_tool_classes: Vec<String>,
    pub output_promotion_target: Option<LowTrustOutputPromotionTarget>,
}

#[derive(Debug, Clone)]
pub struct LowTrustReviewPresetPolicy {
    pub id: String,
    pub version: u32,
    pub raw_output_disposition: String,
}

impl LowTrustReviewPresetPolicy {
    /// Canonical low-trust-review preset policy, matches Node constants.
    pub fn canonical() -> Self {
        Self {
            id: LOW_TRUST_REVIEW_PRESET.to_string(),
            version: LOW_TRUST_REVIEW_PRESET_VERSION,
            raw_output_disposition: LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION.to_string(),
        }
    }
}

/// Trust authorization policy — extensible via `extra` for forward compatibility.
#[derive(Debug, Clone, Default)]
pub struct TrustAuthorizationPolicy {
    pub trust_preset: Option<TrustPreset>,
    pub review_preset: Option<LowTrustReviewPresetPolicy>,
    pub trust_boundary: Option<LowTrustBoundary>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct SourceTrustPromotionSource {
    pub artifact_kind: SourceTrustArtifactKind,
    pub artifact_id: String,
    pub issue_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SourceTrustMetadata {
    pub preset: TrustPreset,
    pub disposition: SourceTrustDisposition,
    pub source_issue_id: Option<String>,
    pub source_run_id: Option<String>,
    pub source_agent_id: Option<String>,
    pub promoted_from: Option<SourceTrustPromotionSource>,
    pub promoted_by_actor_type: Option<PromotedByActorType>,
    pub promoted_by_actor_id: Option<String>,
    pub promoted_at: Option<String>,
}

// ---------- helpers ----------

/// Quick check whether a tool class name is one of the known low-trust classes.
pub fn is_low_trust_tool_class(class: &str) -> bool {
    LOW_TRUST_TOOL_CLASSES.contains(&class)
}

/// Set view of the low-trust tool classes.
pub fn low_trust_tool_classes_set() -> HashSet<&'static str> {
    LOW_TRUST_TOOL_CLASSES.into_iter().collect()
}

/// Helper: build a canonical low-trust-review policy.
pub fn low_trust_review_policy() -> LowTrustReviewPresetPolicy {
    LowTrustReviewPresetPolicy::canonical()
}

/// Determine whether `policy` indicates the low_trust_review preset.
pub fn is_low_trust_review(policy: &TrustAuthorizationPolicy) -> bool {
    matches!(policy.trust_preset, Some(TrustPreset::LowTrustReview))
        || policy
            .review_preset
            .as_ref()
            .is_some_and(|p| p.id == LOW_TRUST_REVIEW_PRESET)
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn trust_preset_round_trip() {
        for s in TRUST_PRESETS {
            assert_eq!(TrustPreset::parse(s).unwrap().as_str(), s);
        }
        assert!(TrustPreset::parse("invalid").is_none());
    }

    #[test]
    fn artifact_kind_round_trip() {
        for k in [
            SourceTrustArtifactKind::Issue,
            SourceTrustArtifactKind::Comment,
            SourceTrustArtifactKind::Document,
            SourceTrustArtifactKind::WorkProduct,
        ] {
            let s = k.as_str();
            assert_eq!(SourceTrustArtifactKind::parse(s), Some(k));
        }
    }

    #[test]
    fn canonical_policy_values() {
        let p = LowTrustReviewPresetPolicy::canonical();
        assert_eq!(p.id, LOW_TRUST_REVIEW_PRESET);
        assert_eq!(p.version, LOW_TRUST_REVIEW_PRESET_VERSION);
        assert_eq!(
            p.raw_output_disposition,
            LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION
        );
    }

    #[test]
    fn low_trust_tool_classes_match_node() {
        assert!(is_low_trust_tool_class("git.read"));
        assert!(is_low_trust_tool_class("github.pr.read"));
        assert!(is_low_trust_tool_class("tests.local"));
        assert!(!is_low_trust_tool_class("unknown"));
    }

    #[test]
    fn is_low_trust_review_detects_preset() {
        let policy = TrustAuthorizationPolicy {
            trust_preset: Some(TrustPreset::LowTrustReview),
            ..Default::default()
        };
        assert!(is_low_trust_review(&policy));
    }
}
