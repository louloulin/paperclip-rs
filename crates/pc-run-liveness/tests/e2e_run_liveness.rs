//! End-to-end tests for `pc-run-liveness`.
//!
//! 包含：
//! - 纯函数 classifier 测试（与 Node `run-liveness.ts` 1:1 对齐）
//! - Hook 测试
//! - 各 liveness state 的边界 case 测试

use pc_run_liveness::{
    classify_run_actionability, classify_run_liveness, declared_blocker,
    has_concrete_action_evidence, has_useful_output, is_planning_or_document_task,
    looks_like_planning_only, RecordingRunLivenessHook, RunLivenessActionability,
    RunLivenessClassificationInput, RunLivenessEvidenceInput, RunLivenessHook,
    RunLivenessHookEvent, RunLivenessIssueInput, RunLivenessService, RunLivenessState,
};
use serde_json::json;
use std::sync::Arc;

// ============================================================================
// classify_run_actionability 测试
// ============================================================================

fn input_with_text(text: &str) -> RunLivenessClassificationInput {
    RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        issue: None,
        result_json: Some(json!({"summary": text})),
        issue_comment_bodies: None,
        continuation_summary_body: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        error: None,
        error_code: None,
        continuation_attempt: None,
        evidence: None,
    }
}

#[test]
fn r667_actionability_empty_text_returns_unknown() {
    let input = input_with_text("");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::Unknown
    );
}

#[test]
fn r667_actionability_negated_blocker_with_runnable() {
    let input = input_with_text("No blockers. Let me run cargo build now.");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::Runnable
    );
}

#[test]
fn r667_actionability_negated_blocker_no_runnable_returns_unknown() {
    let input = input_with_text("No blockers here.");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::Unknown
    );
}

#[test]
fn r667_actionability_approval_required() {
    let input = input_with_text("This action requires approval from the board.");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::ApprovalRequired
    );
}

#[test]
fn r667_actionability_external_blocker_credential() {
    let input = input_with_text("Waiting on API key from admin.");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::BlockedExternal
    );
}

#[test]
fn r667_actionability_external_blocker_secret() {
    let input = input_with_text("Need access to production credentials before continuing.");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::BlockedExternal
    );
}

#[test]
fn r667_actionability_manager_review() {
    let input = input_with_text("This is a security-sensitive change requiring manager review.");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::ManagerReview
    );
}

#[test]
fn r667_actionability_runnable_command() {
    let input = input_with_text("Run pnpm test next");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::Runnable
    );
}

#[test]
fn r667_actionability_runnable_verb() {
    let input = input_with_text("I will implement the feature next.");
    assert_eq!(
        classify_run_actionability(&input),
        RunLivenessActionability::Runnable
    );
}

// ============================================================================
// has_useful_output 测试
// ============================================================================

#[test]
fn r667_has_useful_output_empty() {
    let input = RunLivenessClassificationInput::default();
    assert!(!has_useful_output(&input));
}

#[test]
fn r667_has_useful_output_with_summary() {
    let input = input_with_text("Some output");
    assert!(has_useful_output(&input));
}

#[test]
fn r667_has_useful_output_with_comment() {
    let mut input = RunLivenessClassificationInput::default();
    input.issue_comment_bodies = Some(vec!["A comment".to_string()]);
    assert!(has_useful_output(&input));
}

// ============================================================================
// declared_blocker 测试
// ============================================================================

#[test]
fn r667_declared_blocker_issue_status_blocked() {
    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        issue: Some(RunLivenessIssueInput {
            status: "blocked".to_string(),
            title: "test".to_string(),
            description: None,
        }),
        result_json: None,
        issue_comment_bodies: None,
        continuation_summary_body: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        error: None,
        error_code: None,
        continuation_attempt: None,
        evidence: None,
    };
    assert!(declared_blocker(&input));
}

#[test]
fn r667_declared_blocker_approval_required_text() {
    let input = input_with_text("Need approval from user");
    assert!(declared_blocker(&input));
}

#[test]
fn r667_declared_blocker_returns_false_for_runnable() {
    let input = input_with_text("Continue implementing feature X");
    assert!(!declared_blocker(&input));
}

// ============================================================================
// looks_like_planning_only 测试
// ============================================================================

#[test]
fn r667_planning_only_with_next_steps_label() {
    let input = input_with_text("Next steps:\n- Do thing one\n- Do thing two");
    assert!(looks_like_planning_only(&input));
}

#[test]
fn r667_planning_only_with_plan_label() {
    let input = input_with_text("Plan: \n- step 1\n- step 2");
    assert!(looks_like_planning_only(&input));
}

#[test]
fn r667_planning_only_with_ill_examine() {
    let input = input_with_text("I'll first inspect the file structure.");
    assert!(looks_like_planning_only(&input));
}

#[test]
fn r667_planning_only_returns_false_for_concrete_output() {
    let input = input_with_text("Updated foo.ts to fix the bug.");
    assert!(!looks_like_planning_only(&input));
}

#[test]
fn r667_planning_only_empty_returns_false() {
    let input = input_with_text("");
    assert!(!looks_like_planning_only(&input));
}

// ============================================================================
// is_planning_or_document_task 测试
// ============================================================================

#[test]
fn r667_planning_task_with_title_keyword() {
    let issue = RunLivenessIssueInput {
        status: "todo".to_string(),
        title: "Write plan for feature X".to_string(),
        description: None,
    };
    assert!(is_planning_or_document_task(Some(&issue)));
}

#[test]
fn r667_planning_task_with_description_keyword() {
    let issue = RunLivenessIssueInput {
        status: "todo".to_string(),
        title: "Implement feature X".to_string(),
        description: Some("Please write a research report on the topic.".to_string()),
    };
    assert!(is_planning_or_document_task(Some(&issue)));
}

#[test]
fn r667_implementation_task_returns_false() {
    let issue = RunLivenessIssueInput {
        status: "todo".to_string(),
        title: "Implement feature X".to_string(),
        description: Some("Add a new button to the dashboard.".to_string()),
    };
    assert!(!is_planning_or_document_task(Some(&issue)));
}

#[test]
fn r667_planning_task_with_none_returns_false() {
    assert!(!is_planning_or_document_task(None));
}

// ============================================================================
// has_concrete_action_evidence 测试
// ============================================================================

#[test]
fn r667_no_evidence_returns_false() {
    assert!(!has_concrete_action_evidence(None));
}

#[test]
fn r667_only_workspace_ops_returns_false() {
    // workspace_operations_created alone does NOT count
    let evidence = RunLivenessEvidenceInput {
        workspace_operations_created: 5,
        ..Default::default()
    };
    assert!(!has_concrete_action_evidence(Some(&evidence)));
}

#[test]
fn r667_with_issue_comments_returns_true() {
    let evidence = RunLivenessEvidenceInput {
        issue_comments_created: 1,
        ..Default::default()
    };
    assert!(has_concrete_action_evidence(Some(&evidence)));
}

#[test]
fn r667_with_document_revisions_returns_true() {
    let evidence = RunLivenessEvidenceInput {
        document_revisions_created: 1,
        ..Default::default()
    };
    assert!(has_concrete_action_evidence(Some(&evidence)));
}

#[test]
fn r667_with_work_products_returns_true() {
    let evidence = RunLivenessEvidenceInput {
        work_products_created: 1,
        ..Default::default()
    };
    assert!(has_concrete_action_evidence(Some(&evidence)));
}

#[test]
fn r667_with_activity_events_returns_true() {
    let evidence = RunLivenessEvidenceInput {
        activity_events_created: 1,
        ..Default::default()
    };
    assert!(has_concrete_action_evidence(Some(&evidence)));
}

#[test]
fn r667_with_tool_events_returns_true() {
    let evidence = RunLivenessEvidenceInput {
        tool_or_action_events_created: 1,
        ..Default::default()
    };
    assert!(has_concrete_action_evidence(Some(&evidence)));
}

// ============================================================================
// classify_run_liveness 主分类测试
// ============================================================================

#[test]
fn r667_classify_interrupted() {
    let mut input = input_with_text("test");
    input.run_status = "interrupted".to_string();
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::NeedsFollowup);
}

#[test]
fn r667_classify_interrupted_with_code() {
    let mut input = input_with_text("test");
    input.run_status = "interrupted".to_string();
    input.error_code = Some("E_TIMEOUT".to_string());
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::NeedsFollowup);
    assert!(result.liveness_reason.contains("E_TIMEOUT"));
}

#[test]
fn r667_classify_failed_with_code() {
    let mut input = input_with_text("test");
    input.run_status = "failed".to_string();
    input.error_code = Some("E001".to_string());
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Failed);
    assert!(result.liveness_reason.contains("E001"));
}

#[test]
fn r667_classify_failed_with_unmanaged_background_task() {
    let input = RunLivenessClassificationInput {
        run_status: "failed".to_string(),
        result_json: Some(json!({"stopReason": "unmanaged_background_task_stopped"})),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Failed);
    assert!(result.liveness_reason.contains("unmanaged background task"));
}

#[test]
fn r667_classify_completed_done() {
    let mut input = input_with_text("test");
    input.issue = Some(RunLivenessIssueInput {
        status: "done".to_string(),
        title: "test".to_string(),
        description: None,
    });
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Completed);
}

#[test]
fn r667_classify_completed_cancelled() {
    let mut input = input_with_text("test");
    input.issue = Some(RunLivenessIssueInput {
        status: "cancelled".to_string(),
        title: "test".to_string(),
        description: None,
    });
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Completed);
}

#[test]
fn r667_classify_blocked_issue_status() {
    let mut input = input_with_text("test");
    input.issue = Some(RunLivenessIssueInput {
        status: "blocked".to_string(),
        title: "test".to_string(),
        description: None,
    });
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Blocked);
}

#[test]
fn r667_classify_blocked_external_text() {
    let input = input_with_text("Need API key from admin before continuing.");
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Blocked);
}

#[test]
fn r667_classify_empty_response() {
    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::EmptyResponse);
}

#[test]
fn r667_classify_advanced_with_evidence() {
    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        result_json: Some(json!({"summary": "Updated foo.ts"})),
        evidence: Some(RunLivenessEvidenceInput {
            issue_comments_created: 1,
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Advanced);
    assert!(result.liveness_reason.contains("issue comment"));
}

#[test]
fn r667_classify_advanced_planning_exempt() {
    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        result_json: Some(json!({"summary": "Plan: \n- step 1"})),
        issue: Some(RunLivenessIssueInput {
            status: "todo".to_string(),
            title: "Write plan for feature X".to_string(),
            description: None,
        }),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Advanced);
    assert!(result.liveness_reason.contains("Planning/document task"));
}

#[test]
fn r667_classify_plan_only_runnable() {
    let input = input_with_text("I'll implement the next feature.");
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::PlanOnly);
}

#[test]
fn r667_classify_needs_followup_planning_not_safe() {
    // "I'll wait for ..." matches planning pattern but has no runnable verb
    // and is about waiting, not executing
    let input = input_with_text("I will wait for the upstream dependency to finish.");
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::NeedsFollowup);
}

#[test]
fn r667_classify_needs_followup_useful_output() {
    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        result_json: Some(json!({"summary": "Some text output without actions"})),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::NeedsFollowup);
}

#[test]
fn r667_classify_extracts_next_action() {
    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        result_json: Some(json!({"summary": "Some output.\n\nNext steps:\n- Check the dashboard"})),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert!(result.next_action.is_some());
    assert!(result.next_action.as_ref().unwrap().contains("Check"));
}

#[test]
fn r667_classify_continuation_attempt_propagates() {
    let mut input = input_with_text("Some output");
    input.continuation_attempt = Some(3);
    let result = classify_run_liveness(&input);
    assert_eq!(result.continuation_attempt, 3);
}

// ============================================================================
// Service + Hook 测试
// ============================================================================

#[test]
fn r667_service_classify_with_hook() {
    let hook = Arc::new(RecordingRunLivenessHook::new());
    let svc = RunLivenessService::with_hook(hook.clone());

    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        result_json: Some(json!({"summary": "Updated code"})),
        evidence: Some(RunLivenessEvidenceInput {
            issue_comments_created: 2,
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = svc.classify(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Advanced);

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        RunLivenessHookEvent::BeforeClassify { .. }
    ));
    assert!(matches!(
        events[1],
        RunLivenessHookEvent::AfterClassify { .. }
    ));
}

#[test]
fn r667_hook_clear() {
    let hook = Arc::new(RecordingRunLivenessHook::new());
    hook.before_classify(&RunLivenessClassificationInput::default());
    assert_eq!(hook.len(), 1);
    hook.clear();
    assert!(hook.is_empty());
}

#[test]
fn r667_default_service_uses_noop_hook() {
    let svc = RunLivenessService::new();
    let _ = svc.classify(&RunLivenessClassificationInput::default());
    // Just exercise — no panic = pass
}

// ============================================================================
// RunLivenessState enum 测试
// ============================================================================

#[test]
fn r667_state_as_str() {
    assert_eq!(RunLivenessState::Advanced.as_str(), "advanced");
    assert_eq!(RunLivenessState::Failed.as_str(), "failed");
    assert_eq!(RunLivenessState::NeedsFollowup.as_str(), "needs_followup");
    assert_eq!(RunLivenessState::PlanOnly.as_str(), "plan_only");
    assert_eq!(RunLivenessState::EmptyResponse.as_str(), "empty_response");
    assert_eq!(RunLivenessState::Completed.as_str(), "completed");
    assert_eq!(RunLivenessState::Blocked.as_str(), "blocked");
}

#[test]
fn r667_actionability_as_str() {
    assert_eq!(RunLivenessActionability::Runnable.as_str(), "runnable");
    assert_eq!(
        RunLivenessActionability::ManagerReview.as_str(),
        "manager_review"
    );
    assert_eq!(
        RunLivenessActionability::BlockedExternal.as_str(),
        "blocked_external"
    );
    assert_eq!(
        RunLivenessActionability::ApprovalRequired.as_str(),
        "approval_required"
    );
    assert_eq!(RunLivenessActionability::Unknown.as_str(), "unknown");
}

// ============================================================================
// 边界 / 综合场景
// ============================================================================

#[test]
fn r667_classify_strips_noisy_transcript() {
    // Noisy lines should be filtered before classification.
    let lines = [
        "command: ls",
        "tool: bash",
        "{\"tool\": \"bash\"}",
        "Next steps: check the dashboard",
    ];
    let noisy_text = lines.join("\n");
    let input = RunLivenessClassificationInput {
        run_status: "succeeded".to_string(),
        stdout_excerpt: Some(noisy_text),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert!(matches!(
        result.liveness_state,
        RunLivenessState::PlanOnly | RunLivenessState::NeedsFollowup | RunLivenessState::Advanced
    ));
}

#[test]
fn r667_classify_unmanaged_via_unmanaged_background_task_obj() {
    let input = RunLivenessClassificationInput {
        run_status: "failed".to_string(),
        result_json: Some(json!({
            "unmanagedBackgroundTask": {
                "stopped": true,
                "reason": "unmanaged background task stopped; no durable live path"
            }
        })),
        ..Default::default()
    };
    let result = classify_run_liveness(&input);
    assert_eq!(result.liveness_state, RunLivenessState::Failed);
}
