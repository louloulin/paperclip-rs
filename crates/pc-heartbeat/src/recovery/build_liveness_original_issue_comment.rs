//! `buildLivenessOriginalIssueComment` —— Node `services/recovery/service.ts:748`。
//!
//! 业务语义：
//! - 当某个原始 issue 的 dependency graph 出现 liveness incident 时，会在原始 issue 上
//!   写一条 system comment，解释发生了什么 + 引导 manager 行动。
//! - 与 `build_liveness_escalation_description` 的区别：本函数生成的是**comment**（不是
//!   escalation issue 的 description），措辞更短，目标是让 issue 上下文快速了解事件。
//!
//! 设计意图：
//! - pure 函数：输入 `&IssueLivenessFinding` + issue 引用信息 + 输出 String
//! - 复用 `format_dependency_path`（与 build_liveness_escalation_description 一致）
//! - 与 Node 完全对齐：每行 bullet / section 完全一致

use crate::recovery::build_liveness_escalation_description::format_dependency_path;
use crate::recovery::issue_graph_liveness::IssueLivenessFinding;

/// Node 中 escalation issue 在写这条 comment 时只用到 identifier / id，所以这里也
/// 只接受这两个字段（避免循环依赖 issue row）。
#[derive(Debug, Clone)]
pub struct OriginalIssueCommentContext {
    pub identifier: Option<String>,
    pub id: uuid::Uuid,
}

/// Node `buildLivenessOriginalIssueComment` 的 Rust 等价。
///
/// 输入：`&IssueLivenessFinding` + `&OriginalIssueCommentContext`
/// 输出：markdown 格式的 comment body
pub fn build_liveness_original_issue_comment(
    finding: &IssueLivenessFinding,
    escalation: &OriginalIssueCommentContext,
) -> String {
    [
        "Paperclip detected a harness-level liveness incident in this issue's dependency graph.",
        "",
        &format!(
            "- Escalation issue: {}",
            escalation.identifier.clone().unwrap_or_else(|| escalation.id.to_string())
        ),
        &format!("- Incident key: `{}`", finding.incident_key),
        &format!("- Finding: `{}`", finding.state.as_str()),
        &format!("- Dependency path: {}", format_dependency_path(finding)),
        &format!("- Reason: {}", finding.reason),
        &format!("- Manager action requested: {}", finding.recommended_action),
        "",
        "This issue now keeps its existing blockers and is also blocked by the escalation issue so dependency wakeups remain explicit.",
    ]
    .join("
")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::issue_graph_liveness::{
        IssueLivenessDependencyPathEntry, IssueLivenessFinding, IssueLivenessOwnerCandidate,
        IssueLivenessOwnerCandidateReason, IssueLivenessSeverity, IssueLivenessState,
    };
    use uuid::Uuid;

    fn uuid(seed: u8) -> Uuid {
        Uuid::from_bytes([seed; 16])
    }

    fn finding() -> IssueLivenessFinding {
        IssueLivenessFinding {
            company_id: uuid(1),
            incident_key: "inc-2".to_owned(),
            state: IssueLivenessState::BlockedByCancelledIssue,
            severity: IssueLivenessSeverity::Critical,
            source_issue_id: uuid(2),
            source_issue_label: "ROOT".to_owned(),
            reason: "blocked by cancelled".to_owned(),
            dependency_path: vec![
                IssueLivenessDependencyPathEntry {
                    issue_id: uuid(2),
                    identifier: Some("ROOT-1".to_owned()),
                    title: "Root".to_owned(),
                    status: "todo".to_owned(),
                },
                IssueLivenessDependencyPathEntry {
                    issue_id: uuid(3),
                    identifier: Some("BLK-2".to_owned()),
                    title: "Blk".to_owned(),
                    status: "cancelled".to_owned(),
                },
            ],
            recovery_issue_id: Some(uuid(3)),
            blocker_issue_id: None,
            participant_agent_id: None,
            recommended_owner_agent_id: Some(uuid(4)),
            recommended_owner_candidate_agent_ids: vec![uuid(4)],
            recommended_owner_candidates: vec![IssueLivenessOwnerCandidate {
                agent_id: uuid(4),
                reason: IssueLivenessOwnerCandidateReason::RootAgent,
                source_issue_id: uuid(2),
            }],
            recommended_action: "Resolve or unblock".to_owned(),
        }
    }

    fn escalation() -> OriginalIssueCommentContext {
        OriginalIssueCommentContext {
            identifier: Some("ESC-9".to_owned()),
            id: uuid(5),
        }
    }

    #[test]
    fn comment_starts_with_harness_level_intro() {
        let body = build_liveness_original_issue_comment(&finding(), &escalation());
        assert!(body.starts_with(
            "Paperclip detected a harness-level liveness incident in this issue's dependency graph."
        ));
    }

    #[test]
    fn comment_includes_all_fields() {
        let body = build_liveness_original_issue_comment(&finding(), &escalation());
        assert!(body.contains("- Escalation issue: ESC-9"));
        assert!(body.contains("- Incident key: `inc-2`"));
        assert!(body.contains("- Finding: `blocked_by_cancelled_issue`"));
        assert!(body.contains("- Dependency path: ROOT-1 -> BLK-2"));
        assert!(body.contains("- Reason: blocked by cancelled"));
        assert!(body.contains("- Manager action requested: Resolve or unblock"));
    }

    #[test]
    fn comment_close_guidance_in_block_message() {
        let body = build_liveness_original_issue_comment(&finding(), &escalation());
        assert!(body.contains("This issue now keeps its existing blockers"));
        assert!(
            body.contains("blocked by the escalation issue so dependency wakeups remain explicit")
        );
    }

    #[test]
    fn escalation_identifier_none_falls_back_to_uuid() {
        let ctx = OriginalIssueCommentContext {
            identifier: None,
            id: uuid(5),
        };
        let body = build_liveness_original_issue_comment(&finding(), &ctx);
        assert!(body.contains("- Escalation issue: 05050505-0505-0505-0505-050505050505"));
    }
}
