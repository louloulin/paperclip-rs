#![forbid(unsafe_code)]
//! Skill version pin selection map（原 `pc-runtime-skill-selections` 已下沉）。
//!
//! 对应 Node `server/src/services/runtime-skill-selections.ts`（7 行，纯函数）。
//!
//! 设计目标：1:1 复刻 `skillVersionSelectionMap` 的语义——
//! - 当 `versionPinsEnabled=true`（默认）时，返回 `entry.versionId`；
//! - 当 `versionPinsEnabled=false` 时，返回 `null`（即不固定版本）。

use std::collections::HashMap;

/// 单条 skill selection 输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSelectionEntry {
    pub key: String,
    pub version_id: Option<String>,
}

/// `skillVersionSelectionMap` 的可选参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillVersionSelectionOptions {
    pub version_pins_enabled: bool,
}

impl Default for SkillVersionSelectionOptions {
    fn default() -> Self {
        Self { version_pins_enabled: true }
    }
}

/// 构造 `key → version_id` 选择映射。
///
/// 与 Node `skillVersionSelectionMap` 1:1 对齐：
/// - `versionPinsEnabled=true`（默认）→ 使用 `entry.versionId`
/// - `versionPinsEnabled=false` → 全部置 `None`
///
/// 返回 `HashMap<String, Option<String>>` —— Node 中是 `Map`，
/// Rust 端用 `HashMap` 表达相同的语义。
pub fn skill_version_selection_map(
    entries: &[SkillSelectionEntry],
    options: Option<SkillVersionSelectionOptions>,
) -> HashMap<String, Option<String>> {
    let version_pins_enabled = options
        .map(|o| o.version_pins_enabled)
        .unwrap_or(true);
    entries
        .iter()
        .map(|entry| {
            let v = if version_pins_enabled { entry.version_id.clone() } else { None };
            (entry.key.clone(), v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<SkillSelectionEntry> {
        vec![
            SkillSelectionEntry {
                key: "skill-a".into(),
                version_id: Some("v1.0.0".into()),
            },
            SkillSelectionEntry {
                key: "skill-b".into(),
                version_id: None,
            },
            SkillSelectionEntry {
                key: "skill-c".into(),
                version_id: Some("v3.2.1".into()),
            },
        ]
    }

    #[test]
    fn r689_default_options_pins_versions() {
        let m = skill_version_selection_map(&entries(), None);
        assert_eq!(m.len(), 3);
        assert_eq!(m.get("skill-a"), Some(&Some("v1.0.0".into())));
        assert_eq!(m.get("skill-b"), Some(&None));
        assert_eq!(m.get("skill-c"), Some(&Some("v3.2.1".into())));
    }

    #[test]
    fn r689_explicit_pins_enabled_true() {
        let m = skill_version_selection_map(
            &entries(),
            Some(SkillVersionSelectionOptions { version_pins_enabled: true }),
        );
        assert_eq!(m.get("skill-a"), Some(&Some("v1.0.0".into())));
        assert_eq!(m.get("skill-b"), Some(&None));
    }

    #[test]
    fn r689_explicit_pins_disabled_returns_none() {
        let m = skill_version_selection_map(
            &entries(),
            Some(SkillVersionSelectionOptions { version_pins_enabled: false }),
        );
        // 所有版本都被清空成 None
        assert_eq!(m.get("skill-a"), Some(&None));
        assert_eq!(m.get("skill-b"), Some(&None));
        assert_eq!(m.get("skill-c"), Some(&None));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn r689_empty_input_returns_empty_map() {
        let m = skill_version_selection_map(&[], None);
        assert!(m.is_empty());
    }

    #[test]
    fn r689_duplicate_keys_last_wins() {
        // Node `Map` 语义：后插入的覆盖前面。这里保持一致。
        let input = vec![
            SkillSelectionEntry { key: "dup".into(), version_id: Some("v1".into()) },
            SkillSelectionEntry { key: "dup".into(), version_id: Some("v2".into()) },
        ];
        let m = skill_version_selection_map(&input, None);
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("dup"), Some(&Some("v2".into())));
    }

    #[test]
    fn r689_default_struct_value() {
        let opts = SkillVersionSelectionOptions::default();
        assert!(opts.version_pins_enabled);
    }
}
