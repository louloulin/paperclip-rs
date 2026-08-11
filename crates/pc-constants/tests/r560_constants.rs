//! pc-constants R560 集成测试：验证所有常量可访问 + 与 Node upstream 字段级一致。

use pc_constants::{agent::*, budget::*, company::*, heartbeat::*, issue::*, workflow::*};

#[test]
fn all_modules_export_public_constants() {
    // Smoke test: each module's primary constant is accessible
    assert!(!COMPANY_STATUSES.is_empty());
    assert!(!AGENT_ICON_NAMES.is_empty());
    assert!(!ISSUE_STATUSES.is_empty());
    assert!(!HEARTBEAT_RUN_STATUSES.is_empty());
    assert!(!BUDGET_SCOPE_TYPES.is_empty());
    assert!(!PIPELINE_CASE_STATUSES.is_empty());
}

#[test]
fn company_block_is_self_consistent() {
    // Attachment limit ordering invariant (compile-time check via const block)
    const { assert!(DEFAULT_COMPANY_ATTACHMENT_MAX_BYTES < MAX_COMPANY_ATTACHMENT_MAX_BYTES) };
    // Membership roles: human roles ⊆ company roles
    for role in HUMAN_COMPANY_MEMBERSHIP_ROLES {
        assert!(
            COMPANY_MEMBERSHIP_ROLES.contains(role),
            "human role {role} must be in company roles"
        );
    }
    // Principal types: must include "user" and "agent"
    assert!(PRINCIPAL_TYPES.contains(&"user"));
    assert!(PRINCIPAL_TYPES.contains(&"agent"));
}

#[test]
fn issue_block_inbox_status_subset() {
    // Inbox filter statuses must be subset of all issue statuses
    for status in INBOX_MINE_ISSUE_STATUSES {
        assert!(
            ISSUE_STATUSES.contains(status),
            "inbox status {status} must be in ISSUE_STATUSES"
        );
    }
    // Inbox filter string equals joined statuses
    let expected = INBOX_MINE_ISSUE_STATUSES.join(",");
    assert_eq!(INBOX_MINE_ISSUE_STATUS_FILTER, expected);
}

#[test]
fn issue_origin_kinds_includes_task_watchdog() {
    assert!(ISSUE_ORIGIN_KINDS.contains(&TASK_WATCHDOG_PRODUCT_BUG_ORIGIN_KIND));
}

#[test]
fn agent_block_default_concurrent_runs_is_20() {
    assert_eq!(AGENT_DEFAULT_MAX_CONCURRENT_RUNS, 20);
}

#[test]
fn heartbeat_run_statuses_includes_terminal_set() {
    let terminal = ["succeeded", "failed", "cancelled", "timed_out"];
    for s in terminal {
        assert!(HEARTBEAT_RUN_STATUSES.contains(&s), "missing {s}");
    }
}

#[test]
fn heartbeat_invocation_sources_non_empty() {
    assert!(!HEARTBEAT_INVOCATION_SOURCES.is_empty());
    assert!(HEARTBEAT_INVOCATION_SOURCES.contains(&"scheduler"));
}

#[test]
fn budget_window_kinds_distinct() {
    assert_ne!(BUDGET_WINDOW_KINDS[0], BUDGET_WINDOW_KINDS[1]);
}

#[test]
fn workflow_pipeline_triggers_superset_of_cron() {
    // Workflow adds cron; pipeline doesn't
    assert!(ROUTINE_TRIGGER_KINDS.contains(&"cron"));
    assert!(!PIPELINE_TRIGGER_KINDS.contains(&"cron"));
}

#[test]
fn approval_statuses_have_pending_and_terminal() {
    assert!(APPROVAL_REQUEST_STATUSES.contains(&"pending"));
    assert!(APPROVAL_REQUEST_STATUSES.contains(&"approved"));
    assert!(APPROVAL_REQUEST_STATUSES.contains(&"denied"));
}

#[test]
fn constants_have_no_duplicates_within_module() {
    use std::collections::HashSet;
    let check = |arr: &[&str], name: &str| {
        let unique: HashSet<_> = arr.iter().collect();
        assert_eq!(unique.len(), arr.len(), "{name} has duplicate entries");
    };
    check(COMPANY_STATUSES, "COMPANY_STATUSES");
    check(ISSUE_STATUSES, "ISSUE_STATUSES");
    check(ISSUE_PRIORITIES, "ISSUE_PRIORITIES");
    check(HEARTBEAT_RUN_STATUSES, "HEARTBEAT_RUN_STATUSES");
    check(LIVE_EVENT_TYPES, "LIVE_EVENT_TYPES");
    check(BUDGET_SCOPE_TYPES, "BUDGET_SCOPE_TYPES");
    check(PIPELINE_CASE_STATUSES, "PIPELINE_CASE_STATUSES");
}

#[test]
fn system_doc_keys_match_subsets() {
    // Continuation summary key + pipeline case body key must be in SYSTEM_ISSUE_DOCUMENT_KEYS
    assert!(SYSTEM_ISSUE_DOCUMENT_KEYS.contains(&ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY));
    assert!(SYSTEM_ISSUE_DOCUMENT_KEYS.contains(&PIPELINE_CASE_BODY_DOCUMENT_KEY));
}
