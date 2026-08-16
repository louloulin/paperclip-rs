#![forbid(unsafe_code)]

//! Workflow type pure helpers — enum string conversion + trigger spec helpers.
//!
//! R732: zero-DB helpers for workflow data model (types.rs) conversion.

use crate::types::{StepStatus, TriggerSpec, WorkflowKind, WorkflowRunState, RoutineKind};

/// WorkflowKind → lowercase string label (对齐 serde tag).
pub fn workflow_kind_label(k: WorkflowKind) -> &'static str {
    match k {
        WorkflowKind::Routine => "routine",
        WorkflowKind::Pipeline => "pipeline",
    }
}

/// RoutineKind → lowercase string label.
pub fn routine_kind_label(k: RoutineKind) -> &'static str {
    match k {
        RoutineKind::Script => "script",
        RoutineKind::Webhook => "webhook",
        RoutineKind::Adapter => "adapter",
        RoutineKind::Plugin => "plugin",
    }
}

/// StepStatus → lowercase string label (对齐 serde rename_all).
pub fn step_status_label(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
    }
}

/// WorkflowRunState → lowercase string label.
pub fn workflow_run_state_label(s: WorkflowRunState) -> &'static str {
    match s {
        WorkflowRunState::Pending => "pending",
        WorkflowRunState::Queued => "queued",
        WorkflowRunState::Running => "running",
        WorkflowRunState::Succeeded => "succeeded",
        WorkflowRunState::Failed => "failed",
        WorkflowRunState::Cancelled => "cancelled",
    }
}

/// 判断 run state 是否为终态。
pub fn is_terminal_run_state(s: WorkflowRunState) -> bool {
    matches!(
        s,
        WorkflowRunState::Succeeded | WorkflowRunState::Failed | WorkflowRunState::Cancelled
    )
}

/// 判断 step status 是否为终态。
pub fn is_terminal_step_status(s: StepStatus) -> bool {
    matches!(
        s,
        StepStatus::Succeeded | StepStatus::Failed | StepStatus::Skipped
    )
}

/// 判断 trigger spec 是否为 cron 类型。
pub fn is_cron_trigger(t: &TriggerSpec) -> bool {
    matches!(t, TriggerSpec::Cron { .. })
}

/// 从 trigger spec 提取 cron expression（None 表示非 cron）。
pub fn cron_expression_of(t: &TriggerSpec) -> Option<&str> {
    match t {
        TriggerSpec::Cron { expression } => Some(expression.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use crate::types::{RoutineDefinition, RoutineKind, WorkflowKind};

    #[test]
    fn workflow_kind_label_round_trip() {
        assert_eq!(workflow_kind_label(WorkflowKind::Routine), "routine");
        assert_eq!(workflow_kind_label(WorkflowKind::Pipeline), "pipeline");
    }

    #[test]
    fn routine_kind_label_round_trip() {
        assert_eq!(routine_kind_label(RoutineKind::Script), "script");
        assert_eq!(routine_kind_label(RoutineKind::Webhook), "webhook");
        assert_eq!(routine_kind_label(RoutineKind::Adapter), "adapter");
        assert_eq!(routine_kind_label(RoutineKind::Plugin), "plugin");
    }

    #[test]
    fn step_status_label_all() {
        assert_eq!(step_status_label(StepStatus::Pending), "pending");
        assert_eq!(step_status_label(StepStatus::Running), "running");
        assert_eq!(step_status_label(StepStatus::Succeeded), "succeeded");
        assert_eq!(step_status_label(StepStatus::Failed), "failed");
        assert_eq!(step_status_label(StepStatus::Skipped), "skipped");
    }

    #[test]
    fn workflow_run_state_label_all() {
        assert_eq!(workflow_run_state_label(WorkflowRunState::Pending), "pending");
        assert_eq!(workflow_run_state_label(WorkflowRunState::Running), "running");
        assert_eq!(workflow_run_state_label(WorkflowRunState::Succeeded), "succeeded");
        assert_eq!(workflow_run_state_label(WorkflowRunState::Failed), "failed");
        assert_eq!(workflow_run_state_label(WorkflowRunState::Cancelled), "cancelled");
    }

    #[test]
    fn is_terminal_run_state_true_cases() {
        assert!(is_terminal_run_state(WorkflowRunState::Succeeded));
        assert!(is_terminal_run_state(WorkflowRunState::Failed));
        assert!(is_terminal_run_state(WorkflowRunState::Cancelled));
    }

    #[test]
    fn is_terminal_run_state_false_cases() {
        assert!(!is_terminal_run_state(WorkflowRunState::Pending));
        assert!(!is_terminal_run_state(WorkflowRunState::Running));
    }

    #[test]
    fn is_terminal_step_status_true_cases() {
        assert!(is_terminal_step_status(StepStatus::Succeeded));
        assert!(is_terminal_step_status(StepStatus::Failed));
        assert!(is_terminal_step_status(StepStatus::Skipped));
    }

    #[test]
    fn is_terminal_step_status_false_cases() {
        assert!(!is_terminal_step_status(StepStatus::Pending));
        assert!(!is_terminal_step_status(StepStatus::Running));
    }

    #[test]
    fn is_cron_trigger_matches_cron() {
        assert!(is_cron_trigger(&TriggerSpec::cron("*/5 * * * *")));
        assert!(!is_cron_trigger(&TriggerSpec::manual("alice")));
        assert!(!is_cron_trigger(&TriggerSpec::event("issue", "x")));
    }

    #[test]
    fn cron_expression_of_extracts() {
        let t = TriggerSpec::cron("0 0 * * *");
        assert_eq!(cron_expression_of(&t), Some("0 0 * * *"));
    }

    #[test]
    fn cron_expression_of_non_cron_returns_none() {
        assert_eq!(cron_expression_of(&TriggerSpec::manual("x")), None);
        assert_eq!(cron_expression_of(&TriggerSpec::event("k", "s")), None);
    }
}
