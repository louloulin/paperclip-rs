//! Approval 状态机（pure logic）。
//!
//! 与 paperclip 上游 `services/approvals.ts` 中的 `resolveApproval` 状态转换一致：
//! - `pending → approved | rejected | cancelled | revision_requested`
//! - `approved | rejected | cancelled | expired` → 终态，不可再转换
//!
//! 本模块是纯逻辑层，无 DB 依赖，便于：
//! - 单元测试（无需 sqlx）
//! - 在 service 层调用前预先校验
//! - 在 route 层提供清晰的错误信息

use pc_repos::approval::ApprovalStatus;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("approval is already in terminal state {0:?}, cannot transition to {1:?}")]
    AlreadyTerminal(ApprovalStatus, ApprovalStatus),
    #[error("cannot decide back to pending (was {0:?})")]
    CannotReturnToPending(ApprovalStatus),
    #[error("only pending approvals can request revision (was {0:?})")]
    RevisionOnlyFromPending(ApprovalStatus),
    #[error("unsupported target status {0:?}")]
    UnsupportedTarget(ApprovalStatus),
}

/// 给定当前状态和目标状态，验证转换合法性。
///
/// 返回 `Ok(())` 表示合法；返回 `Err(TransitionError)` 表示非法。
///
/// ## 合法转换
/// - `pending → approved | rejected | cancelled`
///
/// ## 非法转换
/// - 任何终态 → 任意状态
/// - 任意状态 → pending
/// - `approved → revision_requested`（需要先创建新 approval）
pub fn validate_transition(
    from: ApprovalStatus,
    to: ApprovalStatus,
) -> Result<(), TransitionError> {
    use ApprovalStatus::*;

    // Pending 优先检查以提供更精确的错误信息
    if to == Pending {
        return Err(TransitionError::CannotReturnToPending(from));
    }
    if from.is_terminal() {
        return Err(TransitionError::AlreadyTerminal(from, to));
    }
    match (from, to) {
        (Pending, Approved | Rejected | Cancelled) => Ok(()),
        _ => Err(TransitionError::UnsupportedTarget(to)),
    }
}

/// 给定当前状态，请求 revision 是否合法（仅 pending 可用）。
pub fn can_request_revision(from: ApprovalStatus) -> bool {
    from == ApprovalStatus::Pending
}

/// 给定当前状态，决定（approve/reject）是否合法。
pub fn can_decide(from: ApprovalStatus) -> bool {
    from == ApprovalStatus::Pending
}

/// 给定当前状态，取消是否合法（idempotent：终态返回 false 表示 no-op）。
pub fn can_cancel(from: ApprovalStatus) -> bool {
    from == ApprovalStatus::Pending
}

/// 当前未实现 resubmit 状态（业务通过创建新 approval 实现）。
pub fn can_resubmit(_from: ApprovalStatus) -> bool {
    false
}

/// 给定目标状态，决定 ApprovalService 该调用的方法。
///
/// 用于 route 层：HTTP POST /decide with body {status: "approved|rejected|cancelled"}
/// → service.approve / reject / cancel。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAction {
    Approve,
    Reject,
    Cancel,
    RequestRevision,
}

impl DecisionAction {
    #[must_use]
    pub fn from_status(s: ApprovalStatus) -> Option<Self> {
        match s {
            ApprovalStatus::Approved => Some(Self::Approve),
            ApprovalStatus::Rejected => Some(Self::Reject),
            ApprovalStatus::Cancelled => Some(Self::Cancel),
            _ => None,
        }
    }
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Cancel => "cancel",
            Self::RequestRevision => "request_revision",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r576_pending_can_transition_to_approved_rejected_cancelled() {
        assert!(validate_transition(ApprovalStatus::Pending, ApprovalStatus::Approved).is_ok());
        assert!(validate_transition(ApprovalStatus::Pending, ApprovalStatus::Rejected).is_ok());
        assert!(validate_transition(ApprovalStatus::Pending, ApprovalStatus::Cancelled).is_ok());
    }

    #[test]
    fn r576_cannot_transition_from_terminal() {
        for terminal in [
            ApprovalStatus::Approved,
            ApprovalStatus::Rejected,
            ApprovalStatus::Cancelled,
            ApprovalStatus::Expired,
        ] {
            assert_eq!(
                validate_transition(terminal, ApprovalStatus::Approved),
                Err(TransitionError::AlreadyTerminal(
                    terminal,
                    ApprovalStatus::Approved
                ))
            );
            assert_eq!(
                validate_transition(terminal, ApprovalStatus::Pending),
                Err(TransitionError::CannotReturnToPending(terminal))
            );
        }
    }

    #[test]
    fn r576_cannot_return_to_pending() {
        for from in [
            ApprovalStatus::Pending,
            ApprovalStatus::Expired,
            ApprovalStatus::Approved,
            ApprovalStatus::Rejected,
        ] {
            assert_eq!(
                validate_transition(from, ApprovalStatus::Pending),
                Err(TransitionError::CannotReturnToPending(from))
            );
        }
    }

    #[test]
    fn r576_can_request_revision_only_from_pending() {
        assert!(can_request_revision(ApprovalStatus::Pending));
        assert!(!can_request_revision(ApprovalStatus::Expired));
        assert!(!can_request_revision(ApprovalStatus::Approved));
        assert!(!can_request_revision(ApprovalStatus::Rejected));
    }

    #[test]
    fn r576_can_decide_only_from_pending() {
        assert!(can_decide(ApprovalStatus::Pending));
        assert!(!can_decide(ApprovalStatus::Approved));
        assert!(!can_decide(ApprovalStatus::Rejected));
        assert!(!can_decide(ApprovalStatus::Cancelled));
        assert!(!can_decide(ApprovalStatus::Expired));
    }

    #[test]
    fn r576_can_cancel_only_from_pending() {
        assert!(can_cancel(ApprovalStatus::Pending));
        assert!(!can_cancel(ApprovalStatus::Approved));
        assert!(!can_cancel(ApprovalStatus::Rejected));
        assert!(!can_cancel(ApprovalStatus::Cancelled));
        assert!(!can_cancel(ApprovalStatus::Expired));
    }

    #[test]
    fn r576_can_resubmit_always_false() {
        // 当前实现下，resubmit 走创建新 approval 路径，所以总返回 false
        assert!(!can_resubmit(ApprovalStatus::Pending));
        assert!(!can_resubmit(ApprovalStatus::Approved));
        assert!(!can_resubmit(ApprovalStatus::Rejected));
        assert!(!can_resubmit(ApprovalStatus::Cancelled));
        assert!(!can_resubmit(ApprovalStatus::Expired));
    }

    #[test]
    fn r576_decision_action_from_status_mapping() {
        assert_eq!(
            DecisionAction::from_status(ApprovalStatus::Approved),
            Some(DecisionAction::Approve)
        );
        assert_eq!(
            DecisionAction::from_status(ApprovalStatus::Rejected),
            Some(DecisionAction::Reject)
        );
        assert_eq!(
            DecisionAction::from_status(ApprovalStatus::Cancelled),
            Some(DecisionAction::Cancel)
        );
        assert_eq!(DecisionAction::from_status(ApprovalStatus::Pending), None);
        assert_eq!(
            DecisionAction::from_status(ApprovalStatus::Expired),
            None
        );
    }

    #[test]
    fn r576_decision_action_as_str() {
        assert_eq!(DecisionAction::Approve.as_str(), "approve");
        assert_eq!(DecisionAction::Reject.as_str(), "reject");
        assert_eq!(DecisionAction::Cancel.as_str(), "cancel");
        assert_eq!(DecisionAction::RequestRevision.as_str(), "request_revision");
    }
}
