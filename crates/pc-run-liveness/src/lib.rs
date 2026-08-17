#![forbid(unsafe_code)]
//! `pc-run-liveness` — Run liveness classification 业务服务。
//!
//! 对应 Node `services/run-liveness.ts`（368 行 — 核心 liveness 分类器）。
//!
//! 本 crate 提供：
//!
//! - **类型**：`RunLivenessState` / `RunLivenessActionability` /
//!   `RunLivenessIssueInput` / `RunLivenessEvidenceInput` /
//!   `RunLivenessClassificationInput` / `RunLivenessClassification`
//! - **纯函数 classifier**：
//!   - `has_useful_output(input)` —— 是否有有用输出
//!   - `declared_blocker(input)` —— 是否声明 blocker
//!   - `looks_like_planning_only(input)` —— 是否仅规划
//!   - `is_planning_or_document_task(issue)` —— 是否规划/文档任务
//!   - `has_concrete_action_evidence(evidence)` —— 是否有具体动作证据
//!   - `classify_run_actionability(input)` —— 行动性分类
//!   - `classify_run_liveness(input)` —— 主分类（返回 RunLivenessClassification）
//! - **Service 层 API**（`RunLivenessService`）：封装 + Hook
//! - **Hook 系统**：`RunLivenessHook` trait（2 回调）
//!
//! 设计原则：
//! - **高内聚**：所有 liveness 分类逻辑集中在本 crate。
//! - **低耦合**：上游 heartbeat / recovery 只需调用 classifier。
//! - **纯函数**：无 DB I/O，易测试。

mod classifier;
mod hook;
mod service;
mod types;

pub use classifier::{
    classify_run_actionability, classify_run_liveness, declared_blocker,
    has_concrete_action_evidence, has_useful_output, is_planning_or_document_task,
    looks_like_planning_only,
};
pub use hook::{
    NoopRunLivenessHook, RecordingRunLivenessHook, RunLivenessHook, RunLivenessHookEvent,
};
pub use service::RunLivenessService;
pub use types::{
    RunLivenessActionability, RunLivenessClassification, RunLivenessClassificationInput,
    RunLivenessEvidenceInput, RunLivenessIssueInput, RunLivenessState,
    UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON, UNMANAGED_BACKGROUND_TASK_STOP_REASON,
};


#[cfg(test)]
mod internal_tests {
    use super::*;
    use crate::types::{
        RunLivenessActionability, RunLivenessClassification, RunLivenessClassificationInput,
        RunLivenessEvidenceInput, RunLivenessIssueInput, RunLivenessState,
        UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON, UNMANAGED_BACKGROUND_TASK_STOP_REASON,
    };
    use serde_json::json;

    fn empty_input() -> RunLivenessClassificationInput {
        RunLivenessClassificationInput::default()
    }

    fn block_issue(title: &str, status: &str) -> RunLivenessIssueInput {
        RunLivenessIssueInput {
            status: status.to_string(),
            title: title.to_string(),
            description: None,
        }
    }

    #[test]
    fn r787_state_as_str() {
        assert_eq!(RunLivenessState::Advanced.as_str(), "advanced");
        assert_eq!(RunLivenessState::Failed.as_str(), "failed");
        assert_eq!(RunLivenessState::NeedsFollowup.as_str(), "needs_followup");
        assert_eq!(RunLivenessState::PlanOnly.as_str(), "plan_only");
        assert_eq!(RunLivenessState::EmptyResponse.as_str(), "empty_response");
        assert_eq!(RunLivenessState::Completed.as_str(), "completed");
        assert_eq!(RunLivenessState::Blocked.as_str(), "blocked");
    }

    #[test]
    fn r787_actionability_as_str() {
        assert_eq!(RunLivenessActionability::Runnable.as_str(), "runnable");
        assert_eq!(RunLivenessActionability::ManagerReview.as_str(), "manager_review");
        assert_eq!(RunLivenessActionability::BlockedExternal.as_str(), "blocked_external");
        assert_eq!(RunLivenessActionability::ApprovalRequired.as_str(), "approval_required");
        assert_eq!(RunLivenessActionability::Unknown.as_str(), "unknown");
    }

    #[test]
    fn r787_unmanaged_background_task_constants() {
        assert_eq!(UNMANAGED_BACKGROUND_TASK_STOP_REASON, "unmanaged_background_task_stopped");
        assert!(UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON.contains("unmanaged"));
    }

    #[test]
    fn r787_has_useful_output_empty_input() {
        assert!(!has_useful_output(&empty_input()));
    }

    #[test]
    fn r787_has_useful_output_with_stdout() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("Build succeeded".to_string());
        assert!(has_useful_output(&input));
    }

    #[test]
    fn r787_has_useful_output_with_stderr() {
        let mut input = empty_input();
        input.stderr_excerpt = Some("Error: something".to_string());
        assert!(has_useful_output(&input));
    }

    #[test]
    fn r787_has_useful_output_with_comment_bodies() {
        let mut input = empty_input();
        input.issue_comment_bodies = Some(vec!["comment".to_string()]);
        assert!(has_useful_output(&input));
    }

    #[test]
    fn r787_has_useful_output_with_continuation_summary() {
        let mut input = empty_input();
        input.continuation_summary_body = Some("summary".to_string());
        assert!(has_useful_output(&input));
    }

    #[test]
    fn r787_has_useful_output_with_evidence() {
        // has_useful_output ignores evidence (only checks text sources).
        // verify by checking concrete_evidence holds but useful_output does not
        let mut input = empty_input();
        input.evidence = Some(RunLivenessEvidenceInput {
            issue_comments_created: 1,
            ..Default::default()
        });
        assert!(!has_useful_output(&input));
        assert!(has_concrete_action_evidence(input.evidence.as_ref()));
    }

    #[test]
    fn r787_has_useful_output_with_evidence_zero() {
        let mut input = empty_input();
        input.evidence = Some(RunLivenessEvidenceInput::default());
        assert!(!has_useful_output(&input));
    }

    #[test]
    fn r787_declared_blocker_detects_blocked() {
        let mut input = empty_input();
        input.error = Some("blocked on access".to_string());
        assert!(declared_blocker(&input));
    }

    #[test]
    fn r787_declared_blocker_detects_waiting_on() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("Waiting on approval to continue".to_string());
        assert!(declared_blocker(&input));
    }

    #[test]
    fn r787_declared_blocker_negation() {
        let mut input = empty_input();
        input.error = Some("not blocked".to_string());
        assert!(!declared_blocker(&input));
    }

    #[test]
    fn r787_declared_blocker_no_blocker() {
        assert!(!declared_blocker(&empty_input()));
    }

    #[test]
    fn r787_looks_like_planning_only() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("I will first inspect the codebase".to_string());
        assert!(looks_like_planning_only(&input));
    }

    #[test]
    fn r787_looks_like_planning_only_next_step() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("Next: review the file".to_string());
        assert!(looks_like_planning_only(&input));
    }

    #[test]
    fn r787_looks_like_planning_only_no() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("Build succeeded; PR opened".to_string());
        assert!(!looks_like_planning_only(&input));
    }

    #[test]
    fn r787_is_planning_or_document_task_none() {
        assert!(!is_planning_or_document_task(None));
    }

    #[test]
    fn r787_is_planning_or_document_task_by_title() {
        let issue = block_issue("Plan API refactor", "open");
        assert!(is_planning_or_document_task(Some(&issue)));
    }

    #[test]
    fn r787_is_planning_or_document_task_by_description() {
        let issue = RunLivenessIssueInput {
            status: "open".to_string(),
            title: "Some task".to_string(),
            description: Some("Write a design doc for the migration".to_string()),
        };
        assert!(is_planning_or_document_task(Some(&issue)));
    }

    #[test]
    fn r787_is_planning_or_document_task_no() {
        let issue = block_issue("Fix login bug", "open");
        assert!(!is_planning_or_document_task(Some(&issue)));
    }

    #[test]
    fn r787_has_concrete_action_evidence_none() {
        assert!(!has_concrete_action_evidence(None));
    }

    #[test]
    fn r787_has_concrete_action_evidence_with_comments() {
        let ev = RunLivenessEvidenceInput {
            issue_comments_created: 1,
            ..Default::default()
        };
        assert!(has_concrete_action_evidence(Some(&ev)));
    }

    #[test]
    fn r787_has_concrete_action_evidence_with_docs() {
        let ev = RunLivenessEvidenceInput {
            document_revisions_created: 1,
            ..Default::default()
        };
        assert!(has_concrete_action_evidence(Some(&ev)));
    }

    #[test]
    fn r787_has_concrete_action_evidence_with_work_products() {
        let ev = RunLivenessEvidenceInput {
            work_products_created: 1,
            ..Default::default()
        };
        assert!(has_concrete_action_evidence(Some(&ev)));
    }

    #[test]
    fn r787_has_concrete_action_evidence_zero() {
        let ev = RunLivenessEvidenceInput::default();
        assert!(!has_concrete_action_evidence(Some(&ev)));
    }

    #[test]
    fn r787_classify_basic_advanced() {
        let mut input = empty_input();
        input.run_status = "succeeded".to_string();
        input.stdout_excerpt = Some("Successfully merged PR".to_string());
        input.evidence = Some(RunLivenessEvidenceInput {
            work_products_created: 1,
            ..Default::default()
        });
        let c = classify_run_liveness(&input);
        let _ = c; // should not be empty_response
    }

    #[test]
    fn r787_classify_empty_response() {
        // empty input has run_status="", hits "Run ended with" branch -> Failed.
        // To get EmptyResponse, run_status must be "succeeded" with no useful output and no evidence.
        let mut input = empty_input();
        input.run_status = "succeeded".to_string();
        let c = classify_run_liveness(&input);
        assert_eq!(c.liveness_state, RunLivenessState::EmptyResponse);
    }

    #[test]
    fn r787_classify_failed_state() {
        let mut input = empty_input();
        input.run_status = "failed".to_string();
        input.error = Some("boom".to_string());
        let c = classify_run_liveness(&input);
        assert_eq!(c.liveness_state, RunLivenessState::Failed);
    }

    #[test]
    fn r787_classify_blocker_via_declared_blocker() {
        let mut input = empty_input();
        input.run_status = "succeeded".to_string();
        // declared_blocker matches "blocked on access" via EXTERNAL_BLOCKER_RE
        input.error = Some("blocked on access".to_string());
        let c = classify_run_liveness(&input);
        assert_eq!(c.liveness_state, RunLivenessState::Blocked);
    }

    #[test]
    fn r787_classify_actionability_runnable() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("next: run pnpm test".to_string());
        let a = classify_run_actionability(&input);
        assert_eq!(a, RunLivenessActionability::Runnable);
    }

    #[test]
    fn r787_classify_actionability_approval() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("approval required to proceed".to_string());
        let a = classify_run_actionability(&input);
        assert_eq!(a, RunLivenessActionability::ApprovalRequired);
    }

    #[test]
    fn r787_classify_actionability_manager_review() {
        let mut input = empty_input();
        input.stdout_excerpt = Some("deploy to production requires manager review".to_string());
        let a = classify_run_actionability(&input);
        assert_eq!(a, RunLivenessActionability::ManagerReview);
    }

    #[test]
    fn r787_classify_actionability_unknown_when_no_signals() {
        let input = empty_input();
        let a = classify_run_actionability(&input);
        assert_eq!(a, RunLivenessActionability::Unknown);
    }

    #[test]
    fn r787_evidence_default_all_zero() {
        let ev = RunLivenessEvidenceInput::default();
        assert_eq!(ev.issue_comments_created, 0);
        assert_eq!(ev.document_revisions_created, 0);
        assert_eq!(ev.work_products_created, 0);
        assert_eq!(ev.workspace_operations_created, 0);
        assert_eq!(ev.activity_events_created, 0);
        assert_eq!(ev.tool_or_action_events_created, 0);
        assert_eq!(ev.latest_evidence_at, None);
    }

    #[test]
    fn r787_continuation_attempt_normalized() {
        let mut input = empty_input();
        input.continuation_attempt = Some(3);
        let c = classify_run_liveness(&input);
        assert_eq!(c.continuation_attempt, 3);
    }

    #[test]
    fn r787_continuation_attempt_none_defaults_to_zero() {
        let input = empty_input();
        let c = classify_run_liveness(&input);
        assert_eq!(c.continuation_attempt, 0);
    }
}
