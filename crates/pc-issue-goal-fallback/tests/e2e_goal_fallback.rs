//! End-to-end tests for `pc-issue-goal-fallback`.
//!
//! 包含：
//! - 纯函数 service 测试（与 Node `issue-goal-fallback.ts` 1:1 对齐）
//! - Hook 测试：BeforeResolve / AfterResolve / OnNull 触发

use pc_issue_goal_fallback::{
    resolve_issue_goal_id, resolve_next_issue_goal_id,
    IssueGoalFallbackHookEvent, IssueGoalFallbackService, MaybeId,
    RecordingIssueGoalFallbackHook, ResolveIssueGoalIdInput, ResolveNextIssueGoalIdInput,
};
use std::sync::Arc;

fn s(v: &str) -> MaybeId {
    Some(v.to_string())
}

// ============================================================================
// resolve_issue_goal_id —— 单点解析（与 Node 1:1 对齐）
// ============================================================================

#[test]
fn r661_resolve_returns_explicit_goal_id() {
    let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
        project_id: s("p1"),
        goal_id: s("g-explicit"),
        project_goal_id: s("pg1"),
        default_goal_id: s("d1"),
    });
    assert_eq!(out, Some("g-explicit".to_string()));
}

#[test]
fn r661_resolve_uses_project_goal_when_no_goal_id() {
    let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
        project_id: s("p1"),
        goal_id: None,
        project_goal_id: s("pg1"),
        default_goal_id: s("d1"),
    });
    assert_eq!(out, Some("pg1".to_string()));
}

#[test]
fn r661_resolve_returns_null_project_goal_when_project_no_goal() {
    let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
        project_id: s("p1"),
        goal_id: None,
        project_goal_id: None,
        default_goal_id: s("d1"),
    });
    assert_eq!(out, None);
}

#[test]
fn r661_resolve_uses_default_when_no_project() {
    let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
        project_id: None,
        goal_id: None,
        project_goal_id: s("pg1"),
        default_goal_id: s("d1"),
    });
    assert_eq!(out, Some("d1".to_string()));
}

#[test]
fn r661_resolve_returns_none_when_nothing() {
    let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
        project_id: None,
        goal_id: None,
        project_goal_id: None,
        default_goal_id: None,
    });
    assert_eq!(out, None);
}

#[test]
fn r661_resolve_goal_id_beats_project_and_default() {
    let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
        project_id: None,
        goal_id: s("g-explicit"),
        project_goal_id: None,
        default_goal_id: None,
    });
    assert_eq!(out, Some("g-explicit".to_string()));
}

// ============================================================================
// resolve_next_issue_goal_id —— 状态迁移解析（与 Node 1:1 对齐）
// ============================================================================

#[test]
fn r661_resolve_next_explicit_goal_id_wins() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: s("cp"),
        current_goal_id: s("cg"),
        current_project_goal_id: s("cpg"),
        project_id: Some("p".into()),
        goal_id: Some(Some("g".into())),
        project_goal_id: Some("pg".into()),
        default_goal_id: s("d"),
    });
    assert_eq!(out, Some("g".to_string()));
}

#[test]
fn r661_resolve_next_explicit_null_goal_id_falls_back() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: s("cp"),
        current_goal_id: s("cg"),
        current_project_goal_id: s("cpg"),
        project_id: Some("p".into()),
        goal_id: Some(None),
        project_goal_id: Some("pg".into()),
        default_goal_id: s("d"),
    });
    assert_eq!(out, Some("pg".to_string()));
}

#[test]
fn r661_resolve_next_no_current_goal_returns_next_fallback() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: s("cp"),
        current_goal_id: None,
        current_project_goal_id: s("cpg"),
        project_id: Some("p".into()),
        goal_id: None,
        project_goal_id: Some("pg".into()),
        default_goal_id: s("d"),
    });
    assert_eq!(out, Some("pg".to_string()));
}

#[test]
fn r661_resolve_next_current_equals_fallback_returns_next_fallback() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: s("cp"),
        current_goal_id: s("cpg"),
        current_project_goal_id: s("cpg"),
        project_id: Some("p".into()),
        goal_id: None,
        project_goal_id: Some("pg-new".into()),
        default_goal_id: s("d"),
    });
    assert_eq!(out, Some("pg-new".to_string()));
}

#[test]
fn r661_resolve_next_current_differs_from_fallback_keeps_current() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: s("cp"),
        current_goal_id: s("user-pinned"),
        current_project_goal_id: s("cpg"),
        project_id: Some("p".into()),
        goal_id: None,
        project_goal_id: Some("pg-new".into()),
        default_goal_id: s("d"),
    });
    assert_eq!(out, Some("user-pinned".to_string()));
}

#[test]
fn r661_resolve_next_project_id_omitted_falls_back_to_current() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: s("cp"),
        current_goal_id: None,
        current_project_goal_id: s("cpg"),
        project_id: None,
        goal_id: None,
        project_goal_id: None,
        default_goal_id: s("d"),
    });
    assert_eq!(out, Some("cpg".to_string()));
}

#[test]
fn r661_resolve_next_no_project_uses_default_goal_id() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: None,
        current_goal_id: None,
        current_project_goal_id: None,
        project_id: None,
        goal_id: None,
        project_goal_id: None,
        default_goal_id: s("d"),
    });
    assert_eq!(out, Some("d".to_string()));
}

#[test]
fn r661_resolve_next_yields_null_when_all_unresolved() {
    let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
        current_project_id: None,
        current_goal_id: None,
        current_project_goal_id: Some("cpg".into()),
        project_id: None,
        goal_id: None,
        project_goal_id: None,
        default_goal_id: None,
    });
    assert_eq!(out, None);
}

// ============================================================================
// Hook 测试
// ============================================================================

#[test]
fn r661_hook_before_and_after_resolve() {
    let hook = Arc::new(RecordingIssueGoalFallbackHook::new());
    let svc = IssueGoalFallbackService::with_hook(hook.clone());

    let result = svc.resolve(ResolveIssueGoalIdInput {
        project_id: s("p1"),
        goal_id: s("g-explicit"),
        project_goal_id: None,
        default_goal_id: None,
    });
    assert_eq!(result, Some("g-explicit".to_string()));

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        IssueGoalFallbackHookEvent::BeforeResolve { .. }
    ));
    assert!(matches!(
        events[1],
        IssueGoalFallbackHookEvent::AfterResolve { .. }
    ));
}

#[test]
fn r661_hook_on_null_single() {
    let hook = Arc::new(RecordingIssueGoalFallbackHook::new());
    let svc = IssueGoalFallbackService::with_hook(hook.clone());

    let result = svc.resolve(ResolveIssueGoalIdInput {
        project_id: None,
        goal_id: None,
        project_goal_id: None,
        default_goal_id: None,
    });
    assert_eq!(result, None);

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1],
        IssueGoalFallbackHookEvent::OnNullSingle
    ));
}

#[test]
fn r661_hook_before_and_after_resolve_next() {
    let hook = Arc::new(RecordingIssueGoalFallbackHook::new());
    let svc = IssueGoalFallbackService::with_hook(hook.clone());

    let result = svc.resolve_next(ResolveNextIssueGoalIdInput {
        current_project_id: None,
        current_goal_id: None,
        current_project_goal_id: None,
        project_id: None,
        goal_id: None,
        project_goal_id: None,
        default_goal_id: s("d"),
    });
    assert_eq!(result, Some("d".to_string()));

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        IssueGoalFallbackHookEvent::BeforeResolveNext { .. }
    ));
    assert!(matches!(
        events[1],
        IssueGoalFallbackHookEvent::AfterResolveNext { .. }
    ));
}

#[test]
fn r661_hook_on_null_next() {
    let hook = Arc::new(RecordingIssueGoalFallbackHook::new());
    let svc = IssueGoalFallbackService::with_hook(hook.clone());

    let result = svc.resolve_next(ResolveNextIssueGoalIdInput {
        current_project_id: None,
        current_goal_id: None,
        current_project_goal_id: None,
        project_id: None,
        goal_id: None,
        project_goal_id: None,
        default_goal_id: None,
    });
    assert_eq!(result, None);

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(events[1], IssueGoalFallbackHookEvent::OnNullNext));
}

#[test]
fn r661_hook_clear() {
    let hook = Arc::new(RecordingIssueGoalFallbackHook::new());
    let svc = IssueGoalFallbackService::with_hook(hook.clone());

    svc.resolve(ResolveIssueGoalIdInput {
        project_id: None,
        goal_id: s("g"),
        project_goal_id: None,
        default_goal_id: None,
    });
    assert_eq!(hook.len(), 2);
    hook.clear();
    assert!(hook.is_empty());
}

#[test]
fn r661_default_service_uses_noop_hook() {
    let svc = IssueGoalFallbackService::new();
    let hook = svc.hook();
    // Just exercise — no panic = pass
    hook.before_resolve(&ResolveIssueGoalIdInput::default());
    hook.after_resolve("g");
    hook.on_null_single();
    hook.before_resolve_next(&ResolveNextIssueGoalIdInput::default());
    hook.after_resolve_next("g");
    hook.on_null_next();
}
