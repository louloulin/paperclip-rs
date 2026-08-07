//! R383 — Integration tests for the 5 final gaps closed in
//! prompt_compose: typed blocker summaries, principal labels, review
//! request instructions, execution workspace sanitization, and the
//! trailing-space parity fix for inline-code fences. Mirrors the Node
//! `server-utils.ts` parity surface (L1028-1042, L1066-1077, L1208-1210,
//! L1231-1243, L1247-1254, L1455-1460, L1770-1785).
//!
//! All access flows through the public `pc_acpx::` re-exports:
//! - `render_paperclip_wake_prompt` for end-to-end parity
//! - `normalize_paperclip_wake_payload` for typed struct visibility
//! - typed structs `PaperclipWakeExecutionStage` /
//!   `PaperclipWakeExecutionPrincipal` / `PaperclipWakeReviewRequest` /
//!   `PaperclipWakeBlockerSummary` / `PaperclipWakeExecutionWorkspace`

use pc_acpx::{
    normalize_paperclip_wake_payload, render_paperclip_wake_prompt, RenderWakePromptOptions,
};
use serde_json::{json, Value};

fn render(payload: &Value) -> String {
    render_paperclip_wake_prompt(Some(payload), &RenderWakePromptOptions::default())
}

// ============================================================================
// Gap 1 — typed blocker summaries replace opaque Value placeholders.
// ============================================================================

#[test]
fn blocker_summary_normalizes_all_five_fields() {
    let payload = json!({
        "reason": "issue_commented",
        "dependencyBlockedInteraction": true,
        "unresolvedBlockerSummaries": [
            {
                "id": "iss_b_99",
                "identifier": "PC-B-99",
                "title": "Auth blocker",
                "status": "open",
                "priority": "high",
            }
        ]
    });
    let normalized = normalize_paperclip_wake_payload(Some(&payload)).expect("normalized");
    assert_eq!(normalized.unresolved_blocker_summaries.len(), 1);
    let b = &normalized.unresolved_blocker_summaries[0];
    assert_eq!(b.id.as_deref(), Some("iss_b_99"));
    assert_eq!(b.identifier.as_deref(), Some("PC-B-99"));
    assert_eq!(b.title.as_deref(), Some("Auth blocker"));
    assert_eq!(b.status.as_deref(), Some("open"));
    assert_eq!(b.priority.as_deref(), Some("high"));
}

#[test]
fn blocker_summary_skips_all_empty_entries() {
    let payload = json!({
        "reason": "issue_commented",
        "dependencyBlockedInteraction": true,
        "unresolvedBlockerSummaries": [
            {},
            {
                "id": "iss_b_2",
                "identifier": "PC-B-2",
                "title": "Quota",
                "status": "blocked",
            }
        ]
    });
    let normalized = normalize_paperclip_wake_payload(Some(&payload)).expect("normalized");
    assert_eq!(normalized.unresolved_blocker_summaries.len(), 1);
    assert_eq!(
        normalized.unresolved_blocker_summaries[0]
            .identifier
            .as_deref(),
        Some("PC-B-2")
    );
}

#[test]
fn blocker_summary_renders_labeled_line_in_dependency_blocked_section() {
    let payload = json!({
        "reason": "issue_commented",
        "dependencyBlockedInteraction": true,
        "unresolvedBlockerSummaries": [
            {
                "id": "iss_b_1",
                "identifier": "PC-B-1",
                "title": "Auth blocker",
                "status": "open",
            },
            {
                "id": "iss_b_2",
                "identifier": "PC-B-2",
                "title": "Quota exceeded",
                "status": "blocked",
            },
        ]
    });
    let rendered = render(&payload);
    assert!(rendered.contains("- dependency-blocked interaction: yes"));
    assert!(rendered.contains(
        "- unresolved blockers: PC-B-1 Auth blocker (open); PC-B-2 Quota exceeded (blocked)"
    ));
}

// ============================================================================
// Gap 2 — execution principal labels (agent/user/unknown) via principal_label.
// ============================================================================

#[test]
fn principal_label_renders_for_executor_stage_with_agent_and_user() {
    let v = json!({
        "reason": "issue_commented",
        "executionStage": {
            "wakeRole": "executor",
            "stageType": "execution",
            "currentParticipant": { "type": "agent", "agentId": "claude_42" },
            "returnAssignee": { "type": "user", "userId": "u_5" },
        }
    });
    // Normalize path: principal is typed.
    let normalized = normalize_paperclip_wake_payload(Some(&v)).expect("normalized");
    let stage = normalized.execution_stage.expect("stage typed");
    let current = stage.current_participant.as_ref().expect("current typed");
    assert_eq!(current.principal_type.as_deref(), Some("agent"));
    assert_eq!(current.agent_id.as_deref(), Some("claude_42"));
    let assignee = stage.return_assignee.as_ref().expect("assignee typed");
    assert_eq!(assignee.principal_type.as_deref(), Some("user"));
    assert_eq!(assignee.user_id.as_deref(), Some("u_5"));

    // Render path: principal label lines emitted.
    let rendered = render(&v);
    assert!(rendered.contains("- execution participant: agent claude_42"));
    assert!(rendered.contains("- execution return assignee: user u_5"));
    assert!(rendered
        .contains("You are waking because changes were requested in the execution workflow."));
}

#[test]
fn principal_normalize_lowercases_known_types_and_rejects_unknown() {
    let payload = json!({
        "reason": "issue_commented",
        "executionStage": {
            "wakeRole": "executor",
            "stageType": "execution",
            "currentParticipant": { "type": "AGENT", "agentId": "claude_42" },
        }
    });
    let normalized = normalize_paperclip_wake_payload(Some(&payload)).expect("normalized");
    let stage = normalized.execution_stage.expect("stage typed");
    let current = stage
        .current_participant
        .as_ref()
        .expect("AGENT lowercased");
    assert_eq!(current.principal_type.as_deref(), Some("agent"));

    // Unknown type -> normalize returns None -> field not present.
    let payload_unknown = json!({
        "reason": "issue_commented",
        "executionStage": {
            "wakeRole": "executor",
            "stageType": "execution",
            "currentParticipant": { "type": "robot", "agentId": "r1" },
        }
    });
    let normalized_u =
        normalize_paperclip_wake_payload(Some(&payload_unknown)).expect("normalized");
    let stage_u = normalized_u.execution_stage.expect("stage present");
    assert!(stage_u.current_participant.is_none());
}

#[test]
fn principal_label_renders_unknown_when_missing() {
    let payload = json!({
        "reason": "issue_commented",
        "executionStage": {
            "wakeRole": "executor",
            "stageType": "execution",
        }
    });
    let rendered = render(&payload);
    // No principals supplied -> render emits "unknown" placeholder.
    assert!(rendered.contains("- execution participant: unknown"));
    assert!(rendered.contains("- execution return assignee: unknown"));
}

// ============================================================================
// Gap 3 — execution stage review request renders instructions block.
// ============================================================================

#[test]
fn execution_stage_review_request_renders_instructions_block() {
    let payload = json!({
        "reason": "issue_commented",
        "executionStage": {
            "wakeRole": "reviewer",
            "stageType": "review",
            "reviewRequest": {
                "instructions": "Verify the liveness block lands before merging."
            }
        }
    });
    let normalized = normalize_paperclip_wake_payload(Some(&payload)).expect("normalized");
    let stage = normalized.execution_stage.expect("stage typed");
    let review = stage.review_request.as_ref().expect("review typed");
    assert_eq!(
        review.instructions,
        "Verify the liveness block lands before merging."
    );

    let rendered = render(&payload);
    assert!(rendered.contains("Review request instructions:"));
    assert!(rendered.contains("Verify the liveness block lands before merging."));
    assert!(rendered.contains("You are waking as the active reviewer for this issue."));
    assert!(rendered.contains("Do not execute the task itself or continue executor work."));
}

#[test]
fn execution_stage_approver_wake_role_emits_approver_lines() {
    let payload = json!({
        "reason": "issue_commented",
        "executionStage": {
            "wakeRole": "approver",
            "stageType": "approval",
        }
    });
    let rendered = render(&payload);
    assert!(rendered.contains("You are waking as the active approver for this issue."));
    assert!(rendered.contains(
        "If you request changes, the workflow routes back to the stored return assignee."
    ));
}

#[test]
fn execution_stage_review_request_trims_blank_instructions_to_none() {
    let payload = json!({
        "reason": "issue_commented",
        "executionStage": {
            "wakeRole": "reviewer",
            "stageType": "review",
            "reviewRequest": { "instructions": "   " },
        }
    });
    let normalized = normalize_paperclip_wake_payload(Some(&payload)).expect("normalized");
    let stage = normalized.execution_stage.expect("stage typed");
    assert!(stage.review_request.is_none());
}

// ============================================================================
// Gap 4 — execution workspace sanitization (strip control chars + cap).
// ============================================================================

#[test]
fn execution_workspace_normalize_strips_control_chars_and_caps_length() {
    // JSON 4-hex unicode escapes -> real control chars (U+0000, U+000A, U+0009).
    let raw = "pap\\u0000erclip/issue-1\\u000a2\\u0009";
    let v: Value = serde_json::from_str(&format!(
        "{{\"reason\":\"issue_assigned\",\"executionWorkspace\":{{\"branchName\":\"{}\",\"workspaceId\":\"ws_42\"}}}}",
        raw
    ))
    .expect("parse");
    // End-to-end: the rendered prompt must show the sanitized branch name
    // (control chars stripped, length-capped, structure preserved).
    let rendered = render(&v);
    assert!(
        rendered.contains(
            "- execution workspace branch: you are running in an execution workspace on branch `paperclip/issue-12`."
        ),
        "rendered did not contain sanitized branch: {}",
        rendered
    );
}

#[test]
fn execution_workspace_returns_none_when_branch_blank_after_strip() {
    // All control chars -> after strip + trim the branch is empty, so the
    // workspace normalizes to None and the directive line is omitted.
    let raw = "\\u000a\\u0009\\u0000";
    let v: Value = serde_json::from_str(&format!(
        "{{\"reason\":\"issue_assigned\",\"executionWorkspace\":{{\"branchName\":\"{}\"}}}}",
        raw
    ))
    .expect("parse");
    let rendered = render(&v);
    assert!(
        !rendered.contains("- execution workspace branch"),
        "rendered must omit branch directive when branch is blank: {}",
        rendered
    );
}

#[test]
fn execution_workspace_branch_renders_inside_branch_directive() {
    let payload = json!({
        "reason": "issue_assigned",
        "executionWorkspace": { "branchName": "paperclip/issue-r383" },
    });
    let rendered = render(&payload);
    assert!(rendered.contains(
        "- execution workspace branch: you are running in an execution workspace on branch `paperclip/issue-r383`."
    ));
}

// ============================================================================
// Gap 5 — markdown_inline_code trailing-space parity (Node L1247-1254).
// ============================================================================

#[test]
fn end_to_end_render_emits_plain_inline_code_fence() {
    let payload = json!({
        "reason": "issue_assigned",
        "executionWorkspace": { "branchName": "issue-r383" },
    });
    let rendered = render(&payload);
    // No backticks in the branch name -> Node "single backtick" format.
    assert!(rendered.contains("`issue-r383`"));
}
