//! Runtime skill version selection 映射（对齐 Node `server/src/services/runtime-skill-selections.ts`，7 行）。
//!
//! 单一职责：把 `Array<{ key, versionId }>` 转换为 `Map<key, versionId>`，
//! 当 `versionPinsEnabled = false` 时所有 `versionId` 强制设为 `null`（解除版本钉）。
//!
//! 不持有任何状态；不依赖 IO。

use std::collections::HashMap;

/// 输入条目（与 Node `{ key: string; versionId: string | null }` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillVersionSelectionEntry {
    pub key: String,
    pub version_id: Option<String>,
}

impl SkillVersionSelectionEntry {
    pub fn new(key: impl Into<String>, version_id: Option<String>) -> Self {
        Self {
            key: key.into(),
            version_id,
        }
    }
}

/// `skillVersionSelectionMap` 选项（与 Node `options` 1:1 对齐）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SkillVersionSelectionOptions {
    pub version_pins_enabled: Option<bool>,
}

impl SkillVersionSelectionOptions {
    pub fn new(version_pins_enabled: bool) -> Self {
        Self {
            version_pins_enabled: Some(version_pins_enabled),
        }
    }
}

/// 构造 `key -> versionId` 映射（与 Node `skillVersionSelectionMap` 1:1 对齐）。
///
/// 行为：
/// - `versionPinsEnabled` 缺省为 `true`（与 Node `?? true` 1:1 对齐）
/// - `true` → `Map(key -> versionId)` 保留原始 `version_id`
/// - `false` → `Map(key -> null)` 强制解除版本钉
#[must_use]
pub fn skill_version_selection_map(
    entries: &[SkillVersionSelectionEntry],
    options: SkillVersionSelectionOptions,
) -> HashMap<String, Option<String>> {
    let version_pins_enabled = options.version_pins_enabled.unwrap_or(true);
    entries
        .iter()
        .map(|e| {
            let value = if version_pins_enabled {
                e.version_id.clone()
            } else {
                None
            };
            (e.key.clone(), value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<SkillVersionSelectionEntry> {
        vec![
            SkillVersionSelectionEntry::new("skill-a", Some("v1".to_string())),
            SkillVersionSelectionEntry::new("skill-b", Some("v2".to_string())),
            SkillVersionSelectionEntry::new("skill-c", None),
        ]
    }

    #[test]
    fn default_options_preserve_version_pins() {
        let result =
            skill_version_selection_map(&entries(), SkillVersionSelectionOptions::default());
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("skill-a"), Some(&Some("v1".to_string())));
        assert_eq!(result.get("skill-b"), Some(&Some("v2".to_string())));
        assert_eq!(result.get("skill-c"), Some(&None));
    }

    #[test]
    fn explicit_version_pins_enabled_preserves_pins() {
        let opts = SkillVersionSelectionOptions::new(true);
        let result = skill_version_selection_map(&entries(), opts);
        assert_eq!(result.get("skill-a"), Some(&Some("v1".to_string())));
        assert_eq!(result.get("skill-b"), Some(&Some("v2".to_string())));
    }

    #[test]
    fn version_pins_disabled_clears_all_pins() {
        let opts = SkillVersionSelectionOptions::new(false);
        let result = skill_version_selection_map(&entries(), opts);
        assert_eq!(result.get("skill-a"), Some(&None));
        assert_eq!(result.get("skill-b"), Some(&None));
        assert_eq!(result.get("skill-c"), Some(&None));
    }

    #[test]
    fn empty_entries_returns_empty_map() {
        let result = skill_version_selection_map(&[], SkillVersionSelectionOptions::default());
        assert!(result.is_empty());
    }

    #[test]
    fn duplicate_keys_last_wins() {
        // HashMap::from_iter behavior: later entries overwrite earlier ones for the same key.
        let entries = vec![
            SkillVersionSelectionEntry::new("dup", Some("v1".to_string())),
            SkillVersionSelectionEntry::new("dup", Some("v2".to_string())),
        ];
        let result = skill_version_selection_map(&entries, SkillVersionSelectionOptions::default());
        assert_eq!(result.get("dup"), Some(&Some("v2".to_string())));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn entry_new_accepts_str() {
        let e = SkillVersionSelectionEntry::new("k", Some("v".to_string()));
        assert_eq!(e.key, "k");
        assert_eq!(e.version_id, Some("v".to_string()));
    }

    #[test]
    fn entry_new_accepts_none_version() {
        let e = SkillVersionSelectionEntry::new("k", None);
        assert_eq!(e.key, "k");
        assert_eq!(e.version_id, None);
    }
}
