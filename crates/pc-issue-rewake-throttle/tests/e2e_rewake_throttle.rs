//! End-to-end tests for `pc-issue-rewake-throttle`.
//!
//! 包含：
//! - 纯函数 service 测试（与 Node `issue-rewake-throttle.ts` 1:1 对齐）
//! - Hook 测试
//! - cooldown 计算边界测试

use chrono::{Duration, TimeZone, Utc};
use pc_issue_rewake_throttle::{
    compute_issue_rewake_cooldown_ms, evaluate_issue_rewake_throttle,
    is_throttle_candidate_issue_rewake, IssueRewakeCandidateInput,
    IssueRewakeThrottleDecision, IssueRewakeThrottleHookEvent, IssueRewakeThrottleInput,
    IssueRewakeThrottleService, RecentIssueRunSample, RecordingIssueRewakeThrottleHook,
    ISSUE_NEW_INPUT_ACTIVITY_ACTIONS, ISSUE_PROGRESS_ACTIVITY_ACTIONS,
    ISSUE_REWAKE_BASE_COOLDOWN_MS, ISSUE_REWAKE_LOOKBACK_MS, ISSUE_REWAKE_MAX_COOLDOWN_MS,
    ISSUE_REWAKE_NO_PROGRESS_THRESHOLD, ISSUE_REWAKE_RUN_SAMPLE_LIMIT,
    THROTTLED_ISSUE_REWAKE_REASONS,
};
use std::collections::HashSet;
use std::sync::Arc;

fn dt(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, min, sec).unwrap()
}

fn make_run(id: &str, status: &str, finished_at: Option<chrono::DateTime<Utc>>) -> RecentIssueRunSample {
    RecentIssueRunSample {
        id: id.to_string(),
        status: status.to_string(),
        finished_at,
    }
}

// ============================================================================
// 常量测试（与 Node 1:1 对齐）
// ============================================================================

#[test]
fn r664_constants_match_node() {
    assert_eq!(ISSUE_REWAKE_NO_PROGRESS_THRESHOLD, 2);
    assert_eq!(ISSUE_REWAKE_BASE_COOLDOWN_MS, 120_000);
    assert_eq!(ISSUE_REWAKE_MAX_COOLDOWN_MS, 30 * 60_000);
    assert_eq!(ISSUE_REWAKE_LOOKBACK_MS, 6 * 60 * 60_000);
    assert_eq!(ISSUE_REWAKE_RUN_SAMPLE_LIMIT, 8);
}

#[test]
fn r664_throttled_reasons_set() {
    assert!(THROTTLED_ISSUE_REWAKE_REASONS.contains(&"issue_assigned"));
    assert!(THROTTLED_ISSUE_REWAKE_REASONS.contains(&"issue_continuation_needed"));
    assert!(THROTTLED_ISSUE_REWAKE_REASONS.contains(&"issue_assignment_recovery"));
    assert!(THROTTLED_ISSUE_REWAKE_REASONS.contains(&"issue_graph_liveness_backstop"));
    assert_eq!(THROTTLED_ISSUE_REWAKE_REASONS.len(), 4);
}

#[test]
fn r664_progress_actions_includes_core_actions() {
    assert!(ISSUE_PROGRESS_ACTIVITY_ACTIONS.contains(&"issue.updated"));
    assert!(ISSUE_PROGRESS_ACTIVITY_ACTIONS.contains(&"issue.comment_added"));
    assert!(ISSUE_PROGRESS_ACTIVITY_ACTIONS.contains(&"issue.document_upserted"));
}

#[test]
fn r664_new_input_actions_includes_progress_actions() {
    for action in ISSUE_PROGRESS_ACTIVITY_ACTIONS {
        assert!(
            ISSUE_NEW_INPUT_ACTIVITY_ACTIONS.contains(action),
            "new input should include progress action: {}",
            action
        );
    }
    // new input also includes extras
    assert!(ISSUE_NEW_INPUT_ACTIVITY_ACTIONS.contains(&"issue.thread_interaction_accepted"));
    assert!(ISSUE_NEW_INPUT_ACTIVITY_ACTIONS.contains(&"issue.blockers_resolved_wake_emitted"));
}

// ============================================================================
// is_throttle_candidate 测试
// ============================================================================

#[test]
fn r664_candidate_force_fresh_session_returns_false() {
    let input = IssueRewakeCandidateInput {
        reason: Some("issue_assigned".to_string()),
        wake_comment_id: None,
        force_fresh_session: true,
        has_explicit_resume: false,
    };
    assert!(!is_throttle_candidate_issue_rewake(&input));
}

#[test]
fn r664_candidate_with_wake_comment_returns_false() {
    let input = IssueRewakeCandidateInput {
        reason: Some("issue_assigned".to_string()),
        wake_comment_id: Some("c-1".to_string()),
        force_fresh_session: false,
        has_explicit_resume: false,
    };
    assert!(!is_throttle_candidate_issue_rewake(&input));
}

#[test]
fn r664_candidate_with_explicit_resume_returns_false() {
    let input = IssueRewakeCandidateInput {
        reason: Some("issue_assigned".to_string()),
        wake_comment_id: None,
        force_fresh_session: false,
        has_explicit_resume: true,
    };
    assert!(!is_throttle_candidate_issue_rewake(&input));
}

#[test]
fn r664_candidate_no_reason_returns_true() {
    let input = IssueRewakeCandidateInput {
        reason: None,
        wake_comment_id: None,
        force_fresh_session: false,
        has_explicit_resume: false,
    };
    assert!(is_throttle_candidate_issue_rewake(&input));
}

#[test]
fn r664_candidate_throttled_reason_returns_true() {
    for reason in THROTTLED_ISSUE_REWAKE_REASONS {
        let input = IssueRewakeCandidateInput {
            reason: Some(reason.to_string()),
            wake_comment_id: None,
            force_fresh_session: false,
            has_explicit_resume: false,
        };
        assert!(
            is_throttle_candidate_issue_rewake(&input),
            "reason {} should be a candidate",
            reason
        );
    }
}

#[test]
fn r664_candidate_event_reason_returns_false() {
    let input = IssueRewakeCandidateInput {
        reason: Some("comment_added".to_string()),
        wake_comment_id: None,
        force_fresh_session: false,
        has_explicit_resume: false,
    };
    assert!(!is_throttle_candidate_issue_rewake(&input));
}

// ============================================================================
// compute_cooldown_ms 测试
// ============================================================================

#[test]
fn r664_cooldown_below_threshold_returns_base() {
    // streak=0 or 1 (below threshold=2) → not directly used,
    // but compute_cooldown itself always returns at least base for streak=0
    let cd = compute_issue_rewake_cooldown_ms(0);
    assert_eq!(cd, ISSUE_REWAKE_BASE_COOLDOWN_MS);

    let cd = compute_issue_rewake_cooldown_ms(1);
    assert_eq!(cd, ISSUE_REWAKE_BASE_COOLDOWN_MS);

    let cd = compute_issue_rewake_cooldown_ms(2);
    assert_eq!(cd, ISSUE_REWAKE_BASE_COOLDOWN_MS);
}

#[test]
fn r664_cooldown_doubles_per_streak() {
    // streak=3 → 1 doubling → 240_000
    let cd = compute_issue_rewake_cooldown_ms(3);
    assert_eq!(cd, ISSUE_REWAKE_BASE_COOLDOWN_MS * 2);

    // streak=4 → 2 doublings → 480_000
    let cd = compute_issue_rewake_cooldown_ms(4);
    assert_eq!(cd, ISSUE_REWAKE_BASE_COOLDOWN_MS * 4);
}

#[test]
fn r664_cooldown_caps_at_max() {
    // streak large enough to exceed max
    let cd = compute_issue_rewake_cooldown_ms(20);
    assert_eq!(cd, ISSUE_REWAKE_MAX_COOLDOWN_MS);
}

#[test]
fn r664_cooldown_does_not_overflow() {
    // Even with absurd streak
    let cd = compute_issue_rewake_cooldown_ms(usize::MAX);
    assert_eq!(cd, ISSUE_REWAKE_MAX_COOLDOWN_MS);
}

// ============================================================================
// evaluate_issue_rewake_throttle 主决策
// ============================================================================

#[test]
fn r664_evaluate_empty_runs_returns_allowed() {
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 12, 0, 0),
        recent_terminal_runs: vec![],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0
        }
    );
}

#[test]
fn r664_evaluate_new_input_returns_allowed() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30),
        recent_terminal_runs: vec![
            make_run("r1", "succeeded", Some(last_finish)),
            make_run("r2", "succeeded", Some(last_finish - Duration::minutes(5))),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: true, // new input → bypass
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0
        }
    );
}

#[test]
fn r664_evaluate_streak_below_threshold_returns_allowed() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30),
        recent_terminal_runs: vec![
            make_run("r1", "succeeded", Some(last_finish)),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 1
        }
    );
}

#[test]
fn r664_evaluate_streak_at_threshold_in_cooldown_returns_blocked() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30), // 30s after last finish
        recent_terminal_runs: vec![
            make_run("r1", "succeeded", Some(last_finish)),
            make_run("r2", "succeeded", Some(last_finish - Duration::minutes(5))),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    match decision {
        IssueRewakeThrottleDecision::Blocked {
            no_progress_streak,
            cooldown_ms,
            ..
        } => {
            assert_eq!(no_progress_streak, 2);
            assert_eq!(cooldown_ms, ISSUE_REWAKE_BASE_COOLDOWN_MS); // 120s
        }
        _ => panic!("expected Blocked, got {:?}", decision),
    }
}

#[test]
fn r664_evaluate_streak_at_threshold_after_cooldown_returns_allowed() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 5, 0), // 5 minutes after last finish (well past 120s)
        recent_terminal_runs: vec![
            make_run("r1", "succeeded", Some(last_finish)),
            make_run("r2", "succeeded", Some(last_finish - Duration::minutes(5))),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 2
        }
    );
}

#[test]
fn r664_evaluate_streak_with_progress_run_breaks() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let mut progress: HashSet<String> = HashSet::new();
    progress.insert("r1".to_string());
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30),
        recent_terminal_runs: vec![
            make_run("r1", "succeeded", Some(last_finish)),
            make_run("r2", "succeeded", Some(last_finish - Duration::minutes(5))),
        ],
        run_ids_with_issue_progress: progress,
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    // r1 has progress → streak = 0
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0
        }
    );
}

#[test]
fn r664_evaluate_failed_run_breaks_streak() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30),
        recent_terminal_runs: vec![
            make_run("r1", "failed", Some(last_finish)), // failed → break
            make_run("r2", "succeeded", Some(last_finish - Duration::minutes(5))),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    // failed run → streak = 0 (recovery, not throttle)
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0
        }
    );
}

#[test]
fn r664_evaluate_cancelled_run_breaks_streak() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30),
        recent_terminal_runs: vec![
            make_run("r1", "cancelled", Some(last_finish)),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0
        }
    );
}

#[test]
fn r664_evaluate_long_streak_uses_escalating_cooldown() {
    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30),
        recent_terminal_runs: vec![
            make_run("r1", "succeeded", Some(last_finish)),
            make_run("r2", "succeeded", Some(last_finish - Duration::minutes(5))),
            make_run("r3", "succeeded", Some(last_finish - Duration::minutes(10))),
            make_run("r4", "succeeded", Some(last_finish - Duration::minutes(15))),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = evaluate_issue_rewake_throttle(&input);
    match decision {
        IssueRewakeThrottleDecision::Blocked {
            no_progress_streak,
            cooldown_ms,
            ..
        } => {
            assert_eq!(no_progress_streak, 4);
            // streak=4 → 2 doublings → 4 * 120s = 480s
            assert_eq!(cooldown_ms, ISSUE_REWAKE_BASE_COOLDOWN_MS * 4);
        }
        _ => panic!("expected Blocked, got {:?}", decision),
    }
}

// ============================================================================
// Service + Hook 测试
// ============================================================================

#[test]
fn r664_service_is_candidate_with_hook() {
    let hook = Arc::new(RecordingIssueRewakeThrottleHook::new());
    let svc = IssueRewakeThrottleService::with_hook(hook.clone());

    let candidate = IssueRewakeCandidateInput {
        reason: Some("issue_assigned".to_string()),
        wake_comment_id: None,
        force_fresh_session: false,
        has_explicit_resume: false,
    };
    let result = svc.is_candidate(&candidate);
    assert!(result);

    let events = hook.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        IssueRewakeThrottleHookEvent::BeforeEvaluate { .. }
    ));
}

#[test]
fn r664_service_not_candidate_triggers_on_not_candidate() {
    let hook = Arc::new(RecordingIssueRewakeThrottleHook::new());
    let svc = IssueRewakeThrottleService::with_hook(hook.clone());

    let candidate = IssueRewakeCandidateInput {
        reason: Some("issue_assigned".to_string()),
        wake_comment_id: None,
        force_fresh_session: true, // bypass
        has_explicit_resume: false,
    };
    let result = svc.is_candidate(&candidate);
    assert!(!result);

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1],
        IssueRewakeThrottleHookEvent::OnNotCandidate { .. }
    ));
}

#[test]
fn r664_service_evaluate_allowed_triggers_after_allowed() {
    let hook = Arc::new(RecordingIssueRewakeThrottleHook::new());
    let svc = IssueRewakeThrottleService::with_hook(hook.clone());

    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 12, 0, 0),
        recent_terminal_runs: vec![],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = svc.evaluate(&input);
    assert_eq!(
        decision,
        IssueRewakeThrottleDecision::Allowed {
            no_progress_streak: 0
        }
    );

    let events = hook.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        IssueRewakeThrottleHookEvent::AfterAllowed { .. }
    ));
}

#[test]
fn r664_service_evaluate_blocked_triggers_after_blocked() {
    let hook = Arc::new(RecordingIssueRewakeThrottleHook::new());
    let svc = IssueRewakeThrottleService::with_hook(hook.clone());

    let last_finish = dt(2025, 1, 1, 11, 0, 0);
    let input = IssueRewakeThrottleInput {
        now: dt(2025, 1, 1, 11, 0, 30),
        recent_terminal_runs: vec![
            make_run("r1", "succeeded", Some(last_finish)),
            make_run("r2", "succeeded", Some(last_finish - Duration::minutes(5))),
        ],
        run_ids_with_issue_progress: HashSet::new(),
        has_new_issue_input_since_last_run: false,
    };
    let decision = svc.evaluate(&input);
    assert!(matches!(
        decision,
        IssueRewakeThrottleDecision::Blocked { .. }
    ));

    let events = hook.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        IssueRewakeThrottleHookEvent::AfterBlocked { .. }
    ));
}

#[test]
fn r664_hook_clear() {
    let hook = Arc::new(RecordingIssueRewakeThrottleHook::new());
    let svc = IssueRewakeThrottleService::with_hook(hook.clone());

    let _ = svc.is_candidate(&IssueRewakeCandidateInput::default());
    assert_eq!(hook.len(), 1);
    hook.clear();
    assert!(hook.is_empty());
}

#[test]
fn r664_default_service_uses_noop_hook() {
    let svc = IssueRewakeThrottleService::new();
    let hook = svc.hook();
    hook.before_evaluate(&IssueRewakeCandidateInput::default());
    hook.on_not_candidate(&None);
    hook.after_allowed(0);
    hook.after_blocked(0, 1000);
}
