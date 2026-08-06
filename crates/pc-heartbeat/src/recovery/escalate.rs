//! Recovery 升级决策的纯计划层。
//!
//! 对齐 Node `services/recovery/service.ts` 的：
//! - `escalateStrandedAssignedIssue` 决策分支
//! - `escalateStrandedRecoveryIssueInPlace` 决策分支
//!
//! 边界：
//! - 不进行数据库写入
//! - 不进行副作用（不发 wake、不写 comment、不记 activity log）
//! - 输入：`issue` 快照 + `recovery_action` 候选 → 输出 `EscalationDecision`
//!
//! 调用方拿到 `EscalationDecision` 后自行决定：
//! - 是否要更新 issue.status / assignee_agent_id
//! - 是否要发 escalation comment（带 dedup）
//! - 是否要写 activity log

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::origins::{is_stranded_issue_recovery_origin_kind, recovery_origin_kinds};
use super::scheduler::SchedulerCandidate;
use super::source_scoped_recovery_action::StrandedRecoveryCause;

/// 升级前 issue 状态快照（仅决策需要的字段）。
#[derive(Debug, Clone)]
pub struct IssueSnapshot {
    pub id: Uuid,
    pub company_id: Uuid,
    pub origin_kind: Option<String>,
    pub origin_id: Option<Uuid>,
    pub status: String,
    pub assignee_agent_id: Option<Uuid>,
}

/// 升级最终决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationDecision {
    /// 升级 source issue → blocked，复用已 plan 好的 recovery action。
    SourceEscalate(SourceEscalationPlan),
    /// 升级 nested recovery issue → blocked（in-place，不创建新 issue）。
    RecoveryInPlace(RecoveryInPlacePlan),
    /// 跳过：issue 已经 blocked / done / cancelled / hidden。
    Skip(SkipReason),
}

/// Source issue 升级计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEscalationPlan {
    pub issue_id: Uuid,
    pub company_id: Uuid,
    pub previous_status: String,
    pub cause: StrandedRecoveryCause,
    pub recovery_action_id: Uuid,
    pub owner_agent_id: Option<Uuid>,
    pub return_owner_agent_id: Option<Uuid>,
    pub next_assignee_agent_id: Option<Uuid>,
    pub should_post_comment: bool,
    pub comment_body: String,
    pub comment_marker: String,
    pub activity_source: String,
    pub activity_action: String,
    pub is_provider_quota_wait: bool,
}

/// Nested recovery issue 升级计划（in-place）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryInPlacePlan {
    pub issue_id: Uuid,
    pub company_id: Uuid,
    pub previous_status: String,
    pub comment_body: String,
    pub activity_source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    AlreadyBlocked,
    AlreadyTerminal,
    NoRecoveryAction,
    RecoveryActionUnchanged,
}

/// 纯函数入口：根据 issue + scheduler 候选决定升级路径。
///
/// 与 Node `escalateStrandedAssignedIssue` 决策分支对齐：
/// 1. 若 issue 本身已 terminal（done/cancelled）或 hidden → skip
/// 2. 若 issue.origin_kind == "stranded_issue_recovery" → RecoveryInPlace
/// 3. 若 issue.status == "blocked" 且现有 recovery_action 不需要更新 → skip
/// 4. 否则 SourceEscalate（用 scheduler 候选中的 recovery action）
pub fn decide_escalation(
    issue: &IssueSnapshot,
    candidate: Option<&SchedulerCandidate>,
    recovery_action_id: Option<Uuid>,
    previous_status: &str,
) -> EscalationDecision {
    if is_terminal_or_hidden(&issue.status) {
        return EscalationDecision::Skip(SkipReason::AlreadyTerminal);
    }
    if is_stranded_issue_recovery_origin_kind(issue.origin_kind.as_deref()) {
        let plan = RecoveryInPlacePlan {
            issue_id: issue.id,
            company_id: issue.company_id,
            previous_status: previous_status.to_owned(),
            comment_body: build_recovery_in_place_comment_body(issue, previous_status),
            activity_source: "recovery.reconcile_stranded_recovery_issue".to_owned(),
        };
        return EscalationDecision::RecoveryInPlace(plan);
    }
    if issue.status == "blocked" {
        return EscalationDecision::Skip(SkipReason::AlreadyBlocked);
    }
    let Some(candidate) = candidate else {
        return EscalationDecision::Skip(SkipReason::NoRecoveryAction);
    };
    let Some(action_id) = recovery_action_id else {
        return EscalationDecision::Skip(SkipReason::NoRecoveryAction);
    };
    let next_assignee = match candidate.routing.owner_agent_id {
        Some(owner) => Some(owner),
        None => issue.assignee_agent_id,
    };
    let is_provider_quota_wait = candidate.cause == StrandedRecoveryCause::ProviderQuota
        && candidate.routing.owner_agent_id.is_none()
        && candidate.routing.return_owner_agent_id.is_some();
    let should_post_comment = true;
    let comment_marker = format!("Recovery action: `{action_id}`");
    let activity_source = activity_source_for_cause(candidate.cause);
    let activity_action = if candidate.cause == StrandedRecoveryCause::SuccessfulRunMissingState {
        "issue.successful_run_handoff_escalated".to_owned()
    } else {
        "issue.updated".to_owned()
    };
    let comment_body =
        build_source_escalation_comment_body(issue, candidate, action_id, next_assignee);
    EscalationDecision::SourceEscalate(SourceEscalationPlan {
        issue_id: issue.id,
        company_id: issue.company_id,
        previous_status: previous_status.to_owned(),
        cause: candidate.cause,
        recovery_action_id: action_id,
        owner_agent_id: candidate.routing.owner_agent_id,
        return_owner_agent_id: candidate.routing.return_owner_agent_id,
        next_assignee_agent_id: next_assignee,
        should_post_comment,
        comment_body,
        comment_marker,
        activity_source,
        activity_action,
        is_provider_quota_wait,
    })
}

pub fn is_terminal_or_hidden(status: &str) -> bool {
    matches!(status, "done" | "cancelled" | "hidden")
}

/// 是否值得为这个 issue 调用 scheduler 写 recovery action。
///
/// 仅在以下条件**全部满足**时返回 true：
/// - status 非 terminal / hidden
/// - origin_kind 不是 `stranded_issue_recovery`（这种 issue 走 in-place 路径）
/// - status 非 `blocked`（已 blocked 不需要再调 scheduler）
pub fn should_attempt_source_escalation(snapshot: &IssueSnapshot) -> bool {
    if is_terminal_or_hidden(&snapshot.status) {
        return false;
    }
    if is_stranded_issue_recovery_origin_kind(snapshot.origin_kind.as_deref()) {
        return false;
    }
    if snapshot.status == "blocked" {
        return false;
    }
    true
}

fn activity_source_for_cause(cause: StrandedRecoveryCause) -> String {
    match cause {
        StrandedRecoveryCause::SuccessfulRunMissingState => {
            "recovery.reconcile_successful_run_handoff_missing_state".to_owned()
        }
        StrandedRecoveryCause::WorkspaceValidationFailed => {
            "recovery.reconcile_workspace_validation_failed".to_owned()
        }
        StrandedRecoveryCause::ConfigurationIncomplete => {
            "recovery.reconcile_configuration_incomplete".to_owned()
        }
        StrandedRecoveryCause::ExecutionReviewParticipantRecovery => {
            "recovery.reconcile_execution_review_participant".to_owned()
        }
        _ => "recovery.reconcile_stranded_assigned_issue".to_owned(),
    }
}

fn build_source_escalation_comment_body(
    issue: &IssueSnapshot,
    candidate: &SchedulerCandidate,
    action_id: Uuid,
    next_assignee: Option<Uuid>,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "Paperclip exhausted automatic recovery for the assigned issue and escalated to `blocked` (owner: {}).",
        match next_assignee {
            Some(id) => format!("agent `{id}`"),
            None => "board".to_owned(),
        }
    ));
    lines.push(String::new());
    lines.push(format!("- Cause: `{}`", candidate.cause.as_str()));
    if let Some(return_owner) = candidate.routing.return_owner_agent_id {
        lines.push(format!("- Original assignee: `{return_owner}`"));
    }
    lines.push(format!("- Recovery action: `{action_id}`"));
    lines.push(String::new());
    lines.push(
        "- Next action: the recovery owner should either restore a live execution path or record the manual resolution on the source issue."
            .to_owned(),
    );
    lines.join("\n")
}

fn build_recovery_in_place_comment_body(issue: &IssueSnapshot, previous_status: &str) -> String {
    format!(
        "Paperclip retried the recovery issue `{origin_kind}` but the recovery attempt still has no live execution path. \
         Moving it back to `blocked` (previous status: `{previous_status}`). \
         A board operator should inspect the recovery issue and resolve or cancel it manually.",
        origin_kind = issue
            .origin_kind
            .as_deref()
            .unwrap_or(recovery_origin_kinds::STRANDED_ISSUE_RECOVERY)
    )
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::scheduler::SchedulerRoutingHints;
    use crate::recovery::source_scoped_recovery_action::{
        build_source_scoped_recovery_action_plan, StrandedRecoveryCause,
    };
    use serde_json::json;

    fn uuid(seed: u8) -> Uuid {
        Uuid::from_bytes([seed; 16])
    }

    fn issue(origin_kind: Option<&str>, status: &str, assignee: Option<Uuid>) -> IssueSnapshot {
        IssueSnapshot {
            id: uuid(1),
            company_id: uuid(2),
            origin_kind: origin_kind.map(str::to_owned),
            origin_id: None,
            status: status.to_owned(),
            assignee_agent_id: assignee,
        }
    }

    fn candidate(
        cause: StrandedRecoveryCause,
        owner: Option<Uuid>,
        return_owner: Option<Uuid>,
    ) -> SchedulerCandidate {
        SchedulerCandidate {
            cause,
            plan: build_source_scoped_recovery_action_plan(
                uuid(10),
                uuid(1),
                cause,
                owner,
                return_owner,
                None,
            ),
            routing: SchedulerRoutingHints {
                owner_agent_id: owner,
                return_owner_agent_id: return_owner,
                previous_owner_agent_id: None,
                routing_fallback_reason: None,
            },
            evidence: json!({}),
            retry_at: None,
        }
    }

    #[test]
    fn terminal_issue_is_skipped() {
        let result = decide_escalation(&issue(None, "done", None), None, None, "in_progress");
        assert_eq!(
            result,
            EscalationDecision::Skip(SkipReason::AlreadyTerminal)
        );
    }

    #[test]
    fn hidden_issue_is_skipped() {
        let result = decide_escalation(&issue(None, "hidden", None), None, None, "in_progress");
        assert_eq!(
            result,
            EscalationDecision::Skip(SkipReason::AlreadyTerminal)
        );
    }

    #[test]
    fn recovery_issue_takes_in_place_path() {
        let result = decide_escalation(
            &issue(Some("stranded_issue_recovery"), "todo", Some(uuid(3))),
            None,
            None,
            "todo",
        );
        match result {
            EscalationDecision::RecoveryInPlace(plan) => {
                assert_eq!(plan.issue_id, uuid(1));
                assert_eq!(plan.previous_status, "todo");
                assert_eq!(
                    plan.activity_source,
                    "recovery.reconcile_stranded_recovery_issue"
                );
            }
            other => panic!("expected RecoveryInPlace, got {other:?}"),
        }
    }

    #[test]
    fn source_escalate_uses_recovery_owner() {
        let cand = candidate(
            StrandedRecoveryCause::ProcessLost,
            Some(uuid(7)),
            Some(uuid(3)),
        );
        let result = decide_escalation(
            &issue(None, "in_progress", Some(uuid(3))),
            Some(&cand),
            Some(uuid(99)),
            "in_progress",
        );
        match result {
            EscalationDecision::SourceEscalate(plan) => {
                assert_eq!(plan.recovery_action_id, uuid(99));
                assert_eq!(plan.next_assignee_agent_id, Some(uuid(7)));
                assert!(plan.should_post_comment);
                assert!(plan.comment_body.contains("Recovery action: `"));
                assert!(plan.comment_body.contains("`"));
                assert!(plan.comment_marker.contains("Recovery action: `"));
                assert!(!plan.is_provider_quota_wait);
                assert_eq!(plan.activity_action, "issue.updated");
            }
            other => panic!("expected SourceEscalate, got {other:?}"),
        }
    }

    #[test]
    fn blocked_issue_is_skipped_with_already_blocked_reason() {
        let cand = candidate(
            StrandedRecoveryCause::RuntimeFailure,
            Some(uuid(7)),
            Some(uuid(3)),
        );
        let result = decide_escalation(
            &issue(None, "blocked", Some(uuid(3))),
            Some(&cand),
            Some(uuid(99)),
            "blocked",
        );
        assert_eq!(result, EscalationDecision::Skip(SkipReason::AlreadyBlocked));
    }

    #[test]
    fn provider_quota_without_owner_marks_provider_quota_wait() {
        let cand = candidate(StrandedRecoveryCause::ProviderQuota, None, Some(uuid(3)));
        let result = decide_escalation(
            &issue(None, "in_progress", Some(uuid(3))),
            Some(&cand),
            Some(uuid(99)),
            "in_progress",
        );
        match result {
            EscalationDecision::SourceEscalate(plan) => {
                assert!(plan.is_provider_quota_wait);
                assert!(plan.owner_agent_id.is_none());
                assert_eq!(plan.next_assignee_agent_id, Some(uuid(3)));
                assert_eq!(
                    plan.activity_source,
                    "recovery.reconcile_stranded_assigned_issue"
                );
            }
            other => panic!("expected SourceEscalate, got {other:?}"),
        }
    }

    #[test]
    fn no_candidate_yields_no_recovery_action_skip() {
        let result = decide_escalation(
            &issue(None, "in_progress", Some(uuid(3))),
            None,
            None,
            "in_progress",
        );
        assert_eq!(
            result,
            EscalationDecision::Skip(SkipReason::NoRecoveryAction)
        );
    }

    #[test]
    fn activity_source_for_successful_run_missing_state_is_distinct() {
        let cand = candidate(
            StrandedRecoveryCause::SuccessfulRunMissingState,
            Some(uuid(7)),
            Some(uuid(3)),
        );
        let result = decide_escalation(
            &issue(None, "in_progress", Some(uuid(3))),
            Some(&cand),
            Some(uuid(99)),
            "in_progress",
        );
        match result {
            EscalationDecision::SourceEscalate(plan) => {
                assert_eq!(
                    plan.activity_source,
                    "recovery.reconcile_successful_run_handoff_missing_state"
                );
                assert_eq!(
                    plan.activity_action,
                    "issue.successful_run_handoff_escalated"
                );
            }
            other => panic!("expected SourceEscalate, got {other:?}"),
        }
    }
}
