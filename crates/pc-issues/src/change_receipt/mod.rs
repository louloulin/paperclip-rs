//! Issue 业务子模块（原 `pc-issue-change-receipt` 已下沉到 `pc-issues::change_receipt`）。
//!
//! 对应 Node `server/src/services/issue-change-receipt.ts`。

use std::collections::{BTreeMap, BTreeSet};

/// Issue 变更条目 —— 与 Node `IssueChanges[K]` 1:1 对齐。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum IssueChange {
    /// 长文本字段：`{ from, to, updated: true }`
    LongText {
        from: Option<serde_json::Value>,
        to: Option<serde_json::Value>,
        updated: bool,
    },
    /// 普通字段：`{ from, to }`
    Short {
        from: Option<serde_json::Value>,
        to: Option<serde_json::Value>,
    },
}

/// Issue changes receipt —— 与 Node `IssueChanges` 1:1 对齐（动态键字符串 map）。
pub type IssueChanges = BTreeMap<String, IssueChange>;

/// 关系变更输入 —— 与 Node 入参 1:1 对齐。
#[derive(Debug, Clone, Default)]
pub struct RelationChanges {
    pub blocked_by_issue_ids: Option<IdArrayChange>,
    pub label_ids: Option<IdArrayChange>,
}

#[derive(Debug, Clone)]
pub struct IdArrayChange {
    pub from: Option<Vec<String>>,
    pub to: Option<Vec<String>>,
}

/// 长文本字符预算 —— 与 Node 常量一致。
pub const ISSUE_CHANGE_TEXT_BUDGET: usize = 200;

/// 截断长字符串（按 codepoint）。
///
/// 与 Node `truncateIssueChangeText` 1:1 对齐：
/// - 非 string → 原样返回
/// - string → 取前 200 个 codepoint
pub fn truncate_issue_change_text(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::String(s) = &value else {
        return value;
    };
    let truncated: String = s.chars().take(ISSUE_CHANGE_TEXT_BUDGET).collect();
    serde_json::Value::String(truncated)
}

/// 规范化 id 数组：去重 + 排序。
///
/// 与 Node `canonicalIdArray` 1:1 对齐：
/// - 不是数组 / 含非 string 元素 → 原样返回
/// - 数组 → `[...new Set(arr)].sort()`
pub fn canonical_id_array(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Array(arr) = &value else {
        return value;
    };
    if !arr
        .iter()
        .all(|v| matches!(v, serde_json::Value::String(_)))
    {
        return value;
    }
    let mut set: BTreeSet<String> = BTreeSet::new();
    for v in arr {
        if let serde_json::Value::String(s) = v {
            set.insert(s.clone());
        }
    }
    serde_json::Value::Array(set.into_iter().map(serde_json::Value::String).collect())
}

/// 深比较两个 `serde_json::Value` 是否相等。
///
/// 与 Node `isDeepStrictEqual` 1:1 对齐（语义上）。
pub fn is_deep_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    a == b
}

/// 从 `existing` / `updated` 构造 issue change receipt。
///
/// 与 Node `buildIssueChanges` 1:1 对齐。
pub fn build_issue_changes(
    existing: &serde_json::Value,
    updated: &serde_json::Value,
    relation_changes: RelationChanges,
) -> IssueChanges {
    let mut changes = IssueChanges::new();

    let existing_obj = existing.as_object();
    let updated_obj = updated.as_object();

    if let (Some(eo), Some(uo)) = (existing_obj, updated_obj) {
        let mut keys: BTreeSet<String> = BTreeSet::new();
        for k in eo.keys() {
            keys.insert(k.clone());
        }
        for k in uo.keys() {
            keys.insert(k.clone());
        }
        keys.remove("updatedAt");

        for key in keys {
            let from = eo.get(&key).cloned().unwrap_or(serde_json::Value::Null);
            let to = uo.get(&key).cloned().unwrap_or(serde_json::Value::Null);
            if is_deep_equal(&from, &to) {
                continue;
            }

            let from_str_len = from.as_str().map(|s| s.chars().count()).unwrap_or(0);
            let to_str_len = to.as_str().map(|s| s.chars().count()).unwrap_or(0);

            let is_long_text = key == "description"
                || (key == "title"
                    && (from_str_len > ISSUE_CHANGE_TEXT_BUDGET
                        || to_str_len > ISSUE_CHANGE_TEXT_BUDGET));

            if is_long_text {
                changes.insert(
                    key,
                    IssueChange::LongText {
                        from: Some(truncate_issue_change_text(from)),
                        to: Some(truncate_issue_change_text(to)),
                        updated: true,
                    },
                );
            } else {
                changes.insert(
                    key,
                    IssueChange::Short {
                        from: Some(from),
                        to: Some(to),
                    },
                );
            }
        }
    }

    // 关系变更
    if let Some(rc) = relation_changes.blocked_by_issue_ids {
        let from = canonical_id_array(serde_json::to_value(&rc.from).unwrap_or_default());
        let to = canonical_id_array(serde_json::to_value(&rc.to).unwrap_or_default());
        if !is_deep_equal(&from, &to) {
            changes.insert(
                "blockedByIssueIds".to_string(),
                IssueChange::Short {
                    from: Some(from),
                    to: Some(to),
                },
            );
        }
    }
    if let Some(rc) = relation_changes.label_ids {
        let from = canonical_id_array(serde_json::to_value(&rc.from).unwrap_or_default());
        let to = canonical_id_array(serde_json::to_value(&rc.to).unwrap_or_default());
        if !is_deep_equal(&from, &to) {
            changes.insert(
                "labelIds".to_string(),
                IssueChange::Short {
                    from: Some(from),
                    to: Some(to),
                },
            );
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r704_truncate_non_string_unchanged() {
        assert_eq!(truncate_issue_change_text(json!(42)), json!(42));
        assert_eq!(truncate_issue_change_text(json!(null)), json!(null));
        assert_eq!(truncate_issue_change_text(json!([1, 2])), json!([1, 2]));
    }

    #[test]
    fn r704_truncate_short_string_unchanged() {
        let s = "x".repeat(100);
        let v = json!(s.clone());
        assert_eq!(truncate_issue_change_text(v), json!(s));
    }

    #[test]
    fn r704_truncate_at_boundary() {
        let s = "x".repeat(ISSUE_CHANGE_TEXT_BUDGET);
        let v = json!(s.clone());
        assert_eq!(truncate_issue_change_text(v), json!(s));
    }

    #[test]
    fn r704_truncate_long_string() {
        let s: String = "a".repeat(500);
        let v = json!(s);
        let out = truncate_issue_change_text(v);
        let out_str = out.as_str().unwrap();
        assert_eq!(out_str.chars().count(), ISSUE_CHANGE_TEXT_BUDGET);
        assert!(out_str.chars().all(|c| c == 'a'));
    }

    #[test]
    fn r704_truncate_counts_codepoints_not_bytes() {
        // 中文每个 codepoint 占 3 bytes，但 chars() 按 codepoint 计数
        let s = "中".repeat(500);
        let v = json!(s);
        let out = truncate_issue_change_text(v);
        let out_str = out.as_str().unwrap();
        assert_eq!(out_str.chars().count(), ISSUE_CHANGE_TEXT_BUDGET);
    }

    #[test]
    fn r704_canonical_id_array_dedup_and_sort() {
        let v = json!(["c", "a", "b", "a"]);
        let out = canonical_id_array(v);
        assert_eq!(out, json!(["a", "b", "c"]));
    }

    #[test]
    fn r704_canonical_id_array_non_array_unchanged() {
        assert_eq!(canonical_id_array(json!("string")), json!("string"));
        assert_eq!(canonical_id_array(json!(42)), json!(42));
    }

    #[test]
    fn r704_canonical_id_array_mixed_types_unchanged() {
        let v = json!(["a", 1, "b"]);
        assert_eq!(canonical_id_array(v.clone()), v);
    }

    #[test]
    fn r704_canonical_id_array_empty() {
        assert_eq!(canonical_id_array(json!([])), json!([]));
    }

    #[test]
    fn r704_build_skips_unchanged_keys() {
        let existing = json!({"title": "A", "status": "open"});
        let updated = json!({"title": "A", "status": "open"});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        assert!(changes.is_empty());
    }

    #[test]
    fn r704_build_skips_updated_at() {
        let existing = json!({"title": "A", "updatedAt": "2024-01-01"});
        let updated = json!({"title": "A", "updatedAt": "2024-01-02"});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        assert!(changes.is_empty());
    }

    #[test]
    fn r704_build_includes_changed_keys() {
        let existing = json!({"title": "A", "status": "open"});
        let updated = json!({"title": "B", "status": "open"});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        assert_eq!(changes.len(), 1);
        assert!(changes.contains_key("title"));
    }

    #[test]
    fn r704_build_short_text_change() {
        let existing = json!({"title": "old"});
        let updated = json!({"title": "new"});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        match changes.get("title").unwrap() {
            IssueChange::Short { from, to } => {
                assert_eq!(from.as_ref().unwrap(), "old");
                assert_eq!(to.as_ref().unwrap(), "new");
            }
            _ => panic!("expected Short"),
        }
    }

    #[test]
    fn r704_build_description_marks_long_text() {
        let long: String = "x".repeat(300);
        let existing = json!({"description": ""});
        let updated = json!({"description": long});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        match changes.get("description").unwrap() {
            IssueChange::LongText { updated, .. } => {
                assert!(*updated);
            }
            _ => panic!("expected LongText"),
        }
    }

    #[test]
    fn r704_build_title_marks_long_text_when_either_side_exceeds() {
        let long: String = "y".repeat(300);
        let existing = json!({"title": long});
        let updated = json!({"title": "short"});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        assert!(matches!(
            changes.get("title").unwrap(),
            IssueChange::LongText { .. }
        ));
    }

    #[test]
    fn r704_build_relation_blocked_by_issue_ids() {
        let existing = json!({});
        let updated = json!({});
        let rel = RelationChanges {
            blocked_by_issue_ids: Some(IdArrayChange {
                from: Some(vec!["b".into(), "a".into()]),
                to: Some(vec!["a".into(), "c".into()]),
            }),
            label_ids: None,
        };
        let changes = build_issue_changes(&existing, &updated, rel);
        match changes.get("blockedByIssueIds").unwrap() {
            IssueChange::Short { from, to } => {
                assert_eq!(from.as_ref().unwrap(), &json!(["a", "b"]));
                assert_eq!(to.as_ref().unwrap(), &json!(["a", "c"]));
            }
            _ => panic!("expected Short"),
        }
    }

    #[test]
    fn r704_build_relation_label_ids_dedup() {
        let existing = json!({});
        let updated = json!({});
        let rel = RelationChanges {
            blocked_by_issue_ids: None,
            label_ids: Some(IdArrayChange {
                from: Some(vec!["a".into(), "b".into()]),
                to: Some(vec!["a".into(), "b".into(), "a".into()]),
            }),
        };
        let changes = build_issue_changes(&existing, &updated, rel);
        // from 和 to 规范化后都是 ["a","b"] → 相等 → 不加入
        assert!(changes.is_empty());
    }

    #[test]
    fn r704_build_relation_with_null_arrays() {
        let existing = json!({});
        let updated = json!({});
        let rel = RelationChanges {
            blocked_by_issue_ids: Some(IdArrayChange {
                from: None,
                to: Some(vec!["a".into()]),
            }),
            label_ids: None,
        };
        let changes = build_issue_changes(&existing, &updated, rel);
        assert!(changes.contains_key("blockedByIssueIds"));
    }

    #[test]
    fn r704_build_includes_added_keys() {
        // key 只在 updated 中
        let existing = json!({"a": 1});
        let updated = json!({"a": 1, "b": 2});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        assert!(changes.contains_key("b"));
    }

    #[test]
    fn r704_build_includes_removed_keys() {
        // key 只在 existing 中
        let existing = json!({"a": 1, "b": 2});
        let updated = json!({"a": 1});
        let changes = build_issue_changes(&existing, &updated, RelationChanges::default());
        assert!(changes.contains_key("b"));
    }
}
