#![forbid(unsafe_code)]
//! `pc-sidebar-preferences` —— sidebar 顺序偏好规范化。
//!
//! 对应 Node `server/src/services/sidebar-preferences.ts`（97 行）。
//!
//! 设计目标：1:1 复刻
//! - `normalizeOrderedIds(value)` —— 任意值 → 去重 / trim / 过滤非字符串的 id 列表
//! - `toPreference(orderedIds, updatedAt)` —— 包装成 `SidebarOrderPreference`
//!
//! DB 部分（`sidebarPreferenceService(db)`）由上层接入 pc-repos。

use serde::{Deserialize, Serialize};

/// Sidebar order preference —— 与 Node `SidebarOrderPreference` 1:1 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarOrderPreference {
    pub ordered_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 规范化 id 列表：去重、trim、过滤非字符串。
///
/// 与 Node `normalizeOrderedIds` 1:1 对齐：
/// - 非数组 → `[]`
/// - 数组：保留字符串、trim、跳过空字符串、跳过重复（保留首次出现顺序）
pub fn normalize_ordered_ids(value: &serde_json::Value) -> Vec<String> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut ordered = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in arr {
        let Some(s) = item.as_str() else {
            continue;
        };
        let trimmed = s.trim();
        if trimmed.is_empty() || seen.contains(trimmed) {
            continue;
        }
        seen.insert(trimmed.to_string());
        ordered.push(trimmed.to_string());
    }
    ordered
}

/// 包装成 `SidebarOrderPreference`。
///
/// 与 Node `toPreference` 1:1 对齐。
pub fn to_preference(
    ordered_ids: &serde_json::Value,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
) -> SidebarOrderPreference {
    SidebarOrderPreference {
        ordered_ids: normalize_ordered_ids(ordered_ids),
        updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r708_normalize_non_array_returns_empty() {
        assert!(normalize_ordered_ids(&json!("string")).is_empty());
        assert!(normalize_ordered_ids(&json!(42)).is_empty());
        assert!(normalize_ordered_ids(&json!({"k": "v"})).is_empty());
        assert!(normalize_ordered_ids(&json!(null)).is_empty());
    }

    #[test]
    fn r708_normalize_basic_strings() {
        let r = normalize_ordered_ids(&json!(["a", "b", "c"]));
        assert_eq!(r, vec!["a", "b", "c"]);
    }

    #[test]
    fn r708_normalize_filters_non_strings() {
        let r = normalize_ordered_ids(&json!(["a", 42, "b", null, true, "c"]));
        assert_eq!(r, vec!["a", "b", "c"]);
    }

    #[test]
    fn r708_normalize_dedup_preserves_first() {
        let r = normalize_ordered_ids(&json!(["a", "b", "a", "c", "b"]));
        assert_eq!(r, vec!["a", "b", "c"]);
    }

    #[test]
    fn r708_normalize_trims_whitespace() {
        let r = normalize_ordered_ids(&json!(["  a  ", "b\t", "\nc"]));
        assert_eq!(r, vec!["a", "b", "c"]);
    }

    #[test]
    fn r708_normalize_skips_empty() {
        let r = normalize_ordered_ids(&json!(["a", "", "   ", "b"]));
        assert_eq!(r, vec!["a", "b"]);
    }

    #[test]
    fn r708_normalize_empty_array() {
        let r = normalize_ordered_ids(&json!([]));
        assert!(r.is_empty());
    }

    #[test]
    fn r708_normalize_trim_then_dedup() {
        // "  a  " 和 "a" 视为相同 → 去重
        let r = normalize_ordered_ids(&json!(["  a  ", "a"]));
        assert_eq!(r, vec!["a"]);
    }

    #[test]
    fn r708_to_preference_normalizes() {
        let p = to_preference(&json!(["a", "b", "a"]), None);
        assert_eq!(p.ordered_ids, vec!["a", "b"]);
        assert!(p.updated_at.is_none());
    }

    #[test]
    fn r708_to_preference_empty() {
        let p = to_preference(&json!("not-array"), None);
        assert!(p.ordered_ids.is_empty());
    }

    #[test]
    fn r708_serialization_camel_case() {
        let p = SidebarOrderPreference {
            ordered_ids: vec!["a".into()],
            updated_at: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["orderedIds"], json!(["a"]));
        assert!(v.get("updatedAt").is_none());
    }
}
