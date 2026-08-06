//! `issue_change_receipt` 模块的纯函数单元测试。
//!
//! `build_issue_changes` 是纯函数（无 DB I/O），适合单元测试覆盖各种边界场景。
//!
//! 关键规则（与 Node `issue-change-receipt.ts` 1:1）：
//! - 忽略 `updatedAt`（永远变化）
//! - description 总是 truncate 到 200 字符 + `updated: true`
//! - title 任一长度 > 200 → truncate + `updated: true`
//! - 深比较（用 serde_json::Value PartialEq）
//! - relation changes：去重 + 排序后再比较（避免顺序差异触发 false positive）
use pc_heartbeat::recovery::{build_issue_changes, IssueChanges};
use serde_json::{json, Value};

fn s(val: &str) -> Value {
    Value::String(val.to_string())
}

#[test]
fn no_changes_when_existing_equals_updated() {
    let existing = json!({"title": "Same", "status": "todo"})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({"title": "Same", "status": "todo"})
        .as_object()
        .unwrap()
        .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert!(result.is_empty());
}

#[test]
fn ignores_updated_at_field() {
    let existing = json!({"title": "A", "updatedAt": "2024-01-01"})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({"title": "A", "updatedAt": "2024-12-31"})
        .as_object()
        .unwrap()
        .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert!(result.is_empty(), "updatedAt should be ignored");
}

#[test]
fn detects_status_change() {
    let existing = json!({"status": "todo"}).as_object().unwrap().clone();
    let updated = json!({"status": "done"}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert_eq!(result.fields.len(), 1);
    let change = result
        .fields
        .get("status")
        .expect("status should be in changes");
    assert_eq!(change.from, s("todo"));
    assert_eq!(change.to, s("done"));
    assert!(!change.updated);
}

#[test]
fn detects_assignee_change() {
    let existing = json!({"assigneeAgentId": "agent-1"})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({"assigneeAgentId": "agent-2"})
        .as_object()
        .unwrap()
        .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert_eq!(result.fields.len(), 1);
    let change = result.fields.get("assigneeAgentId").unwrap();
    assert_eq!(change.from, s("agent-1"));
    assert_eq!(change.to, s("agent-2"));
}

#[test]
fn description_always_truncated_with_updated_flag() {
    let long_text = "x".repeat(500);
    let existing = json!({"description": long_text})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({"description": "short"}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let change = result
        .fields
        .get("description")
        .expect("description should be in changes");
    // truncate 到 200 字符
    assert_eq!(change.from.as_str().unwrap().chars().count(), 200);
    assert_eq!(change.to.as_str().unwrap(), "short");
    assert!(change.updated, "description change must have updated=true");
}

#[test]
fn description_truncates_even_when_added_field() {
    let long_text = "y".repeat(1000);
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({"description": long_text})
        .as_object()
        .unwrap()
        .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let change = result
        .fields
        .get("description")
        .expect("description should appear");
    assert_eq!(change.from, Value::Null);
    assert_eq!(change.to.as_str().unwrap().chars().count(), 200);
    assert!(change.updated);
}

#[test]
fn title_short_no_truncate_no_updated_flag() {
    let existing = json!({"title": "Short"}).as_object().unwrap().clone();
    let updated = json!({"title": "New short"}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let change = result
        .fields
        .get("title")
        .expect("title should be in changes");
    assert_eq!(change.from, s("Short"));
    assert_eq!(change.to, s("New short"));
    assert!(!change.updated);
}

#[test]
fn title_long_truncated_with_updated_flag() {
    let long_title = "z".repeat(300);
    let existing = json!({"title": "OK"}).as_object().unwrap().clone();
    let updated = json!({"title": long_title}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let change = result
        .fields
        .get("title")
        .expect("title should be in changes");
    assert_eq!(change.from, s("OK"));
    assert_eq!(change.to.as_str().unwrap().chars().count(), 200);
    assert!(change.updated);
}

#[test]
fn detects_null_to_value_change() {
    let existing = json!({"description": null}).as_object().unwrap().clone();
    let updated = json!({"description": "new"}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let change = result.fields.get("description").unwrap();
    assert_eq!(change.from, Value::Null);
    assert_eq!(change.to, s("new"));
}

#[test]
fn detects_value_to_null_change() {
    let existing = json!({"description": "old"}).as_object().unwrap().clone();
    let updated = json!({"description": null}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let change = result.fields.get("description").unwrap();
    assert_eq!(change.from, s("old"));
    assert_eq!(change.to, Value::Null);
}

#[test]
fn multiple_changes_detected_independently() {
    let existing = json!({
        "status": "todo",
        "title": "Old",
        "priority": "high",
    })
    .as_object()
    .unwrap()
    .clone();
    let updated = json!({
        "status": "done",
        "title": "New",
        "priority": "high",
    })
    .as_object()
    .unwrap()
    .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert_eq!(result.fields.len(), 2);
    assert!(result.fields.contains_key("status"));
    assert!(result.fields.contains_key("title"));
    assert!(!result.fields.contains_key("priority"));
}

#[test]
fn relation_change_blocked_by_issue_ids_detected() {
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();

    let rel = pc_heartbeat::recovery::RelationChangeInput {
        blocked_by_issue_ids: Some(pc_heartbeat::recovery::IdArrayChange {
            from: vec!["a".to_string(), "b".to_string()],
            to: vec!["a".to_string(), "c".to_string()],
        }),
        label_ids: None,
    };

    let result = build_issue_changes(&existing, &updated, rel);
    assert_eq!(result.fields.len(), 1);
    let change = result.fields.get("blockedByIssueIds").unwrap();
    assert_eq!(change.from, json!(["a".to_string(), "b".to_string()]));
    assert_eq!(change.to, json!(["a".to_string(), "c".to_string()]));
}

#[test]
fn relation_change_canonical_sort_order() {
    // 输入顺序不同，但 canonical 后相同 → 无 change
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();

    let rel = pc_heartbeat::recovery::RelationChangeInput {
        blocked_by_issue_ids: Some(pc_heartbeat::recovery::IdArrayChange {
            from: vec!["b".to_string(), "a".to_string()],
            to: vec!["a".to_string(), "b".to_string()],
        }),
        label_ids: None,
    };

    let result = build_issue_changes(&existing, &updated, rel);
    assert!(
        result.is_empty(),
        "different order but same set → no change"
    );
}

#[test]
fn relation_change_dedup_handles_duplicates() {
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();

    let rel = pc_heartbeat::recovery::RelationChangeInput {
        blocked_by_issue_ids: Some(pc_heartbeat::recovery::IdArrayChange {
            from: vec!["a".to_string(), "a".to_string(), "b".to_string()],
            to: vec!["a".to_string(), "b".to_string()],
        }),
        label_ids: None,
    };

    let result = build_issue_changes(&existing, &updated, rel);
    assert!(result.is_empty(), "duplicates dedup'd → no change");
}

#[test]
fn relation_change_label_ids_detected() {
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();

    let rel = pc_heartbeat::recovery::RelationChangeInput {
        blocked_by_issue_ids: None,
        label_ids: Some(pc_heartbeat::recovery::IdArrayChange {
            from: vec!["bug".to_string()],
            to: vec!["feature".to_string(), "urgent".to_string()],
        }),
    };

    let result = build_issue_changes(&existing, &updated, rel);
    assert_eq!(result.fields.len(), 1);
    let change = result.fields.get("labelIds").unwrap();
    assert_eq!(change.from, json!(["bug".to_string()]));
    assert_eq!(
        change.to,
        json!(["feature".to_string(), "urgent".to_string()])
    );
}

#[test]
fn deep_equal_handles_nested_objects() {
    let existing = json!({
        "executionState": {"status": "pending", "currentStageId": "stage-1"}
    })
    .as_object()
    .unwrap()
    .clone();
    let updated = json!({
        "executionState": {"status": "pending", "currentStageId": "stage-1"}
    })
    .as_object()
    .unwrap()
    .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert!(
        result.is_empty(),
        "nested objects with same content → no change"
    );
}

#[test]
fn detects_nested_object_change() {
    let existing = json!({
        "executionState": {"status": "pending", "currentStageId": "stage-1"}
    })
    .as_object()
    .unwrap()
    .clone();
    let updated = json!({
        "executionState": {"status": "running", "currentStageId": "stage-1"}
    })
    .as_object()
    .unwrap()
    .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert_eq!(result.fields.len(), 1);
    let change = result.fields.get("executionState").unwrap();
    assert_eq!(
        change.from,
        json!({"status": "pending", "currentStageId": "stage-1"})
    );
    assert_eq!(
        change.to,
        json!({"status": "running", "currentStageId": "stage-1"})
    );
}

#[test]
fn detects_array_change() {
    let existing = json!({"labels": ["bug", "p1"]})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({"labels": ["bug", "p2"]})
        .as_object()
        .unwrap()
        .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert_eq!(result.fields.len(), 1);
    let change = result.fields.get("labels").unwrap();
    assert_eq!(change.from, json!(["bug", "p1"]));
    assert_eq!(change.to, json!(["bug", "p2"]));
}

#[test]
fn mixed_field_and_relation_changes() {
    let existing = json!({"status": "todo"}).as_object().unwrap().clone();
    let updated = json!({"status": "done"}).as_object().unwrap().clone();

    let rel = pc_heartbeat::recovery::RelationChangeInput {
        blocked_by_issue_ids: Some(pc_heartbeat::recovery::IdArrayChange {
            from: vec![],
            to: vec!["x".to_string()],
        }),
        label_ids: None,
    };

    let result = build_issue_changes(&existing, &updated, rel);
    assert_eq!(result.fields.len(), 2);
    assert!(result.fields.contains_key("status"));
    assert!(result.fields.contains_key("blockedByIssueIds"));
}

#[test]
fn empty_inputs_produce_empty_changes() {
    let existing = serde_json::Map::new();
    let updated = serde_json::Map::new();
    let result = build_issue_changes(&existing, &updated, Default::default());
    assert!(result.is_empty());
}

#[test]
fn keys_only_in_existing_detected() {
    let existing = json!({"removedField": "value"})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert_eq!(result.fields.len(), 1);
    let change = result.fields.get("removedField").unwrap();
    assert_eq!(change.from, s("value"));
    assert_eq!(change.to, Value::Null);
}

#[test]
fn keys_only_in_updated_detected() {
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({"newField": "value"}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    assert_eq!(result.fields.len(), 1);
    let change = result.fields.get("newField").unwrap();
    assert_eq!(change.from, Value::Null);
    assert_eq!(change.to, s("value"));
}

#[test]
fn issue_changes_serializes_to_camel_case_json() {
    let existing = json!({"status": "todo"}).as_object().unwrap().clone();
    let updated = json!({"status": "done"}).as_object().unwrap().clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let serialized = serde_json::to_string(&result).unwrap();
    // 字段 key 是 camelCase（如果 caller 提供 blockedByIssueIds 等）
    assert!(serialized.contains("status"));
    // 普通字段不需要 updated 字段（default=false 时 skip）
    assert!(!serialized.contains("\"updated\""));
}

#[test]
fn long_text_serialization_includes_updated_flag() {
    let long_text = "x".repeat(500);
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({"description": long_text})
        .as_object()
        .unwrap()
        .clone();

    let result = build_issue_changes(&existing, &updated, Default::default());
    let serialized = serde_json::to_string(&result).unwrap();
    assert!(
        serialized.contains("\"updated\":true"),
        "long-text change must serialize updated:true, got: {}",
        serialized
    );
}
