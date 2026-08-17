#![forbid(unsafe_code)]

//! Issue visibility reason / stats pure helpers — enum 双向转换 + stats 聚合.
//!
//! R739: 零依赖 helpers for IssueVisibilityReason and VisibilityStats.

use std::collections::HashMap;

/// Issue visibility 不通过的原因（mirror visibility::types::IssueVisibilityReason）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VisibilityReason {
    Visible,
    HiddenAt,
    HasHarnessKind,
}

impl VisibilityReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::HiddenAt => "hidden_at",
            Self::HasHarnessKind => "has_harness_kind",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        let normalized = s.trim().to_lowercase();
        match normalized.as_str() {
            "visible" => Some(Self::Visible),
            "hidden_at" | "hiddenat" => Some(Self::HiddenAt),
            "has_harness_kind" | "hasharnesskind" => Some(Self::HasHarnessKind),
            _ => None,
        }
    }

    /// 是否阻碍可见性。
    pub fn blocks_visibility(self) -> bool {
        !matches!(self, Self::Visible)
    }

    /// 是否为 hidden 类（hidden_at）。
    pub fn is_hidden(self) -> bool {
        matches!(self, Self::HiddenAt)
    }

    /// 是否为 harness 类（has_harness_kind）。
    pub fn is_harness(self) -> bool {
        matches!(self, Self::HasHarnessKind)
    }
}

/// 单条 visibility 记录（pure 简化版）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibilityEntry {
    pub id: String,
    pub reason: VisibilityReason,
}

/// Visibility 统计聚合。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisibilityAggregate {
    pub total: usize,
    pub visible: usize,
    pub hidden: usize,
    pub harness: usize,
}

/// 聚合 visibility entries → stats。
///
/// 1:1 对齐 Node classify / count_by_reason。
pub fn aggregate_visibility(entries: &[VisibilityEntry]) -> VisibilityAggregate {
    let mut agg = VisibilityAggregate {
        total: entries.len(),
        ..Default::default()
    };
    for e in entries {
        match e.reason {
            VisibilityReason::Visible => agg.visible += 1,
            VisibilityReason::HiddenAt => agg.hidden += 1,
            VisibilityReason::HasHarnessKind => agg.harness += 1,
        }
    }
    agg
}

/// 按 reason 分组计数。
pub fn count_by_reason(entries: &[VisibilityEntry]) -> HashMap<VisibilityReason, usize> {
    let mut out: HashMap<VisibilityReason, usize> = HashMap::new();
    for e in entries {
        *out.entry(e.reason).or_insert(0) += 1;
    }
    out
}

/// 判断 aggregate 是否全部 visible。
pub fn is_all_visible(agg: &VisibilityAggregate) -> bool {
    agg.total > 0 && agg.visible == agg.total
}

/// 计算 visible ratio (0.0 ~ 1.0)。
pub fn visible_ratio(agg: &VisibilityAggregate) -> f64 {
    if agg.total == 0 {
        0.0
    } else {
        agg.visible as f64 / agg.total as f64
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn reason_as_str() {
        assert_eq!(VisibilityReason::Visible.as_str(), "visible");
        assert_eq!(VisibilityReason::HiddenAt.as_str(), "hidden_at");
        assert_eq!(VisibilityReason::HasHarnessKind.as_str(), "has_harness_kind");
    }

    #[test]
    fn reason_from_str_known() {
        assert_eq!(VisibilityReason::from_str("visible"), Some(VisibilityReason::Visible));
        assert_eq!(VisibilityReason::from_str("hidden_at"), Some(VisibilityReason::HiddenAt));
        assert_eq!(VisibilityReason::from_str("has_harness_kind"), Some(VisibilityReason::HasHarnessKind));
    }

    #[test]
    fn reason_from_str_case_insensitive() {
        assert_eq!(VisibilityReason::from_str("VISIBLE"), Some(VisibilityReason::Visible));
        assert_eq!(VisibilityReason::from_str("  Hidden_At  "), Some(VisibilityReason::HiddenAt));
    }

    #[test]
    fn reason_from_str_camelcase_legacy() {
        assert_eq!(VisibilityReason::from_str("hiddenat"), Some(VisibilityReason::HiddenAt));
        assert_eq!(VisibilityReason::from_str("hasharnesskind"), Some(VisibilityReason::HasHarnessKind));
    }

    #[test]
    fn reason_from_str_unknown() {
        assert_eq!(VisibilityReason::from_str("foo"), None);
        assert_eq!(VisibilityReason::from_str(""), None);
    }

    #[test]
    fn blocks_visibility() {
        assert!(!VisibilityReason::Visible.blocks_visibility());
        assert!(VisibilityReason::HiddenAt.blocks_visibility());
        assert!(VisibilityReason::HasHarnessKind.blocks_visibility());
    }

    #[test]
    fn is_hidden_or_harness() {
        assert!(VisibilityReason::HiddenAt.is_hidden());
        assert!(!VisibilityReason::Visible.is_hidden());
        assert!(VisibilityReason::HasHarnessKind.is_harness());
        assert!(!VisibilityReason::Visible.is_harness());
    }

    #[test]
    fn aggregate_empty() {
        let agg = aggregate_visibility(&[]);
        assert_eq!(agg, VisibilityAggregate::default());
    }

    #[test]
    fn aggregate_mixed() {
        let entries = vec![
            VisibilityEntry { id: "1".into(), reason: VisibilityReason::Visible },
            VisibilityEntry { id: "2".into(), reason: VisibilityReason::HiddenAt },
            VisibilityEntry { id: "3".into(), reason: VisibilityReason::HasHarnessKind },
            VisibilityEntry { id: "4".into(), reason: VisibilityReason::Visible },
        ];
        let agg = aggregate_visibility(&entries);
        assert_eq!(agg.total, 4);
        assert_eq!(agg.visible, 2);
        assert_eq!(agg.hidden, 1);
        assert_eq!(agg.harness, 1);
    }

    #[test]
    fn count_by_reason_basic() {
        let entries = vec![
            VisibilityEntry { id: "1".into(), reason: VisibilityReason::Visible },
            VisibilityEntry { id: "2".into(), reason: VisibilityReason::Visible },
            VisibilityEntry { id: "3".into(), reason: VisibilityReason::HiddenAt },
        ];
        let counts = count_by_reason(&entries);
        assert_eq!(counts.get(&VisibilityReason::Visible), Some(&2));
        assert_eq!(counts.get(&VisibilityReason::HiddenAt), Some(&1));
        assert_eq!(counts.get(&VisibilityReason::HasHarnessKind), None);
    }

    #[test]
    fn is_all_visible_true() {
        let agg = VisibilityAggregate { total: 3, visible: 3, hidden: 0, harness: 0 };
        assert!(is_all_visible(&agg));
    }

    #[test]
    fn is_all_visible_false_with_hidden() {
        let agg = VisibilityAggregate { total: 3, visible: 2, hidden: 1, harness: 0 };
        assert!(!is_all_visible(&agg));
    }

    #[test]
    fn is_all_visible_false_when_empty() {
        assert!(!is_all_visible(&VisibilityAggregate::default()));
    }

    #[test]
    fn visible_ratio_normal() {
        let agg = VisibilityAggregate { total: 4, visible: 3, hidden: 1, harness: 0 };
        assert!((visible_ratio(&agg) - 0.75).abs() < 0.001);
    }

    #[test]
    fn visible_ratio_empty() {
        assert_eq!(visible_ratio(&VisibilityAggregate::default()), 0.0);
    }

    #[test]
    fn visible_ratio_all() {
        let agg = VisibilityAggregate { total: 5, visible: 5, hidden: 0, harness: 0 };
        assert!((visible_ratio(&agg) - 1.0).abs() < 0.001);
    }
}
