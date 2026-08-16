#![forbid(unsafe_code)]

//! Workflow run state machine transition matrix — 1:1 port of
//! paperclip/server/src/services/workflow/state-machine.ts (logic only).
//!
//! R733: 零依赖 state machine transition validation（合法 / 非法转移）。

use crate::types::{StepStatus, WorkflowRunState};

/// 判断 from → to 是否为合法的 run state transition。
///
/// 合法转移（与 Node state-machine.ts 一致）：
/// - Pending → Queued | Running | Cancelled
/// - Queued → Running | Cancelled
/// - Running → Succeeded | Failed | Cancelled
/// - Succeeded / Failed / Cancelled → 任意（终态不可达，但保留 forward path 兼容）
pub fn is_valid_run_state_transition(from: WorkflowRunState, to: WorkflowRunState) -> bool {
    use WorkflowRunState as S;
    match (from, to) {
        (S::Pending, S::Queued) | (S::Pending, S::Running) | (S::Pending, S::Cancelled) => true,
        (S::Queued, S::Running) | (S::Queued, S::Cancelled) => true,
        (S::Running, S::Succeeded) | (S::Running, S::Failed) | (S::Running, S::Cancelled) => true,
        (S::Succeeded, S::Succeeded) | (S::Failed, S::Failed) | (S::Cancelled, S::Cancelled) => true,
        _ => false,
    }
}

/// 判断 from → to 是否为合法的 step status transition。
///
/// 合法转移：
/// - Pending → Running | Skipped
/// - Running → Succeeded | Failed | Skipped
/// - Succeeded / Failed / Skipped → 同态（idempotent）
pub fn is_valid_step_status_transition(from: StepStatus, to: StepStatus) -> bool {
    use StepStatus as S;
    match (from, to) {
        (S::Pending, S::Running) | (S::Pending, S::Skipped) => true,
        (S::Running, S::Succeeded) | (S::Running, S::Failed) | (S::Running, S::Skipped) => true,
        (S::Succeeded, S::Succeeded) | (S::Failed, S::Failed) | (S::Skipped, S::Skipped) => true,
        _ => false,
    }
}

/// 判断 step 当前状态是否允许重试（仅 Failed → Pending 视为可重试）。
pub fn is_retryable_step_status(from: StepStatus) -> bool {
    matches!(from, StepStatus::Failed)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use crate::types::{StepStatus, WorkflowRunState};

    #[test]
    fn run_state_pending_to_queued_valid() {
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Pending,
            WorkflowRunState::Queued
        ));
    }

    #[test]
    fn run_state_pending_to_running_valid() {
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Pending,
            WorkflowRunState::Running
        ));
    }

    #[test]
    fn run_state_pending_to_succeeded_invalid() {
        assert!(!is_valid_run_state_transition(
            WorkflowRunState::Pending,
            WorkflowRunState::Succeeded
        ));
    }

    #[test]
    fn run_state_queued_to_running_valid() {
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Queued,
            WorkflowRunState::Running
        ));
    }

    #[test]
    fn run_state_queued_to_succeeded_invalid() {
        assert!(!is_valid_run_state_transition(
            WorkflowRunState::Queued,
            WorkflowRunState::Succeeded
        ));
    }

    #[test]
    fn run_state_running_to_succeeded_valid() {
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Running,
            WorkflowRunState::Succeeded
        ));
    }

    #[test]
    fn run_state_running_to_failed_valid() {
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Running,
            WorkflowRunState::Failed
        ));
    }

    #[test]
    fn run_state_terminal_idempotent() {
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Succeeded,
            WorkflowRunState::Succeeded
        ));
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Failed,
            WorkflowRunState::Failed
        ));
        assert!(is_valid_run_state_transition(
            WorkflowRunState::Cancelled,
            WorkflowRunState::Cancelled
        ));
    }

    #[test]
    fn run_state_terminal_to_running_invalid() {
        assert!(!is_valid_run_state_transition(
            WorkflowRunState::Succeeded,
            WorkflowRunState::Running
        ));
        assert!(!is_valid_run_state_transition(
            WorkflowRunState::Failed,
            WorkflowRunState::Running
        ));
        assert!(!is_valid_run_state_transition(
            WorkflowRunState::Cancelled,
            WorkflowRunState::Running
        ));
    }

    #[test]
    fn step_pending_to_running_valid() {
        assert!(is_valid_step_status_transition(
            StepStatus::Pending,
            StepStatus::Running
        ));
    }

    #[test]
    fn step_pending_to_succeeded_invalid() {
        assert!(!is_valid_step_status_transition(
            StepStatus::Pending,
            StepStatus::Succeeded
        ));
    }

    #[test]
    fn step_running_to_succeeded_valid() {
        assert!(is_valid_step_status_transition(
            StepStatus::Running,
            StepStatus::Succeeded
        ));
    }

    #[test]
    fn step_running_to_failed_valid() {
        assert!(is_valid_step_status_transition(
            StepStatus::Running,
            StepStatus::Failed
        ));
    }

    #[test]
    fn step_terminal_idempotent() {
        assert!(is_valid_step_status_transition(
            StepStatus::Succeeded,
            StepStatus::Succeeded
        ));
        assert!(is_valid_step_status_transition(
            StepStatus::Failed,
            StepStatus::Failed
        ));
        assert!(is_valid_step_status_transition(
            StepStatus::Skipped,
            StepStatus::Skipped
        ));
    }

    #[test]
    fn step_terminal_to_running_invalid() {
        assert!(!is_valid_step_status_transition(
            StepStatus::Succeeded,
            StepStatus::Running
        ));
        assert!(!is_valid_step_status_transition(
            StepStatus::Failed,
            StepStatus::Running
        ));
    }

    #[test]
    fn is_retryable_step_failed_only() {
        assert!(is_retryable_step_status(StepStatus::Failed));
        assert!(!is_retryable_step_status(StepStatus::Succeeded));
        assert!(!is_retryable_step_status(StepStatus::Skipped));
        assert!(!is_retryable_step_status(StepStatus::Pending));
        assert!(!is_retryable_step_status(StepStatus::Running));
    }
}
