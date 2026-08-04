//! Issue 更新变更收据。
//!
//! 对齐 Node `services/issue-change-receipt.ts::buildIssueChanges`：
//! 忽略 `updatedAt`，对长文本截断，并对关系 ID 数组做去重排序，
//! 以便活动日志和 continuation payload 稳定、可比较。

use std::collections::BTreeSet;

use serde_json::{Map, Value};

pub const ISSUE_CHANGE_TEXT_BUDGET: usize = 200;

#[derive(Debug, Clone, Default)]
pub struct IssueRelationChanges {
    pub blocked_by_issue_ids: Option<(Vec<String>, Vec<String>)>,
    pub label_ids: Option<(Vec<String>, Vec<String>)>,
}

pub type IssueChanges = Map<String, Value>;

fn truncate_text(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.chars().take(ISSUE_CHANGE_TEXT_BUDGET).collect()),
        _ => value.clone(),
    }
}

fn is_long_text(key: &str, from: &Value, to: &Value) -> bool {
    key == "description"
        || (key == "title"
            && [from, to].iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|text| text.chars().count() > ISSUE_CHANGE_TEXT_BUDGET)
            }))
}

fn canonical_id_array(values: &[String]) -> Value {
    Value::Array(
        BTreeSet::from_iter(values.iter().cloned())
            .into_iter()
            .map(Value::String)
            .collect(),
    )
}

fn add_change(changes: &mut IssueChanges, key: &str, from: Value, to: Value) {
    changes.insert(
        key.to_owned(),
        serde_json::json!({ "from": from, "to": to }),
    );
}

/// 构造稳定的 issue 变更收据。
pub fn build_issue_changes(
    existing: &Map<String, Value>,
    updated: &Map<String, Value>,
    relation_changes: &IssueRelationChanges,
) -> IssueChanges {
    let mut changes = IssueChanges::new();
    let keys: BTreeSet<&str> = existing
        .keys()
        .chain(updated.keys())
        .map(String::as_str)
        .filter(|key| *key != "updatedAt")
        .collect();

    for key in keys {
        let from = existing.get(key).cloned().unwrap_or(Value::Null);
        let to = updated.get(key).cloned().unwrap_or(Value::Null);
        if from == to {
            continue;
        }
        if is_long_text(key, &from, &to) {
            add_change(&mut changes, key, truncate_text(&from), truncate_text(&to));
        } else {
            add_change(&mut changes, key, from, to);
        }
    }

    if let Some((from, to)) = &relation_changes.blocked_by_issue_ids {
        let from = canonical_id_array(from);
        let to = canonical_id_array(to);
        if from != to {
            add_change(&mut changes, "blockedByIssueIds", from, to);
        }
    }
    if let Some((from, to)) = &relation_changes.label_ids {
        let from = canonical_id_array(from);
        let to = canonical_id_array(to);
        if from != to {
            add_change(&mut changes, "labelIds", from, to);
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn ignores_updated_at_and_unchanged_values() {
        let changes = build_issue_changes(
            &object(json!({"title":"same", "updatedAt":"a"})),
            &object(json!({"title":"same", "updatedAt":"b"})),
            &IssueRelationChanges::default(),
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn truncates_description_and_long_title_by_unicode_chars() {
        let long = "界".repeat(201);
        let changes = build_issue_changes(
            &object(json!({"description":"old", "title":"old"})),
            &object(json!({"description":long, "title":long})),
            &IssueRelationChanges::default(),
        );
        assert_eq!(
            changes["description"]["to"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            200
        );
        assert_eq!(
            changes["title"]["to"].as_str().unwrap().chars().count(),
            200
        );
    }

    #[test]
    fn preserves_short_scalar_changes() {
        let changes = build_issue_changes(
            &object(json!({"status":"open", "priority":1})),
            &object(json!({"status":"done", "priority":2})),
            &IssueRelationChanges::default(),
        );
        assert_eq!(changes["status"], json!({"from":"open", "to":"done"}));
        assert_eq!(changes["priority"], json!({"from":1, "to":2}));
    }

    #[test]
    fn canonicalizes_relation_arrays_before_comparing() {
        let changes = build_issue_changes(
            &Map::new(),
            &Map::new(),
            &IssueRelationChanges {
                blocked_by_issue_ids: Some((
                    vec!["b".into(), "a".into(), "a".into()],
                    vec!["a".into(), "b".into()],
                )),
                label_ids: Some((vec!["old".into()], vec!["new".into()])),
            },
        );
        assert!(!changes.contains_key("blockedByIssueIds"));
        assert_eq!(changes["labelIds"]["from"], json!(["old"]));
    }

    #[test]
    fn relation_changes_are_optional() {
        let changes =
            build_issue_changes(&Map::new(), &Map::new(), &IssueRelationChanges::default());
        assert!(changes.is_empty());
    }
}
