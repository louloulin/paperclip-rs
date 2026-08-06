//! Recovery origin kinds / reason kinds / key 前缀与构建解析
//!
//! 对齐 Node `services/recovery/origins.ts`：
//! - 常量 `RECOVERY_ORIGIN_KINDS` / `RECOVERY_REASON_KINDS` / `RECOVERY_KEY_PREFIXES`
//! - 类型 `RecoveryOriginKind` / `RecoveryReasonKind` / `RecoveryKeyPrefix`
//! - 函数 `is_stranded_issue_recovery_origin_kind(origin)`
//! - 函数 `build_issue_graph_liveness_incident_key(input)` / `parse_issue_graph_liveness_incident_key(key)`
//! - 函数 `build_issue_graph_liveness_leaf_key(input)`
//!
//! 设计：
//! - 纯函数无副作用，方便单测
//! - 强类型（`RecoveryOriginKind` 是 enum 而非字符串），编译期防止拼写错误
//! - 字符串字面量与 Node 完全一致，跨语言日志可读
//! - key 格式 `prefix:companyId:issueId:state:leafIssueId`，用 `:` 分隔

use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// Recovery 起源类型（事件来源分类）。
///
/// 对齐 Node `RECOVERY_ORIGIN_KINDS`。
pub mod recovery_origin_kinds {
    pub const ISSUE_GRAPH_LIVENESS_ESCALATION: &str = "harness_liveness_escalation";
    pub const ISSUE_PRODUCTIVITY_REVIEW: &str = "issue_productivity_review";
    pub const STRANDED_ISSUE_RECOVERY: &str = "stranded_issue_recovery";
    pub const STALE_ACTIVE_RUN_EVALUATION: &str = "stale_active_run_evaluation";
}

/// Recovery 原因类型（触发原因）。
///
/// 对齐 Node `RECOVERY_REASON_KINDS`。
pub mod recovery_reason_kinds {
    pub const RUN_LIVENESS_CONTINUATION: &str = "run_liveness_continuation";
}

/// Recovery key 前缀（用于 idempotency / 去重）。
///
/// 对齐 Node `RECOVERY_KEY_PREFIXES`。
pub mod recovery_key_prefixes {
    pub const ISSUE_GRAPH_LIVENESS_INCIDENT: &str = "harness_liveness";
    pub const ISSUE_GRAPH_LIVENESS_LEAF: &str = "harness_liveness_leaf";
}

// ============================================================================
// Types
// ============================================================================

/// Recovery 起源类型枚举（强类型版本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryOriginKind {
    IssueGraphLivenessEscalation,
    IssueProductivityReview,
    StrandedIssueRecovery,
    StaleActiveRunEvaluation,
}

impl RecoveryOriginKind {
    /// 返回 Node 端字符串字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssueGraphLivenessEscalation => {
                recovery_origin_kinds::ISSUE_GRAPH_LIVENESS_ESCALATION
            }
            Self::IssueProductivityReview => recovery_origin_kinds::ISSUE_PRODUCTIVITY_REVIEW,
            Self::StrandedIssueRecovery => recovery_origin_kinds::STRANDED_ISSUE_RECOVERY,
            Self::StaleActiveRunEvaluation => recovery_origin_kinds::STALE_ACTIVE_RUN_EVALUATION,
        }
    }

    /// 从字符串字面量解析（None 表示非法）。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            recovery_origin_kinds::ISSUE_GRAPH_LIVENESS_ESCALATION => {
                Some(Self::IssueGraphLivenessEscalation)
            }
            recovery_origin_kinds::ISSUE_PRODUCTIVITY_REVIEW => Some(Self::IssueProductivityReview),
            recovery_origin_kinds::STRANDED_ISSUE_RECOVERY => Some(Self::StrandedIssueRecovery),
            recovery_origin_kinds::STALE_ACTIVE_RUN_EVALUATION => {
                Some(Self::StaleActiveRunEvaluation)
            }
            _ => None,
        }
    }
}

/// Recovery 原因类型枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryReasonKind {
    RunLivenessContinuation,
}

impl RecoveryReasonKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunLivenessContinuation => recovery_reason_kinds::RUN_LIVENESS_CONTINUATION,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            recovery_reason_kinds::RUN_LIVENESS_CONTINUATION => Some(Self::RunLivenessContinuation),
            _ => None,
        }
    }
}

/// Recovery key 前缀枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryKeyPrefix {
    IssueGraphLivenessIncident,
    IssueGraphLivenessLeaf,
}

impl RecoveryKeyPrefix {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssueGraphLivenessIncident => {
                recovery_key_prefixes::ISSUE_GRAPH_LIVENESS_INCIDENT
            }
            Self::IssueGraphLivenessLeaf => recovery_key_prefixes::ISSUE_GRAPH_LIVENESS_LEAF,
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// 判断 origin 是否为「stranded issue recovery」（孤立 issue 恢复）。
///
/// 对齐 Node `isStrandedIssueRecoveryOriginKind`。
pub fn is_stranded_issue_recovery_origin_kind(origin: Option<&str>) -> bool {
    matches!(origin, Some(recovery_origin_kinds::STRANDED_ISSUE_RECOVERY))
}

/// 构建 issue graph liveness incident key。
///
/// 格式：`harness_liveness:{companyId}:{issueId}:{state}:{blockerIssueId|participantAgentId|"none"}`
///
/// 对齐 Node `buildIssueGraphLivenessIncidentKey`。
pub fn build_issue_graph_liveness_incident_key(input: IncidentKeyInput<'_>) -> String {
    let leaf = input
        .blocker_issue_id
        .or(input.participant_agent_id)
        .unwrap_or("none");
    [
        recovery_key_prefixes::ISSUE_GRAPH_LIVENESS_INCIDENT,
        input.company_id,
        input.issue_id,
        input.state,
        leaf,
    ]
    .join(":")
}

/// 解析 issue graph liveness incident key。
///
/// 返回 `None` 当 key 为空、格式不对（不是 5 段）、或首段不是正确前缀。
/// 对齐 Node `parseIssueGraphLivenessIncidentKey`。
pub fn parse_issue_graph_liveness_incident_key(
    incident_key: Option<&str>,
) -> Option<ParsedIncidentKey<'_>> {
    let key = incident_key?;
    let parts: Vec<&str> = key.split(':').collect();
    if parts.len() != 5 {
        return None;
    }
    if parts[0] != recovery_key_prefixes::ISSUE_GRAPH_LIVENESS_INCIDENT {
        return None;
    }
    let company_id = parts[1];
    let issue_id = parts[2];
    let state = parts[3];
    let leaf_issue_id = parts[4];
    if company_id.is_empty() || issue_id.is_empty() || state.is_empty() || leaf_issue_id.is_empty()
    {
        return None;
    }
    Some(ParsedIncidentKey {
        company_id,
        issue_id,
        state,
        leaf_issue_id,
    })
}

/// 构建 issue graph liveness leaf key。
///
/// 格式：`harness_liveness_leaf:{companyId}:{state}:{leafIssueId}`
pub fn build_issue_graph_liveness_leaf_key(input: LeafKeyInput<'_>) -> String {
    [
        recovery_key_prefixes::ISSUE_GRAPH_LIVENESS_LEAF,
        input.company_id,
        input.state,
        input.leaf_issue_id,
    ]
    .join(":")
}

// ============================================================================
// Inputs / Outputs
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub struct IncidentKeyInput<'a> {
    pub company_id: &'a str,
    pub issue_id: &'a str,
    pub state: &'a str,
    pub blocker_issue_id: Option<&'a str>,
    pub participant_agent_id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedIncidentKey<'a> {
    pub company_id: &'a str,
    pub issue_id: &'a str,
    pub state: &'a str,
    pub leaf_issue_id: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct LeafKeyInput<'a> {
    pub company_id: &'a str,
    pub state: &'a str,
    pub leaf_issue_id: &'a str,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Origin kind enum round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn origin_kind_as_str_matches_constants() {
        assert_eq!(
            RecoveryOriginKind::IssueGraphLivenessEscalation.as_str(),
            recovery_origin_kinds::ISSUE_GRAPH_LIVENESS_ESCALATION
        );
        assert_eq!(
            RecoveryOriginKind::IssueProductivityReview.as_str(),
            recovery_origin_kinds::ISSUE_PRODUCTIVITY_REVIEW
        );
        assert_eq!(
            RecoveryOriginKind::StrandedIssueRecovery.as_str(),
            recovery_origin_kinds::STRANDED_ISSUE_RECOVERY
        );
        assert_eq!(
            RecoveryOriginKind::StaleActiveRunEvaluation.as_str(),
            recovery_origin_kinds::STALE_ACTIVE_RUN_EVALUATION
        );
    }

    #[test]
    fn origin_kind_from_str_round_trip() {
        for kind in [
            RecoveryOriginKind::IssueGraphLivenessEscalation,
            RecoveryOriginKind::IssueProductivityReview,
            RecoveryOriginKind::StrandedIssueRecovery,
            RecoveryOriginKind::StaleActiveRunEvaluation,
        ] {
            assert_eq!(RecoveryOriginKind::from_str(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn origin_kind_from_str_unknown_returns_none() {
        assert_eq!(RecoveryOriginKind::from_str("not_a_real_origin"), None);
        assert_eq!(RecoveryOriginKind::from_str(""), None);
    }

    // -----------------------------------------------------------------------
    // Reason kind round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn reason_kind_as_str_matches_constants() {
        assert_eq!(
            RecoveryReasonKind::RunLivenessContinuation.as_str(),
            recovery_reason_kinds::RUN_LIVENESS_CONTINUATION
        );
    }

    #[test]
    fn reason_kind_from_str_round_trip() {
        for kind in [RecoveryReasonKind::RunLivenessContinuation] {
            assert_eq!(RecoveryReasonKind::from_str(kind.as_str()), Some(kind));
        }
        assert_eq!(RecoveryReasonKind::from_str("unknown"), None);
    }

    // -----------------------------------------------------------------------
    // is_stranded_issue_recovery_origin_kind
    // -----------------------------------------------------------------------

    #[test]
    fn stranded_check_matches_only_stranded_constant() {
        assert!(is_stranded_issue_recovery_origin_kind(Some(
            recovery_origin_kinds::STRANDED_ISSUE_RECOVERY
        )));
        assert!(!is_stranded_issue_recovery_origin_kind(Some(
            recovery_origin_kinds::ISSUE_PRODUCTIVITY_REVIEW
        )));
        assert!(!is_stranded_issue_recovery_origin_kind(Some(
            recovery_origin_kinds::ISSUE_GRAPH_LIVENESS_ESCALATION
        )));
        assert!(!is_stranded_issue_recovery_origin_kind(None));
        assert!(!is_stranded_issue_recovery_origin_kind(Some("")));
        assert!(!is_stranded_issue_recovery_origin_kind(Some("unrelated")));
    }

    // -----------------------------------------------------------------------
    // build_issue_graph_liveness_incident_key
    // -----------------------------------------------------------------------

    #[test]
    fn build_incident_key_uses_blocker_when_present() {
        let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
            company_id: "co1",
            issue_id: "is1",
            state: "stuck",
            blocker_issue_id: Some("blk1"),
            participant_agent_id: Some("ag1"),
        });
        assert_eq!(key, "harness_liveness:co1:is1:stuck:blk1");
    }

    #[test]
    fn build_incident_key_falls_back_to_participant() {
        let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
            company_id: "co1",
            issue_id: "is1",
            state: "stuck",
            blocker_issue_id: None,
            participant_agent_id: Some("ag1"),
        });
        assert_eq!(key, "harness_liveness:co1:is1:stuck:ag1");
    }

    #[test]
    fn build_incident_key_falls_back_to_none() {
        let key = build_issue_graph_liveness_incident_key(IncidentKeyInput {
            company_id: "co1",
            issue_id: "is1",
            state: "stuck",
            blocker_issue_id: None,
            participant_agent_id: None,
        });
        assert_eq!(key, "harness_liveness:co1:is1:stuck:none");
    }

    // -----------------------------------------------------------------------
    // parse_issue_graph_liveness_incident_key
    // -----------------------------------------------------------------------

    #[test]
    fn parse_incident_key_round_trip() {
        let original = build_issue_graph_liveness_incident_key(IncidentKeyInput {
            company_id: "co1",
            issue_id: "is1",
            state: "stuck",
            blocker_issue_id: Some("blk1"),
            participant_agent_id: None,
        });
        let parsed = parse_issue_graph_liveness_incident_key(Some(&original)).unwrap();
        assert_eq!(parsed.company_id, "co1");
        assert_eq!(parsed.issue_id, "is1");
        assert_eq!(parsed.state, "stuck");
        assert_eq!(parsed.leaf_issue_id, "blk1");
    }

    #[test]
    fn parse_incident_key_rejects_none_input() {
        assert!(parse_issue_graph_liveness_incident_key(None).is_none());
        assert!(parse_issue_graph_liveness_incident_key(Some("")).is_none());
    }

    #[test]
    fn parse_incident_key_rejects_wrong_segment_count() {
        // 4 segments
        assert!(
            parse_issue_graph_liveness_incident_key(Some("harness_liveness:co1:is1:stuck"))
                .is_none()
        );
        // 6 segments
        assert!(parse_issue_graph_liveness_incident_key(Some(
            "harness_liveness:co1:is1:stuck:x:y"
        ))
        .is_none());
    }

    #[test]
    fn parse_incident_key_rejects_wrong_prefix() {
        assert!(
            parse_issue_graph_liveness_incident_key(Some("wrong_prefix:co1:is1:stuck:x")).is_none()
        );
        assert!(parse_issue_graph_liveness_incident_key(Some(
            "harness_liveness_leaf:co1:is1:stuck:x"
        ))
        .is_none());
    }

    #[test]
    fn parse_incident_key_rejects_empty_segments() {
        // Empty companyId
        assert!(
            parse_issue_graph_liveness_incident_key(Some("harness_liveness::is1:stuck:x"))
                .is_none()
        );
        // Empty state
        assert!(
            parse_issue_graph_liveness_incident_key(Some("harness_liveness:co1:is1::x")).is_none()
        );
    }

    // -----------------------------------------------------------------------
    // build_issue_graph_liveness_leaf_key
    // -----------------------------------------------------------------------

    #[test]
    fn build_leaf_key_format() {
        let key = build_issue_graph_liveness_leaf_key(LeafKeyInput {
            company_id: "co1",
            state: "stuck",
            leaf_issue_id: "leaf1",
        });
        assert_eq!(key, "harness_liveness_leaf:co1:stuck:leaf1");
    }

    // -----------------------------------------------------------------------
    // Serde round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn serde_origin_kind_round_trip() {
        let kind = RecoveryOriginKind::StrandedIssueRecovery;
        let json = serde_json::to_string(&kind).unwrap();
        let back: RecoveryOriginKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }
}
