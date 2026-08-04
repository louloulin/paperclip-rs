//! 纯助手函数（与 Node `task-watchdog-scope.ts` 顶部 3 个 helper 1:1 对齐）。

use serde_json::Value as JsonValue;

/// 判断 value 是否是 plain object（与 Node `isPlainRecord` 1:1 对齐）。
///
/// 接受：`typeof === "object" && !== null && !Array.isArray`。
#[must_use]
pub fn is_plain_record(value: &JsonValue) -> bool {
    value.is_object()
}

/// 把 unknown 强制为 plain record 引用（与 Node `isPlainRecord(x) ? x : null` 1:1 对齐）。
#[must_use]
pub fn as_plain_record(value: &JsonValue) -> Option<&serde_json::Map<String, JsonValue>> {
    value.as_object()
}

/// 读取 trim 后非空字符串（与 Node `readString` 1:1 对齐）。
#[must_use]
pub fn read_string(value: Option<&JsonValue>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 从 run.contextSnapshot 提取 taskWatchdog 上下文（与 Node `readTaskWatchdogContext` 1:1 对齐）。
///
/// 行为：
/// - `context` 不是 plain object → 返回 None
/// - `context.taskWatchdog` 不是 plain object 且 !== true → 返回 None
/// - 否则从 `context.taskWatchdog` 或 fallback 到 `context` 顶层读取 `watchedIssueId` / `stopFingerprint`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWatchdogContext {
    pub watched_issue_id: Option<String>,
    pub stop_fingerprint: Option<String>,
}

#[must_use]
pub fn read_task_watchdog_context(context_snapshot: Option<&JsonValue>) -> Option<TaskWatchdogContext> {
    let context = as_plain_record(context_snapshot?)?;
    let task_watchdog = context.get("taskWatchdog").and_then(as_plain_record);
    if task_watchdog.is_none() && context.get("taskWatchdog") != Some(&JsonValue::Bool(true)) {
        return None;
    }
    let watched_issue_id = read_string(task_watchdog.and_then(|t| t.get("watchedIssueId")))
        .or_else(|| read_string(context.get("watchedIssueId")));
    let stop_fingerprint = read_string(task_watchdog.and_then(|t| t.get("stopFingerprint")))
        .or_else(|| read_string(context.get("stopFingerprint")));
    Some(TaskWatchdogContext {
        watched_issue_id,
        stop_fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_plain_record_accepts_object() {
        assert!(is_plain_record(&json!({})));
        assert!(is_plain_record(&json!({"a": 1})));
    }

    #[test]
    fn is_plain_record_rejects_array_null_primitive() {
        assert!(!is_plain_record(&json!([1, 2, 3])));
        assert!(!is_plain_record(&json!(null)));
        assert!(!is_plain_record(&json!("hello")));
        assert!(!is_plain_record(&json!(42)));
        assert!(!is_plain_record(&json!(true)));
    }

    #[test]
    fn read_string_trims_and_filters_empty() {
        assert_eq!(read_string(None), None);
        assert_eq!(read_string(Some(&json!(""))), None);
        assert_eq!(read_string(Some(&json!("  "))), None);
        assert_eq!(read_string(Some(&json!("  hello  "))), Some("hello".to_string()));
    }

    #[test]
    fn read_string_rejects_non_string_types() {
        assert_eq!(read_string(Some(&json!(42))), None);
        assert_eq!(read_string(Some(&json!(null))), None);
        assert_eq!(read_string(Some(&json!(true))), None);
        assert_eq!(read_string(Some(&json!([]))), None);
        assert_eq!(read_string(Some(&json!({}))), None);
    }

    #[test]
    fn read_task_watchdog_context_requires_object() {
        assert_eq!(read_task_watchdog_context(None), None);
        assert_eq!(read_task_watchdog_context(Some(&json!(null))), None);
        assert_eq!(read_task_watchdog_context(Some(&json!("s"))), None);
    }

    #[test]
    fn read_task_watchdog_context_requires_task_watchdog_key() {
        assert_eq!(read_task_watchdog_context(Some(&json!({}))), None);
        assert_eq!(
            read_task_watchdog_context(Some(&json!({"foo": "bar"}))),
            None
        );
    }

    #[test]
    fn read_task_watchdog_context_accepts_explicit_object() {
        let ctx = read_task_watchdog_context(Some(&json!({
            "taskWatchdog": {
                "watchedIssueId": "issue-1",
                "stopFingerprint": "fp-1",
            }
        })))
        .expect("should parse");
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("issue-1"));
        assert_eq!(ctx.stop_fingerprint.as_deref(), Some("fp-1"));
    }

    #[test]
    fn read_task_watchdog_context_accepts_true_marker() {
        // `taskWatchdog: true` is a valid marker (Node behavior)
        let ctx = read_task_watchdog_context(Some(&json!({
            "taskWatchdog": true,
            "watchedIssueId": "issue-2",
        })))
        .expect("should parse with true marker");
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("issue-2"));
    }

    #[test]
    fn read_task_watchdog_context_falls_back_to_top_level_keys() {
        let ctx = read_task_watchdog_context(Some(&json!({
            "taskWatchdog": true,
            "watchedIssueId": "issue-3",
            "stopFingerprint": "fp-3",
        })))
        .expect("should parse");
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("issue-3"));
        assert_eq!(ctx.stop_fingerprint.as_deref(), Some("fp-3"));
    }

    #[test]
    fn read_task_watchdog_context_prefers_nested_over_top_level() {
        // taskWatchdog.watchedIssueId 优先于顶层 watchedIssueId
        let ctx = read_task_watchdog_context(Some(&json!({
            "taskWatchdog": {
                "watchedIssueId": "inner",
            },
            "watchedIssueId": "outer",
        })))
        .expect("should parse");
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("inner"));
    }
}
