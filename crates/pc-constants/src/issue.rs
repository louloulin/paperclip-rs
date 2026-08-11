//! Issue 域常量。
//!
//! 提供 issue 状态 / 优先级 / work mode / origin kind 等常用枚举常量。
//! 各域 crate 也可以 `use pc_constants::issue::*` 复用。

/// Issue 状态。
pub const ISSUE_STATUSES: &[&str] = &[
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "done",
    "cancelled",
    "blocked",
];

/// Inbox 中"我的"issue 状态过滤集合。
pub const INBOX_MINE_ISSUE_STATUSES: &[&str] = &["todo", "in_progress", "in_review", "blocked"];

/// Inbox 过滤字符串（逗号拼接，镜像 Node `INBOX_MINE_ISSUE_STATUS_FILTER`）。
pub const INBOX_MINE_ISSUE_STATUS_FILTER: &str = "todo,in_progress,in_review,blocked";

/// Issue 优先级。
pub const ISSUE_PRIORITIES: &[&str] = &["critical", "high", "medium", "low"];

/// Issue work mode（standard / ask / planning / skill_test）。
pub const ISSUE_WORK_MODES: &[&str] = &["standard", "ask", "planning", "skill_test"];

/// Issue harness kind（用于 skill_test 模式识别）。
pub const ISSUE_HARNESS_KINDS: &[&str] = &["skill_test"];

/// Issue request depth 上限（防止无限嵌套）。
pub const MAX_ISSUE_REQUEST_DEPTH: u32 = 1024;

/// Summary slot scope kind。
pub const SUMMARY_SLOT_SCOPE_KINDS: &[&str] =
    &["project", "workspaces_overview", "project_workspace"];

/// Summary slot key。
pub const SUMMARY_SLOT_KEYS: &[&str] = &["header"];

/// Summary slot 状态。
pub const SUMMARY_SLOT_STATUSES: &[&str] = &["idle", "generating", "failed"];

/// Issue comment 作者类型。
pub const ISSUE_COMMENT_AUTHOR_TYPES: &[&str] = &["user", "agent", "system"];

/// Issue comment presentation kind。
pub const ISSUE_COMMENT_PRESENTATION_KINDS: &[&str] = &["message", "system_notice"];

/// Issue comment presentation tone。
pub const ISSUE_COMMENT_PRESENTATION_TONES: &[&str] =
    &["neutral", "info", "success", "warning", "danger"];

/// Issue comment presentation density。
pub const ISSUE_COMMENT_PRESENTATION_DENSITIES: &[&str] = &["compact"];

/// Issue 关系类型（目前只 blocks；后续可扩展）。
pub const ISSUE_RELATION_TYPES: &[&str] = &["blocks"];

/// Issue tree control mode。
pub const ISSUE_TREE_CONTROL_MODES: &[&str] = &["pause", "resume", "cancel", "restore"];

/// Issue tree hold 状态。
pub const ISSUE_TREE_HOLD_STATUSES: &[&str] = &["active", "released"];

/// Issue tree hold release policy strategy。
pub const ISSUE_TREE_HOLD_RELEASE_POLICY_STRATEGIES: &[&str] =
    &["manual", "after_active_runs_finish"];

/// Issue origin kind（包含 system / harness / human / agent）。
pub const ISSUE_ORIGIN_KINDS: &[&str] = &[
    "system",
    "user",
    "agent",
    "harness",
    "harness_liveness",
    "harness_liveness_escalation",
    "watchdog",
    "task_watchdog_product_bug",
    "scheduled",
    "imported",
];

/// Task watchdog product bug origin kind（用于标记 watchdog 识别的产品 bug）。
pub const TASK_WATCHDOG_PRODUCT_BUG_ORIGIN_KIND: &str = "task_watchdog_product_bug";

/// Issue watchdog discovery kind。
pub const ISSUE_WATCHDOG_DISCOVERY_KINDS: &[&str] = &["product_bug", "platform_bug"];

/// Issue surface visibility。
pub const ISSUE_SURFACE_VISIBILITIES: &[&str] = &["default", "plugin_operation"];

/// Issue recovery action kind。
pub const ISSUE_RECOVERY_ACTION_KINDS: &[&str] = &[
    "create_orphan_recovery",
    "adopt_existing_recovery",
    "cancel_recovery",
    "rerun_recovery",
    "assign_recovery_owner",
    "mark_source_resolved",
];

/// Issue recovery action status。
pub const ISSUE_RECOVERY_ACTION_STATUSES: &[&str] = &["pending", "applied", "skipped", "failed"];

/// Issue recovery action owner type。
pub const ISSUE_RECOVERY_ACTION_OWNER_TYPES: &[&str] = &["user", "agent"];

/// Issue recovery action outcome。
pub const ISSUE_RECOVERY_ACTION_OUTCOMES: &[&str] =
    &["recovered", "no_change", "escalated", "obsolete"];

/// Issue continuation summary document key（与 Node 对齐）。
pub const ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY: &str = "continuation-summary";

/// Pipeline case body document key（与 Node 对齐）。
pub const PIPELINE_CASE_BODY_DOCUMENT_KEY: &str = "pipeline-case-body";

/// Pipeline automation 默认标题模板。
pub const PIPELINE_AUTOMATION_DEFAULT_TITLE_TEMPLATE: &str =
    "{{pipeline_name}} / {{stage_name}}: {{case_title}}";

/// System issue document keys（用于系统级文档注册）。
pub const SYSTEM_ISSUE_DOCUMENT_KEYS: &[&str] = &[
    "continuation-summary",
    "pipeline-case-body",
    "issue-graph-liveness-preview",
    "liveness-escalation-description",
];

/// Issue reference source kind。
pub const ISSUE_REFERENCE_SOURCE_KINDS: &[&str] = &["title", "description", "comment", "document"];

/// Issue thread interaction kind。
pub const ISSUE_THREAD_INTERACTION_KINDS: &[&str] = &[
    "request_checkbox_confirmation",
    "request_form_response",
    "request_item_verdict",
    "comment",
    "approval_decision",
];

/// Issue thread interaction 状态。
pub const ISSUE_THREAD_INTERACTION_STATUSES: &[&str] =
    &["pending", "answered", "expired", "cancelled"];

/// Issue thread interaction continuation policy。
pub const ISSUE_THREAD_INTERACTION_CONTINUATION_POLICIES: &[&str] =
    &["wait_for_user", "auto_resolve", "manual_followup"];

/// Request checkbox confirmation option limit。
pub const REQUEST_CHECKBOX_CONFIRMATION_OPTION_LIMIT: u32 = 200;

/// Request item verdict item limit（与 checkbox limit 镜像）。
pub const REQUEST_ITEM_VERDICTS_ITEM_LIMIT: u32 = REQUEST_CHECKBOX_CONFIRMATION_OPTION_LIMIT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_statuses_match_node() {
        assert_eq!(
            ISSUE_STATUSES,
            &[
                "backlog",
                "todo",
                "in_progress",
                "in_review",
                "done",
                "cancelled",
                "blocked",
            ]
        );
    }

    #[test]
    fn inbox_filter_matches_statuses() {
        // INBOX filter is derived from INBOX_MINE_ISSUE_STATUSES
        let expected = INBOX_MINE_ISSUE_STATUSES.join(",");
        assert_eq!(INBOX_MINE_ISSUE_STATUS_FILTER, expected);
    }

    #[test]
    fn priorities_match_node() {
        assert_eq!(ISSUE_PRIORITIES, &["critical", "high", "medium", "low"]);
    }

    #[test]
    fn work_modes_contains_skill_test() {
        assert!(ISSUE_WORK_MODES.contains(&"skill_test"));
        assert_eq!(ISSUE_HARNESS_KINDS, &["skill_test"]);
    }

    #[test]
    fn max_request_depth_is_1024() {
        assert_eq!(MAX_ISSUE_REQUEST_DEPTH, 1024);
    }

    #[test]
    fn relation_types_is_blocks_only() {
        assert_eq!(ISSUE_RELATION_TYPES, &["blocks"]);
    }

    #[test]
    fn tree_control_modes_match_node() {
        assert_eq!(
            ISSUE_TREE_CONTROL_MODES,
            &["pause", "resume", "cancel", "restore"]
        );
    }

    #[test]
    fn system_doc_keys_includes_continuation() {
        assert!(SYSTEM_ISSUE_DOCUMENT_KEYS.contains(&ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY));
        assert!(SYSTEM_ISSUE_DOCUMENT_KEYS.contains(&PIPELINE_CASE_BODY_DOCUMENT_KEY));
    }

    #[test]
    fn request_limits_match() {
        assert_eq!(
            REQUEST_ITEM_VERDICTS_ITEM_LIMIT,
            REQUEST_CHECKBOX_CONFIRMATION_OPTION_LIMIT
        );
        assert_eq!(REQUEST_CHECKBOX_CONFIRMATION_OPTION_LIMIT, 200);
    }
}
