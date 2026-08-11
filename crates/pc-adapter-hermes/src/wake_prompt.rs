//! Paperclip wake payload prompt 渲染（对齐 Node
//! `packages/adapter-utils/src/server-utils.ts` 中的
//! `stringifyPaperclipWakePayload` + `renderPaperclipWakePrompt` +
//! `selectPaperclipTaskMarkdown` + `isPaperclipRecoveryWakePayload`）。
//!
//! Rust 端选择**精简版**实现：保留对外契约（输入 wake JSON → 输出 prompt
//! markdown 字符串）与关键语义（recovery contract / execution contract /
//! task markdown variant），但去掉 Node 端超过 100 行的特定 reason 指令
//! 与 plan-review 渲染（与 Hermes 当前 task lane 无关）。

use serde_json::Value;

/// 决定 wake payload 是否为 recovery 类别（与 Node
/// `isPaperclipRecoveryWakePayload` 一致）。
pub fn is_recovery_wake_payload(wake: Option<&Value>) -> bool {
    let normalized = match wake.and_then(|w| w.as_object()) {
        Some(map) => map,
        None => return false,
    };
    normalized
        .get("recovery")
        .map(|v| !v.is_null())
        .unwrap_or(false)
        || normalized
            .get("reason")
            .and_then(Value::as_str)
            .map(|reason| reason == "source_scoped_recovery_action")
            .unwrap_or(false)
}

/// 选中 task markdown 变体（full / compact）。
///
/// 行为对齐 Node `selectPaperclipTaskMarkdown`：
/// - 全新会话（!resumed）→ 始终 full
/// - 恢复会话 + wake 缺失 → full
/// - 恢复会话 + wake 是 assignment 形（issue_assigned / reopened /
///   recovery_action_restored / tree_restored）→ full
/// - 恢复会话 + wake 是 recovery → full
/// - 其他恢复场景 → 优先 compact，回退 full
pub fn select_task_markdown(context: Option<&Value>, resumed_session: bool) -> String {
    let context = match context.and_then(|c| c.as_object()) {
        Some(map) => map,
        None => return String::new(),
    };
    let full = context
        .get("paperclipTaskMarkdown")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let full = match full {
        Some(value) => value,
        None => return String::new(),
    };
    if !resumed_session {
        return full;
    }
    let wake = context.get("paperclipWake");
    if wake.is_none() {
        return full;
    }
    if is_recovery_wake_payload(wake) {
        return full;
    }
    if is_assignment_shaped_reason(wake) {
        return full;
    }
    context
        .get("paperclipTaskMarkdownCompact")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or(full)
}

fn is_assignment_shaped_reason(wake: Option<&Value>) -> bool {
    let reason = wake.and_then(|w| w.get("reason")).and_then(Value::as_str);
    matches!(
        reason,
        Some(
            "issue_assigned"
                | "issue_reopened_via_comment"
                | "issue_recovery_action_restored"
                | "issue_tree_restored"
        )
    )
}

/// 渲染 wake payload → JSON 字符串（供 `PAPERCLIP_WAKE_PAYLOAD_JSON` env）。
///
/// 与 Node `stringifyPaperclipWakePayload` 行为一致：null 输入返回 None，
/// 否则序列化整个对象。
pub fn stringify_wake_payload(wake: Option<&Value>) -> Option<String> {
    let normalized = wake?;
    serde_json::to_string(normalized).ok()
}

/// 渲染 wake prompt 字符串。
///
/// `resumed_session` 与 `suppress_issue_description` 两个开关对齐 Node
/// `renderPaperclipWakePrompt` 的 `options`。
///
/// 简化决策：
/// - 空 wake → ""
/// - `resumed_session || include_execution_contract` 时加 execution contract
/// - `recovery` 类型 wake → 加 recovery contract + cause-specific instruction
pub fn render_wake_prompt(wake: Option<&Value>, resumed_session: bool) -> String {
    let normalized = match wake.and_then(|w| w.as_object()) {
        Some(map) => map,
        None => return String::new(),
    };

    let reason = normalized
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let issue = normalized.get("issue").and_then(|w| w.as_object());
    let issue_identifier = issue
        .and_then(|i| i.get("identifier").or_else(|| i.get("id")))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let issue_title = issue
        .and_then(|i| i.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let issue_title_suffix = if issue_title.is_empty() {
        String::new()
    } else {
        format!(" {issue_title}")
    };

    let recovery_scoped = is_recovery_wake_payload(wake);
    let contract_lines = if recovery_scoped {
        vec![
            "Recovery contract: your job is to RECOVER this task, not to do the work. Do not produce the deliverable yourself.".to_string(),
            format!("Cause: {}.", normalized
                .get("recovery")
                .and_then(|r| r.get("cause"))
                .and_then(Value::as_str)
                .unwrap_or("unspecified")),
            "Fix the underlying problem and hand the issue back to the original assignee.".to_string(),
        ]
    } else if resumed_session {
        vec![
            "Execution contract: take concrete action in this heartbeat when the issue is actionable. Leave durable progress and set a final disposition before ending (`done` / `in_review` / `blocked` / delegated follow-up / `in_progress` with a live continuation path).".to_string(),
        ]
    } else {
        Vec::new()
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push("## Paperclip wake context".to_string());
    if !contract_lines.is_empty() {
        lines.extend(contract_lines);
        lines.push(String::new());
    }
    lines.push(format!("- reason: {reason}"));
    lines.push(format!("- issue: {issue_identifier}{issue_title_suffix}"));
    if let Some(comment_id) = normalized.get("commentId").and_then(Value::as_str) {
        lines.push(format!("- commentId: {comment_id}"));
    }
    if let Some(stage) = normalized.get("executionStage").and_then(|w| w.as_object()) {
        if let Some(role) = stage.get("wakeRole").and_then(Value::as_str) {
            lines.push(format!("- stageRole: {role}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_wake_returns_empty_string() {
        assert_eq!(render_wake_prompt(None, false), "");
        assert_eq!(render_wake_prompt(Some(&Value::Null), false), "");
    }

    #[test]
    fn recovery_wake_detection() {
        let wake = json!({"recovery": {"cause": "process_lost"}});
        assert!(is_recovery_wake_payload(Some(&wake)));
        let wake2 = json!({"reason": "source_scoped_recovery_action"});
        assert!(is_recovery_wake_payload(Some(&wake2)));
        let wake3 = json!({"reason": "issue_assigned"});
        assert!(!is_recovery_wake_payload(Some(&wake3)));
    }

    #[test]
    fn stringify_serializes_wake() {
        let wake = json!({"reason": "issue_assigned", "issue": {"id": "T-1"}});
        let json = stringify_wake_payload(Some(&wake)).expect("stringify");
        assert!(json.contains("issue_assigned"));
        assert!(json.contains("T-1"));
    }

    #[test]
    fn select_task_markdown_full_for_fresh_session() {
        let ctx = json!({"paperclipTaskMarkdown": "FULL BRIEF"});
        assert_eq!(select_task_markdown(Some(&ctx), false), "FULL BRIEF");
    }

    #[test]
    fn select_task_markdown_compact_for_resume_without_assignment() {
        let ctx = json!({
            "paperclipTaskMarkdown": "FULL",
            "paperclipTaskMarkdownCompact": "COMPACT",
            "paperclipWake": {"reason": "comment_added"}
        });
        assert_eq!(select_task_markdown(Some(&ctx), true), "COMPACT");
    }

    #[test]
    fn select_task_markdown_full_for_assignment_shaped_wake() {
        let ctx = json!({
            "paperclipTaskMarkdown": "FULL",
            "paperclipTaskMarkdownCompact": "COMPACT",
            "paperclipWake": {"reason": "issue_assigned"}
        });
        assert_eq!(select_task_markdown(Some(&ctx), true), "FULL");
    }

    #[test]
    fn select_task_markdown_falls_back_to_full_when_no_compact() {
        let ctx = json!({
            "paperclipTaskMarkdown": "FULL",
            "paperclipWake": {"reason": "comment_added"}
        });
        assert_eq!(select_task_markdown(Some(&ctx), true), "FULL");
    }

    #[test]
    fn render_basic_wake() {
        let wake = json!({
            "reason": "issue_assigned",
            "issue": {"id": "T-1", "title": "Fix bug"}
        });
        let prompt = render_wake_prompt(Some(&wake), false);
        assert!(prompt.contains("issue_assigned"));
        assert!(prompt.contains("T-1"));
        assert!(prompt.contains("Fix bug"));
        // fresh session without include_execution_contract → no execution contract line
        assert!(!prompt.contains("Execution contract"));
    }

    #[test]
    fn render_recovery_wake_includes_recovery_contract() {
        let wake = json!({
            "reason": "issue_recovery_action_restored",
            "recovery": {"cause": "process_lost", "failureSummary": "killed by monitor"}
        });
        let prompt = render_wake_prompt(Some(&wake), false);
        assert!(prompt.contains("Recovery contract"));
        assert!(prompt.contains("process_lost"));
    }

    #[test]
    fn render_resumed_session_includes_execution_contract() {
        let wake = json!({
            "reason": "issue_assigned",
            "issue": {"id": "T-2"}
        });
        let prompt = render_wake_prompt(Some(&wake), true);
        assert!(prompt.contains("Execution contract"));
    }
}
