//! R382 集成测试 — 验证 5 个 R381 stub (plan review / task watchdog /
//! liveness continuation / annotation deltas / continuation summary)
//! 通过 `render_paperclip_wake_prompt` 端到端渲染后,真实数据进入 prompt。
//!
//! 覆盖场景:
//! - Continuation summary → prompt 含 "Issue continuation summary" + body
//! - Liveness continuation → prompt 含 attempt/maxAttempts/instruction
//! - Task watchdog → prompt 含 "## Task Watchdog Mandate" + WATCHDOG_DEFAULT_MANDATE
//! - Annotation deltas → prompt 含 "New plan annotation deltas" + thread body
//! - Plan review context → prompt 含 "Open plan comments to incorporate" + thread
//! - Typed normalize: NormalizedPaperclipWake 的 5 个新 typed 字段不再有 Value
//! - 5 个 stub 共存: 同一 payload 含 5 个 stub 字段,所有都正确渲染

use pc_acpx::{
    normalize_paperclip_wake_payload, render_paperclip_wake_prompt, RenderWakePromptOptions,
};
use serde_json::{json, Value};

// =============================================================================
// Continuation summary
// =============================================================================

#[test]
fn render_continuation_summary_includes_body_in_prompt() {
    let payload = json!({
        "reason": "issue_commented",
        "continuationSummary": {
            "key": "summary-1",
            "title": "Mid-run state",
            "body": "Stopped after the second pass; resume at step 5.",
            "bodyTruncated": true,
            "updatedAt": "2026-08-07T10:00:00Z",
        }
    });
    let rendered =
        render_paperclip_wake_prompt(Some(&payload), &RenderWakePromptOptions::default());
    assert!(rendered.contains("Issue continuation summary:"));
    assert!(rendered.contains("Stopped after the second pass; resume at step 5."));
    assert!(rendered.contains("[continuation summary truncated]"));
}

// =============================================================================
// Liveness continuation
// =============================================================================

#[test]
fn render_liveness_continuation_includes_attempt_and_instruction() {
    let payload = json!({
        "reason": "issue_commented",
        "livenessContinuation": {
            "attempt": 3,
            "maxAttempts": 5,
            "sourceRunId": "run-source-1",
            "state": "pending_continuation",
            "reason": "no output for 90s",
            "instruction": "Resume from durable progress; do not redo steps.",
        }
    });
    let rendered =
        render_paperclip_wake_prompt(Some(&payload), &RenderWakePromptOptions::default());
    assert!(rendered.contains("Run liveness continuation:"));
    assert!(rendered.contains("- attempt: 3/5"));
    assert!(rendered.contains("- source run: run-source-1"));
    assert!(rendered.contains("- liveness state: pending_continuation"));
    assert!(rendered.contains("- reason: no output for 90s"));
    assert!(rendered.contains("- instruction: Resume from durable progress"));
}

// =============================================================================
// Task watchdog
// =============================================================================

#[test]
fn render_task_watchdog_includes_mandate_and_capabilities() {
    let payload = json!({
        "reason": "issue_monitor_recovery",
        "taskWatchdog": {
            "watchedIssueId": "iss_watch_42",
            "watchedIssueIdentifier": "PC-WATCH-42",
            "watchedIssueTitle": "Watched subtree",
            "stopFingerprint": "stop-fp-xyz",
            "capabilities": {
                "operations": ["comment", "reopen"],
                "deniedOperations": ["merge"],
                "targetScope": {
                    "watchedIssueId": "iss_watch_42",
                    "watchedIssueIdentifier": "PC-WATCH-42",
                    "watchdogIssueId": "iss_wd_99",
                    "includeNonWatchdogDescendants": true,
                    "excludedOriginKinds": ["test"],
                },
            },
            "terminalLeafSummaries": [
                {
                    "id": "leaf_1",
                    "identifier": "PC-LEAF-1",
                    "title": "Stopped leaf",
                    "status": "blocked",
                    "role": "executor",
                    "summary": "Waiting on auth",
                }
            ],
        }
    });
    let rendered =
        render_paperclip_wake_prompt(Some(&payload), &RenderWakePromptOptions::default());
    assert!(rendered.contains("## Task Watchdog Mandate"));
    assert!(rendered.contains("Watched issue: PC-WATCH-42 Watched subtree"));
    assert!(rendered.contains("Stop fingerprint: stop-fp-xyz"));
    assert!(rendered.contains("You are running as a task watchdog"));
    assert!(rendered.contains("Safety constraints (these always apply"));
    assert!(rendered.contains("- Allowed operations: comment, reopen."));
    assert!(rendered.contains("- Denied operations: merge."));
    assert!(rendered.contains("- Target scope: PC-WATCH-42 plus non-watchdog descendants."));
    assert!(rendered.contains("- Reusable watchdog issue: iss_wd_99."));
    assert!(rendered.contains("- Excluded origin kinds: test."));
    assert!(rendered.contains("Terminal / stopped leaves to verify:"));
    assert!(rendered.contains("- PC-LEAF-1 Stopped leaf (blocked) [executor]"));
    assert!(rendered.contains("  Waiting on auth"));
    assert!(rendered.contains("No board-supplied watchdog instructions."));
}

// =============================================================================
// Annotation deltas
// =============================================================================

#[test]
fn render_annotation_deltas_lists_thread_context_and_body() {
    let payload = json!({
        "reason": "issue_commented",
        "annotationDeltas": [
            {
                "id": "annot_1",
                "threadId": "thread_42",
                "documentKey": "doc-key-1",
                "revisionNumber": 3,
                "threadStatus": "open",
                "anchorState": "anchored",
                "anchorConfidence": "high",
                "quote": "ship the R382 stub completion",
                "prefix": "before context",
                "suffix": "after context",
                "body": "Please also include the liveness section.",
                "bodyTruncated": true,
                "createdAt": "2026-08-07T11:00:00Z",
                "author": { "type": "user", "id": "u_42" },
            }
        ]
    });
    let rendered =
        render_paperclip_wake_prompt(Some(&payload), &RenderWakePromptOptions::default());
    assert!(rendered.contains("New plan annotation deltas:"));
    assert!(rendered.contains("- annotation annot_1 (open, revision #3, anchored, high)"));
    assert!(rendered.contains("  thread: thread_42"));
    assert!(rendered.contains("  document: doc-key-1"));
    assert!(rendered.contains("  selected text: ship the R382 stub completion"));
    assert!(rendered.contains("  context before: before context"));
    assert!(rendered.contains("  context after: after context"));
    assert!(rendered.contains("  comment by user u_42 at 2026-08-07T11:00:00Z:"));
    assert!(rendered.contains("Please also include the liveness section."));
    assert!(rendered.contains("[annotation comment body truncated]"));
}

// =============================================================================
// Plan review context
// =============================================================================

#[test]
fn render_plan_review_context_includes_threads_and_interaction() {
    let payload = json!({
        "reason": "issue_commented",
        "planReviewContext": {
            "documentKey": "doc-key-7",
            "issueId": "iss_pr_1",
            "latestRevisionId": "rev_5",
            "latestRevisionNumber": 5,
            "interaction": {
                "kind": "request_confirmation",
                "status": "pending",
                "target": {
                    "key": "doc-key-7",
                    "revisionNumber": 5,
                },
            },
            "threads": [
                {
                    "id": "thread_pr_1",
                    "status": "open",
                    "revisionNumber": 5,
                    "anchorState": "anchored",
                    "selectedText": "the liveness section",
                    "comments": [
                        {
                            "id": "c1",
                            "body": "Please add the liveness section.",
                            "author": { "type": "user", "id": "u_5" },
                            "createdAt": "2026-08-07T12:00:00Z",
                        }
                    ],
                }
            ],
        }
    });
    let rendered =
        render_paperclip_wake_prompt(Some(&payload), &RenderWakePromptOptions::default());
    assert!(rendered.contains("Open plan comments to incorporate:"));
    assert!(rendered.contains("- latest plan revision: 5 (rev_5)"));
    assert!(rendered.contains("- interaction: request_confirmation pending"));
    assert!(rendered.contains("- target: doc-key-7 revision #5"));
    assert!(rendered.contains("- open annotation threads included: 1/1"));
    assert!(rendered.contains("- thread thread_pr_1 (open, revision #5, anchored)"));
    assert!(rendered.contains("  selected text: the liveness section"));
    assert!(rendered.contains("  comment c1 by user u_5 at 2026-08-07T12:00:00Z:"));
    assert!(rendered.contains("Please add the liveness section."));
}

// =============================================================================
// Typed normalize integration
// =============================================================================

#[test]
fn normalize_replaces_all_five_value_placeholders_with_typed_structs() {
    let payload = json!({
        "taskWatchdog": { "watchedIssueId": "iss_w_1" },
        "annotationDeltas": [
            { "id": "annot_1", "body": "body", "author": { "type": "user", "id": "u_1" } }
        ],
        "continuationSummary": { "body": "summary body" },
        "livenessContinuation": { "attempt": 1, "instruction": "go" },
        "planReviewContext": {
            "documentKey": "doc-1",
            "threads": [{ "id": "t_1", "selectedText": "sel" }]
        },
    });
    let normalized = normalize_paperclip_wake_payload(Some(&payload)).expect("normalized");
    assert!(normalized.task_watchdog.is_some());
    assert_eq!(normalized.annotation_deltas.len(), 1);
    assert!(normalized.continuation_summary.is_some());
    assert!(normalized.liveness_continuation.is_some());
    assert!(normalized.plan_review_context.is_some());
}

#[test]
fn all_five_stubs_coexist_in_a_single_payload() {
    let payload = json!({
        "reason": "issue_commented",
        "taskWatchdog": { "watchedIssueId": "iss_w_1", "watchedIssueIdentifier": "PC-W-1" },
        "annotationDeltas": [{ "id": "a1", "body": "ab", "author": { "type": "user", "id": "u" } }],
        "continuationSummary": { "body": "summary body" },
        "livenessContinuation": { "attempt": 2, "instruction": "go" },
        "planReviewContext": { "documentKey": "doc-1", "threads": [] },
    });
    let rendered =
        render_paperclip_wake_prompt(Some(&payload), &RenderWakePromptOptions::default());
    // All 5 stub sections must appear in the rendered prompt body.
    assert!(rendered.contains("## Task Watchdog Mandate"));
    assert!(rendered.contains("New plan annotation deltas:"));
    assert!(rendered.contains("Issue continuation summary:"));
    assert!(rendered.contains("Run liveness continuation:"));
    assert!(rendered.contains("Open plan comments to incorporate:"));
}
