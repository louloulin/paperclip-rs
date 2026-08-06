//! `buildIssueChanges` 纯函数 —— issue 变更 diff 用于 activity log。
//!
//! 对齐 Node `services/issue-change-receipt.ts` 的 `buildIssueChanges`：
//! 比较 `existing` 与 `updated` issue 状态，返回字段级 diff。
//!
//! 关键规则（与 Node 1:1）：
//! - 忽略 `updatedAt`（每次 update 都会变）
//! - 深比较（`isDeepStrictEqual`）：用 `serde_json::Value` 等价
//! - description 字段总是 truncate 到 200 字符
//! - title 字段：任一长度 > 200 → truncate + 标记 `updated: true`
//! - relation changes：去重 + 排序后再比较（避免顺序差异触发 false positive）
//!
//! 设计：
//! - 纯函数：仅依赖输入，无 DB I/O
//! - 单一职责：只做 diff，不做 activity log 写入（后者由 caller 负责）
//! - 高内聚：所有 diff 规则集中在一个函数
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Long-text truncation 阈值（与 Node `ISSUE_CHANGE_TEXT_BUDGET` 对齐）。
pub const ISSUE_CHANGE_TEXT_BUDGET: usize = 200;

/// Relation changes 输入。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationChangeInput {
    pub blocked_by_issue_ids: Option<IdArrayChange>,
    pub label_ids: Option<IdArrayChange>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdArrayChange {
    pub from: Vec<String>,
    pub to: Vec<String>,
}

/// 单个字段的 change。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub from: Value,
    pub to: Value,
    /// 仅当 long text 被 truncate 时为 true。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub updated: bool,
}

/// IssueChanges 输出。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IssueChanges {
    #[serde(flatten)]
    pub fields: std::collections::BTreeMap<String, FieldChange>,
}

impl IssueChanges {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// 比较 existing vs updated，返回字段级 diff。
///
/// 与 Node `buildIssueChanges` 对齐：
/// - 忽略 `updatedAt`
/// - description 总是 truncate + 标记 `updated: true`
/// - title 任一长度 > 200 → truncate + 标记 `updated: true`
/// - relation changes：去重 + 排序后再比较
pub fn build_issue_changes(
    existing: &serde_json::Map<String, Value>,
    updated: &serde_json::Map<String, Value>,
    relation_changes: RelationChangeInput,
) -> IssueChanges {
    let mut changes = IssueChanges::default();
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    for k in existing.keys() {
        keys.insert(k.as_str());
    }
    for k in updated.keys() {
        keys.insert(k.as_str());
    }
    keys.remove("updatedAt");

    for key in keys {
        let from = existing.get(key).cloned().unwrap_or(Value::Null);
        let to = updated.get(key).cloned().unwrap_or(Value::Null);
        if deep_equal(&from, &to) {
            continue;
        }

        let long_text = is_long_text(key, &from, &to);
        let field_change = if long_text {
            FieldChange {
                from: truncate_text(&from),
                to: truncate_text(&to),
                updated: true,
            }
        } else {
            FieldChange {
                from,
                to,
                updated: false,
            }
        };
        changes.fields.insert(key.to_string(), field_change);
    }

    // 处理 relation changes
    if let Some(blocked_change) = relation_changes.blocked_by_issue_ids {
        let from = canonical_id_array(&blocked_change.from);
        let to = canonical_id_array(&blocked_change.to);
        if !deep_equal(&from, &to) {
            changes.fields.insert(
                "blockedByIssueIds".to_string(),
                FieldChange {
                    from,
                    to,
                    updated: false,
                },
            );
        }
    }
    if let Some(label_change) = relation_changes.label_ids {
        let from = canonical_id_array(&label_change.from);
        let to = canonical_id_array(&label_change.to);
        if !deep_equal(&from, &to) {
            changes.fields.insert(
                "labelIds".to_string(),
                FieldChange {
                    from,
                    to,
                    updated: false,
                },
            );
        }
    }

    changes
}

// ============================================================================
// Helpers (private)
// ============================================================================

/// 深比较：与 Node `isDeepStrictEqual` 等价。
///
/// 简化：使用 serde_json::Value 的 PartialEq（已实现递归比较）。
/// 对 Array/Object 都递归处理。
fn deep_equal(a: &Value, b: &Value) -> bool {
    a == b
}

/// Truncate text field 到 ISSUE_CHANGE_TEXT_BUDGET 字符。
fn truncate_text(value: &Value) -> Value {
    if let Some(s) = value.as_str() {
        let truncated: String = s.chars().take(ISSUE_CHANGE_TEXT_BUDGET).collect();
        Value::String(truncated)
    } else {
        value.clone()
    }
}

/// 判定字段是否需要 long-text 处理。
///
/// 与 Node 规则对齐：
/// - key == "description" → true
/// - key == "title" 且 from 或 to 任一长度 > 200 → true
fn is_long_text(key: &str, from: &Value, to: &Value) -> bool {
    if key == "description" {
        return true;
    }
    if key == "title" {
        let from_len = from.as_str().map(|s| s.chars().count()).unwrap_or(0);
        let to_len = to.as_str().map(|s| s.chars().count()).unwrap_or(0);
        return from_len > ISSUE_CHANGE_TEXT_BUDGET || to_len > ISSUE_CHANGE_TEXT_BUDGET;
    }
    false
}

/// Canonical id array：去重 + 排序，与 Node `canonicalIdArray` 对齐。
fn canonical_id_array(value: &[String]) -> Value {
    let mut unique: BTreeSet<&String> = BTreeSet::new();
    for v in value {
        unique.insert(v);
    }
    let sorted: Vec<String> = unique.into_iter().cloned().collect();
    Value::Array(sorted.into_iter().map(Value::String).collect())
}
