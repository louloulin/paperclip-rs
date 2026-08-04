//! Source trust 纯规则层（对齐 Node `server/src/services/source-trust.ts` 纯逻辑部分）。
//!
//! 单一职责：
//! - 判断某个 `SourceTrustMetadata` 是否处于「quarantined low-trust」状态
//! - 把 quarantined 内容 redact / sanitize 后再喂给 higher-trust agent
//! - 构造 `low_trust_review` preset 的 `quarantined` / `promoted` metadata
//!
//! 不持有任何 IO 状态；DB 适配由 `pc_repos::source_trust` 提供。

use serde::{Deserialize, Serialize};

/// Low-trust 处置的预设名（与 Node `LOW_TRUST_REVIEW_PRESET` 1:1 对齐）。
pub const LOW_TRUST_REVIEW_PRESET: &str = "low_trust_review";

/// 默认 trust preset（与 Node `DEFAULT_TRUST_PRESET` 1:1 对齐）。
pub const DEFAULT_TRUST_PRESET: &str = "standard";

/// Trust preset 名（与 Node `TrustPreset` 1:1 对齐）。
///
/// 注：Rust 端保留为 `String` 而非 enum，因为 Node 端允许 `TrustPreset = string`（trust-policy.ts）。
pub type TrustPreset = String;

/// Source trust disposition（与 Node `SourceTrustDisposition` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceTrustDisposition {
    Quarantined,
    Promoted,
}

impl SourceTrustDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quarantined => "quarantined",
            Self::Promoted => "promoted",
        }
    }
}

/// Source trust promotion source artifact kind（与 Node `SourceTrustArtifactKind` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTrustArtifactKind {
    Comment,
    Document,
    WorkProduct,
    Issue,
}

impl SourceTrustArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Document => "document",
            Self::WorkProduct => "work_product",
            Self::Issue => "issue",
        }
    }
}

/// Source trust promotion 来源（与 Node `SourceTrustPromotionSource` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTrustPromotionSource {
    #[serde(rename = "artifactKind")]
    pub artifact_kind: SourceTrustArtifactKind,
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    #[serde(rename = "issueId", skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
}

/// Actor 类型（与 Node `promotedByActorType` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromotedByActorType {
    Agent,
    User,
    System,
}

impl PromotedByActorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::System => "system",
        }
    }
}

/// Source trust metadata（与 Node `SourceTrustMetadata` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTrustMetadata {
    pub preset: TrustPreset,
    pub disposition: SourceTrustDisposition,
    #[serde(rename = "sourceIssueId", skip_serializing_if = "Option::is_none")]
    pub source_issue_id: Option<String>,
    #[serde(rename = "sourceRunId", skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(rename = "sourceAgentId", skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(rename = "promotedFrom", skip_serializing_if = "Option::is_none")]
    pub promoted_from: Option<SourceTrustPromotionSource>,
    #[serde(
        rename = "promotedByActorType",
        skip_serializing_if = "Option::is_none"
    )]
    pub promoted_by_actor_type: Option<PromotedByActorType>,
    #[serde(rename = "promotedByActorId", skip_serializing_if = "Option::is_none")]
    pub promoted_by_actor_id: Option<String>,
    #[serde(rename = "promotedAt", skip_serializing_if = "Option::is_none")]
    pub promoted_at: Option<String>,
}

/// Quarantined body 占位文案（与 Node `LOW_TRUST_QUARANTINED_BODY` 1:1 对齐）。
pub const LOW_TRUST_QUARANTINED_BODY: &str =
    "[Quarantined low-trust output omitted from higher-trust agent context. A trusted reviewer can inspect and promote a sanitized artifact.]";

/// 判断 sourceTrust 是否处于 quarantined low-trust 状态（与 Node `isLowTrustQuarantined` 1:1 对齐）。
#[must_use]
pub fn is_low_trust_quarantined(source_trust: Option<&SourceTrustMetadata>) -> bool {
    match source_trust {
        Some(st) => {
            st.preset == LOW_TRUST_REVIEW_PRESET
                && st.disposition == SourceTrustDisposition::Quarantined
        }
        None => false,
    }
}

/// 把 quarantined value 的 `body` 替换为占位文案（与 Node `redactQuarantinedBodyForHigherTrust` 1:1 对齐）。
///
/// 仅当 `isLowTrustQuarantined(value.sourceTrust)` 为 true 时替换 body；其他情况原样返回。
pub fn redact_quarantined_body_for_higher_trust<T>(value: T) -> T
where
    T: SourceTrustRedactable,
{
    if !is_low_trust_quarantined(value.source_trust_ref()) {
        return value;
    }
    value.with_redacted_body(LOW_TRUST_QUARANTINED_BODY)
}

/// 把 quarantined comment 的 body / presentation / metadata 全部置空（与 Node `sanitizeQuarantinedCommentForHigherTrust` 1:1 对齐）。
pub fn sanitize_quarantined_comment_for_higher_trust<T>(comment: T) -> T
where
    T: SourceTrustCommentSanitizable,
{
    if !is_low_trust_quarantined(comment.source_trust_ref()) {
        return comment;
    }
    comment.with_sanitized(LOW_TRUST_QUARANTINED_BODY)
}

/// 构造 low-trust quarantined source trust（与 Node `buildLowTrustSourceTrust` 1:1 对齐）。
#[must_use]
pub fn build_low_trust_source_trust(input: BuildLowTrustSourceTrustInput) -> SourceTrustMetadata {
    SourceTrustMetadata {
        preset: LOW_TRUST_REVIEW_PRESET.to_string(),
        disposition: SourceTrustDisposition::Quarantined,
        source_issue_id: Some(input.issue_id),
        source_run_id: input.run_id,
        source_agent_id: input.agent_id,
        promoted_from: None,
        promoted_by_actor_type: None,
        promoted_by_actor_id: None,
        promoted_at: None,
    }
}

/// 构造 low-trust promoted source trust（与 Node `buildPromotedSourceTrust` 1:1 对齐）。
#[must_use]
pub fn build_promoted_source_trust(input: BuildPromotedSourceTrustInput) -> SourceTrustMetadata {
    SourceTrustMetadata {
        preset: LOW_TRUST_REVIEW_PRESET.to_string(),
        disposition: SourceTrustDisposition::Promoted,
        source_issue_id: Some(input.source_issue_id.clone()),
        source_run_id: None,
        source_agent_id: None,
        promoted_from: Some(SourceTrustPromotionSource {
            artifact_kind: input.source_artifact_kind,
            artifact_id: input.source_artifact_id,
            issue_id: Some(input.source_issue_id),
        }),
        promoted_by_actor_type: Some(input.promoted_by_actor_type),
        promoted_by_actor_id: Some(input.promoted_by_actor_id),
        promoted_at: Some(promoted_at_to_rfc3339(input.promoted_at.as_ref())),
    }
}

/// `buildLowTrustSourceTrust` 输入（与 Node 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct BuildLowTrustSourceTrustInput {
    pub issue_id: String,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
}

/// `buildPromotedSourceTrust` 输入（与 Node 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct BuildPromotedSourceTrustInput {
    pub source_issue_id: String,
    pub source_artifact_kind: SourceTrustArtifactKind,
    pub source_artifact_id: String,
    pub promoted_by_actor_type: PromotedByActorType,
    pub promoted_by_actor_id: String,
    pub promoted_at: Option<PromotedAt>,
}

/// `promoted_at` 接受 `Date | string | undefined`（与 Node `Date` 1:1 对齐）。
#[derive(Debug, Clone)]
pub enum PromotedAt {
    DateTime(chrono::DateTime<chrono::Utc>),
    String(String),
}

impl From<chrono::DateTime<chrono::Utc>> for PromotedAt {
    fn from(dt: chrono::DateTime<chrono::Utc>) -> Self {
        Self::DateTime(dt)
    }
}

impl From<chrono::DateTime<chrono::FixedOffset>> for PromotedAt {
    fn from(dt: chrono::DateTime<chrono::FixedOffset>) -> Self {
        Self::DateTime(dt.with_timezone(&chrono::Utc))
    }
}

impl From<String> for PromotedAt {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for PromotedAt {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

fn promoted_at_to_rfc3339(promoted_at: Option<&PromotedAt>) -> String {
    let now = || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    match promoted_at {
        Some(PromotedAt::DateTime(dt)) => dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        Some(PromotedAt::String(s)) => s.clone(),
        None => now(),
    }
}

// ---- traits for redaction / sanitization (mirror Node's generic T) ----

/// 暴露 `sourceTrust` 引用 + 可重新构造 `body` 字段（与 Node `T extends { body?, sourceTrust? }` 1:1 对齐）。
pub trait SourceTrustRedactable: Sized {
    fn source_trust_ref(&self) -> Option<&SourceTrustMetadata>;
    fn with_redacted_body(self, new_body: &'static str) -> Self;
}

/// 暴露 `sourceTrust` 引用 + 可重新构造 body / presentation / metadata 三个字段
/// （与 Node `T extends { body; presentation?; metadata?; sourceTrust? }` 1:1 对齐）。
pub trait SourceTrustCommentSanitizable: Sized {
    fn source_trust_ref(&self) -> Option<&SourceTrustMetadata>;
    fn with_sanitized(self, new_body: &'static str) -> Self;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quarantined() -> SourceTrustMetadata {
        SourceTrustMetadata {
            preset: LOW_TRUST_REVIEW_PRESET.to_string(),
            disposition: SourceTrustDisposition::Quarantined,
            source_issue_id: Some("issue-1".into()),
            source_run_id: Some("run-1".into()),
            source_agent_id: Some("agent-1".into()),
            promoted_from: None,
            promoted_by_actor_type: None,
            promoted_by_actor_id: None,
            promoted_at: None,
        }
    }

    fn promoted() -> SourceTrustMetadata {
        build_promoted_source_trust(BuildPromotedSourceTrustInput {
            source_issue_id: "issue-1".into(),
            source_artifact_kind: SourceTrustArtifactKind::Comment,
            source_artifact_id: "comment-1".into(),
            promoted_by_actor_type: PromotedByActorType::User,
            promoted_by_actor_id: "user-1".into(),
            promoted_at: None,
        })
    }

    #[test]
    fn is_low_trust_quarantined_handles_none() {
        assert!(!is_low_trust_quarantined(None));
    }

    #[test]
    fn is_low_trust_quarantined_requires_both_preset_and_disposition() {
        let mut st = quarantined();
        st.disposition = SourceTrustDisposition::Promoted;
        assert!(!is_low_trust_quarantined(Some(&st)));
        st.disposition = SourceTrustDisposition::Quarantined;
        st.preset = "standard".to_string();
        assert!(!is_low_trust_quarantined(Some(&st)));
        st.preset = LOW_TRUST_REVIEW_PRESET.to_string();
        assert!(is_low_trust_quarantined(Some(&st)));
    }

    #[test]
    fn build_low_trust_source_trust_sets_preset_and_disposition() {
        let st = build_low_trust_source_trust(BuildLowTrustSourceTrustInput {
            issue_id: "issue-1".into(),
            run_id: Some("run-1".into()),
            agent_id: Some("agent-1".into()),
        });
        assert_eq!(st.preset, LOW_TRUST_REVIEW_PRESET);
        assert_eq!(st.disposition, SourceTrustDisposition::Quarantined);
        assert_eq!(st.source_issue_id.as_deref(), Some("issue-1"));
        assert_eq!(st.source_run_id.as_deref(), Some("run-1"));
        assert_eq!(st.source_agent_id.as_deref(), Some("agent-1"));
        assert!(st.promoted_from.is_none());
        assert!(st.promoted_at.is_none());
    }

    #[test]
    fn build_low_trust_source_trust_with_null_run_agent() {
        let st = build_low_trust_source_trust(BuildLowTrustSourceTrustInput {
            issue_id: "issue-1".into(),
            run_id: None,
            agent_id: None,
        });
        assert_eq!(st.source_run_id, None);
        assert_eq!(st.source_agent_id, None);
    }

    #[test]
    fn build_promoted_source_trust_populates_all_fields() {
        let st = build_promoted_source_trust(BuildPromotedSourceTrustInput {
            source_issue_id: "issue-1".into(),
            source_artifact_kind: SourceTrustArtifactKind::Document,
            source_artifact_id: "doc-1".into(),
            promoted_by_actor_type: PromotedByActorType::Agent,
            promoted_by_actor_id: "agent-1".into(),
            promoted_at: None,
        });
        assert_eq!(st.preset, LOW_TRUST_REVIEW_PRESET);
        assert_eq!(st.disposition, SourceTrustDisposition::Promoted);
        assert_eq!(st.source_issue_id.as_deref(), Some("issue-1"));
        let pf = st.promoted_from.expect("promoted_from");
        assert_eq!(pf.artifact_kind, SourceTrustArtifactKind::Document);
        assert_eq!(pf.artifact_id, "doc-1");
        assert_eq!(pf.issue_id.as_deref(), Some("issue-1"));
        assert_eq!(st.promoted_by_actor_type, Some(PromotedByActorType::Agent));
        assert_eq!(st.promoted_by_actor_id.as_deref(), Some("agent-1"));
        let promoted_at = st.promoted_at.expect("promoted_at");
        // Should be a valid RFC3339 timestamp
        assert!(chrono::DateTime::parse_from_rfc3339(&promoted_at).is_ok());
    }

    #[test]
    fn build_promoted_source_trust_with_explicit_date() {
        let explicit = chrono::DateTime::parse_from_rfc3339("2026-07-23T18:13:03.000Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let st = build_promoted_source_trust(BuildPromotedSourceTrustInput {
            source_issue_id: "issue-1".into(),
            source_artifact_kind: SourceTrustArtifactKind::Comment,
            source_artifact_id: "comment-1".into(),
            promoted_by_actor_type: PromotedByActorType::User,
            promoted_by_actor_id: "user-1".into(),
            promoted_at: Some(PromotedAt::DateTime(explicit)),
        });
        assert_eq!(st.promoted_at.as_deref(), Some("2026-07-23T18:13:03.000Z"));
    }

    #[test]
    fn build_promoted_source_trust_with_string_timestamp() {
        let st = build_promoted_source_trust(BuildPromotedSourceTrustInput {
            source_issue_id: "issue-1".into(),
            source_artifact_kind: SourceTrustArtifactKind::Issue,
            source_artifact_id: "issue-1".into(),
            promoted_by_actor_type: PromotedByActorType::System,
            promoted_by_actor_id: "system".into(),
            promoted_at: Some(PromotedAt::String("2026-01-01T00:00:00Z".into())),
        });
        assert_eq!(st.promoted_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn source_trust_metadata_serializes_with_camel_case() {
        let st = quarantined();
        let json = serde_json::to_value(&st).unwrap();
        assert!(json.get("sourceIssueId").is_some());
        assert!(json.get("sourceRunId").is_some());
        assert!(json.get("sourceAgentId").is_some());
        assert_eq!(json["preset"], "low_trust_review");
        assert_eq!(json["disposition"], "quarantined");
    }

    #[test]
    fn disposition_as_str() {
        assert_eq!(SourceTrustDisposition::Quarantined.as_str(), "quarantined");
        assert_eq!(SourceTrustDisposition::Promoted.as_str(), "promoted");
    }

    #[test]
    fn artifact_kind_as_str() {
        assert_eq!(SourceTrustArtifactKind::Comment.as_str(), "comment");
        assert_eq!(SourceTrustArtifactKind::Document.as_str(), "document");
        assert_eq!(
            SourceTrustArtifactKind::WorkProduct.as_str(),
            "work_product"
        );
        assert_eq!(SourceTrustArtifactKind::Issue.as_str(), "issue");
    }

    #[test]
    fn promoted_actor_type_as_str() {
        assert_eq!(PromotedByActorType::Agent.as_str(), "agent");
        assert_eq!(PromotedByActorType::User.as_str(), "user");
        assert_eq!(PromotedByActorType::System.as_str(), "system");
    }
}
