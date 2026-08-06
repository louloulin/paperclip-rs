//! Round 251: Task-watchdog wake context builder（与 Node `watchdogWakeContext` 1:1 对齐）。
//!
//! 作用：当 watchdog evaluation 触发新的 heartbeat_runs 时，把
//! `{ taskWatchdog: { watchedIssueId, ... }, watchdogId, watchedIssueId, ... }`
//! 写入 `heartbeat_runs.context_snapshot`，使得后续的
//! `resolve_task_watchdog_mutation_scope` 能通过 `read_task_watchdog_context` 读取并匹配。

use serde_json::{json, Value};
use uuid::Uuid;

use super::types::TASK_WATCHDOG_ORIGIN_KIND;

/// `TaskWatchdogWakeContext` 输入参数（与 Node `watchdogWakeContext` 1:1 对齐）。
///
/// 注：Node 端是从 `IssueWatchdogRow` + `IssueRow` (watchdogIssue / sourceIssue) +
/// `TaskWatchdogClassifierResult` 派生。Rust 端为简化，只取最关键的几个字段；
/// 后续可扩展 `pendingInteractions` / `capabilities` 等嵌套结构。
#[derive(Debug, Clone)]
pub struct TaskWatchdogWakeInput<'a> {
    /// Watchdog 行 id
    pub watchdog_id: Uuid,
    /// 被监听的 issue id（即 source issue）
    pub watched_issue_id: Uuid,
    /// 被监听的 issue 标识（人类可读，例如 `PMA-123`）
    pub watched_issue_identifier: Option<&'a str>,
    /// 被监听的 issue 标题
    pub watched_issue_title: Option<&'a str>,
    /// Watchdog agent 触发的 task issue id（与 Node `watchdogIssue.id` 对齐）
    pub watchdog_issue_id: Option<Uuid>,
    /// 当前 stop fingerprint（与 Node `classification.stopFingerprint` 对齐）
    pub stop_fingerprint: Option<&'a str>,
    /// 自定义 instructions（来自 `issue_watchdogs.instructions`）
    pub custom_instructions: Option<&'a str>,
}

/// 构造 heartbeat_runs.context_snapshot 写入对象。
///
/// 完整 Node 形状：
/// ```jsonc
/// {
///   "issueId": "<watchdogIssue.id>",
///   "taskId": "<watchdogIssue.id>",
///   "wakeReason": "task_watchdog_stopped_subtree",
///   "source": "task_watchdog",
///   "taskWatchdog": {
///     "watchedIssueId": "...",
///     "watchedIssueIdentifier": "PMA-123",
///     "watchedIssueTitle": "...",
///     "stopFingerprint": "...",
///     "capabilities": {
///       "targetScope": { "watchedIssueId": "...", "includeNonWatchdogDescendants": true, ... },
///       "operations": [...],
///       "deniedOperations": [...]
///     }
///   },
///   "watchdogId": "...",
///   "watchedIssueId": "...",
///   "watchedIssueIdentifier": "PMA-123",
///   "stopFingerprint": "...",
///   "customInstructions": "...",
///   "resumeIntent": true,
///   "followUpRequested": true,
/// }
/// ```
///
/// R251 实现聚焦「能被 `read_task_watchdog_context` 消费」的最小子集：
/// - 顶层 `watchdogId` / `watchedIssueId` / `watchedIssueIdentifier` / `stopFingerprint` / `customInstructions`
/// - `taskWatchdog` 嵌套对象（含 `watchedIssueId` / `stopFingerprint` / `watchedIssueIdentifier` / `watchedIssueTitle`）
/// - `source: "task_watchdog"` 标记
/// - `wakeReason: "task_watchdog_stopped_subtree"`
///
/// capabilities / operations / deniedOperations 在后续轮次按需扩展。
pub fn build_task_watchdog_wake_context(input: &TaskWatchdogWakeInput<'_>) -> Value {
    let mut ctx = json!({
        "source": TASK_WATCHDOG_ORIGIN_KIND,
        "wakeReason": "task_watchdog_stopped_subtree",
        "watchdogId": input.watchdog_id,
        "watchedIssueId": input.watched_issue_id,
        "watchedIssueIdentifier": input.watched_issue_identifier,
        "stopFingerprint": input.stop_fingerprint,
        "customInstructions": input.custom_instructions,
        "resumeIntent": true,
        "followUpRequested": true,
    });

    // 兼容 Node `watchdogIssueId`：如果提供了 watchdog_issue_id，写到顶层 issueId/taskId
    if let Some(wiid) = input.watchdog_issue_id {
        ctx["issueId"] = json!(wiid);
        ctx["taskId"] = json!(wiid);
    }

    // 嵌套 taskWatchdog 对象 —— `read_task_watchdog_context` 消费的就是这个 key
    ctx["taskWatchdog"] = json!({
        "watchedIssueId": input.watched_issue_id,
        "watchedIssueIdentifier": input.watched_issue_identifier,
        "watchedIssueTitle": input.watched_issue_title,
        "stopFingerprint": input.stop_fingerprint,
    });

    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_watchdog_scope::helpers::read_task_watchdog_context;

    #[test]
    fn build_context_includes_all_node_top_level_keys() {
        let input = TaskWatchdogWakeInput {
            watchdog_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            watched_issue_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            watched_issue_identifier: Some("PMA-42"),
            watched_issue_title: Some("Fix login bug"),
            watchdog_issue_id: Some(Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()),
            stop_fingerprint: Some("fp-abc"),
            custom_instructions: Some("Don't break tests"),
        };
        let ctx = build_task_watchdog_wake_context(&input);
        // 顶层键
        assert_eq!(ctx["watchdogId"], input.watchdog_id.to_string());
        assert_eq!(ctx["watchedIssueId"], input.watched_issue_id.to_string());
        assert_eq!(ctx["watchedIssueIdentifier"], "PMA-42");
        assert_eq!(ctx["stopFingerprint"], "fp-abc");
        assert_eq!(ctx["customInstructions"], "Don't break tests");
        assert_eq!(ctx["resumeIntent"], true);
        assert_eq!(ctx["followUpRequested"], true);
        assert_eq!(ctx["source"], "task_watchdog");
        assert_eq!(ctx["wakeReason"], "task_watchdog_stopped_subtree");
        // issueId / taskId 来自 watchdog_issue_id
        assert_eq!(
            ctx["issueId"],
            "33333333-3333-3333-3333-333333333333"
        );
        assert_eq!(
            ctx["taskId"],
            "33333333-3333-3333-3333-333333333333"
        );
    }

    #[test]
    fn build_context_round_trips_through_read_task_watchdog_context() {
        let input = TaskWatchdogWakeInput {
            watchdog_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            watched_issue_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            watched_issue_identifier: Some("PMA-42"),
            watched_issue_title: Some("Fix login bug"),
            watchdog_issue_id: None,
            stop_fingerprint: Some("fp-abc"),
            custom_instructions: None,
        };
        let ctx = build_task_watchdog_wake_context(&input);
        // read_task_watchdog_context 必须能解析出 watched_issue_id + stop_fingerprint
        let parsed = read_task_watchdog_context(Some(&ctx)).expect("must parse");
        assert_eq!(
            parsed.watched_issue_id.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
        assert_eq!(parsed.stop_fingerprint.as_deref(), Some("fp-abc"));
    }

    #[test]
    fn build_context_omits_watchdog_issue_id_when_none() {
        let input = TaskWatchdogWakeInput {
            watchdog_id: Uuid::new_v4(),
            watched_issue_id: Uuid::new_v4(),
            watched_issue_identifier: None,
            watched_issue_title: None,
            watchdog_issue_id: None,
            stop_fingerprint: None,
            custom_instructions: None,
        };
        let ctx = build_task_watchdog_wake_context(&input);
        assert!(ctx.get("issueId").is_none());
        assert!(ctx.get("taskId").is_none());
    }
}
