use pc_acpx::server_utils_wake::{
    is_assignment_shaped_paperclip_wake_reason, is_paperclip_recovery_wake_payload,
    normalize_paperclip_wake_agent_message, normalize_paperclip_wake_issue,
    normalize_paperclip_wake_payload, read_paperclip_issue_work_mode_from_context,
    select_paperclip_task_markdown, stringify_paperclip_wake_payload,
    SelectTaskMarkdownOptions, StringifyWakePayloadOptions,
    ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS, DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE,
    WATCHDOG_DEFAULT_MANDATE,
};

#[test]
fn 模板包含核心执行约束() {
    assert!(DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE.contains("Execution contract"));
    assert!(DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE.contains("PAPERCLIP_SCRATCH_DIR"));
    assert!(WATCHDOG_DEFAULT_MANDATE.contains("Safety constraints"));
}

#[test]
fn 恢复与问题可从真实唤醒载荷归一化() {
    let payload = serde_json::json!({
        "reason": "issue_recovery_action_restored",
        "recovery": {"cause": "process_lost", "attemptCount": 2},
        "issue": {"identifier": "PC-42", "workMode": "implementation"}
    });
    let normalized = normalize_paperclip_wake_payload(&payload).unwrap();
    assert_eq!(normalized.reason.as_deref(), Some("issue_recovery_action_restored"));
    assert_eq!(normalized.recovery.unwrap().attempt_count, Some(2));
    assert_eq!(normalized.issue.unwrap().identifier.as_deref(), Some("PC-42"));
}

#[test]
fn agent_message会清除控制字符并保留正文() {
    let message = normalize_paperclip_wake_agent_message(
        &serde_json::json!({"text": "开始\u{0000}执行\u{0007}"}),
    )
    .unwrap();
    assert_eq!(message.text, "开始执行");
}

#[test]
fn 空问题不会被误认为有效问题() {
    assert!(normalize_paperclip_wake_issue(&serde_json::json!({})).is_none());
    assert!(normalize_paperclip_wake_issue(&serde_json::json!({"workMode": "review"})).is_some());
}

#[test]
fn 恢复判定同时支持恢复对象和来源范围原因() {
    assert!(is_paperclip_recovery_wake_payload(
        &serde_json::json!({"recovery": {"cause": "timeout"}})
    ));
    assert!(is_paperclip_recovery_wake_payload(
        &serde_json::json!({"reason": "source_scoped_recovery_action"})
    ));
    assert!(!is_paperclip_recovery_wake_payload(
        &serde_json::json!({"reason": "issue_commented"})
    ));
}

#[test]
fn 工作模式优先使用直接上下文() {
    let context = serde_json::json!({
        "paperclipIssue": {"workMode": "direct"},
        "paperclipWake": {"issue": {"workMode": "wake"}}
    });
    assert_eq!(read_paperclip_issue_work_mode_from_context(&context).as_deref(), Some("direct"));
}

#[test]
fn 工作模式可回退到唤醒问题() {
    let context = serde_json::json!({"paperclipWake": {"issue": {"workMode": "wake"}}});
    assert_eq!(read_paperclip_issue_work_mode_from_context(&context).as_deref(), Some("wake"));
}

#[test]
fn 任务摘要按新会话和唤醒类型选择() {
    let fresh = serde_json::json!({
        "paperclipTaskMarkdown": "FULL",
        "paperclipTaskMarkdownCompact": "COMPACT"
    });
    assert_eq!(select_paperclip_task_markdown(Some(&fresh), SelectTaskMarkdownOptions::default()), "FULL");

    let delta = serde_json::json!({
        "paperclipTaskMarkdown": "FULL",
        "paperclipTaskMarkdownCompact": "COMPACT",
        "paperclipWake": {"reason": "issue_commented"}
    });
    assert_eq!(
        select_paperclip_task_markdown(Some(&delta), SelectTaskMarkdownOptions { resumed_session: true }),
        "COMPACT"
    );
}

#[test]
fn 指派和恢复唤醒在恢复会话仍返回完整摘要() {
    let context = serde_json::json!({
        "paperclipTaskMarkdown": "FULL",
        "paperclipTaskMarkdownCompact": "COMPACT",
        "paperclipWake": {"reason": "issue_assigned"}
    });
    assert_eq!(
        select_paperclip_task_markdown(Some(&context), SelectTaskMarkdownOptions { resumed_session: true }),
        "FULL"
    );
    for reason in ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS {
        assert!(is_assignment_shaped_paperclip_wake_reason(Some(reason)));
    }
}

#[test]
fn 字符串化可省略问题描述并保留其他字段() {
    let payload = serde_json::json!({
        "reason": "issue_assigned",
        "issue": {"id": "i-1", "title": "标题", "description": "机密", "descriptionTruncated": true}
    });
    let text = stringify_paperclip_wake_payload(
        &payload,
        StringifyWakePayloadOptions { omit_issue_description: true },
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["reason"], "issue_assigned");
    assert_eq!(value["issue"]["description"], serde_json::Value::Null);
    assert_eq!(value["issue"]["descriptionTruncated"], false);
}
