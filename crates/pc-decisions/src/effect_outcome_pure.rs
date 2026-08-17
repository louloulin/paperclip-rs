#![forbid(unsafe_code)]

//! Decision effect execution outcome aggregation — 1:1 port of
//! paperclip/server/src/services/decisions.ts::aggregateExecutionOutcomes.
//!
//! R737: 零 DB pure logic（输入 Vec<EffectExecutionStatus> → 输出 success_count, total, status_label）。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectExecutionStatus {
    Executed,
    Failed,
    Skipped,
    Pending,
}

impl EffectExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Pending => "pending",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "executed" => Some(Self::Executed),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            "pending" => Some(Self::Pending),
            _ => None,
        }
    }

    /// 是否算入 successful 计数。
    pub fn is_successful(self) -> bool {
        matches!(self, Self::Executed)
    }
}

/// 聚合 outcomes，返回 (successful, total, status_label)。
///
/// status_label 逻辑（与 Node aggregateExecutionOutcomes 1:1）：
/// - total == 0 → "succeeded"
/// - successful == total → "succeeded"
/// - successful == 0 → "failed"
/// - 部分 → "partial"
pub fn aggregate_outcomes(rows: &[EffectExecutionStatus]) -> (usize, usize, String) {
    let total = rows.len();
    let successful = rows.iter().filter(|r| r.is_successful()).count();
    let status = if total == 0 {
        "succeeded".to_string()
    } else if successful == total {
        "succeeded".to_string()
    } else if successful == 0 {
        "failed".to_string()
    } else {
        "partial".to_string()
    };
    (successful, total, status)
}

/// 判断聚合 status 是否为最终成功。
pub fn is_final_success(status_label: &str) -> bool {
    status_label == "succeeded"
}

/// 判断聚合 status 是否为部分成功。
pub fn is_partial_success(status_label: &str) -> bool {
    status_label == "partial"
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn aggregate_empty_returns_succeeded() {
        let rows: Vec<EffectExecutionStatus> = vec![];
        let (s, t, status) = aggregate_outcomes(&rows);
        assert_eq!((s, t), (0, 0));
        assert_eq!(status, "succeeded");
    }

    #[test]
    fn aggregate_all_executed_succeeded() {
        let rows = vec![EffectExecutionStatus::Executed; 3];
        let (s, t, status) = aggregate_outcomes(&rows);
        assert_eq!((s, t), (3, 3));
        assert_eq!(status, "succeeded");
    }

    #[test]
    fn aggregate_all_failed_failed() {
        let rows = vec![EffectExecutionStatus::Failed; 2];
        let (s, t, status) = aggregate_outcomes(&rows);
        assert_eq!((s, t), (0, 2));
        assert_eq!(status, "failed");
    }

    #[test]
    fn aggregate_partial_returns_partial() {
        let rows = vec![
            EffectExecutionStatus::Executed,
            EffectExecutionStatus::Failed,
            EffectExecutionStatus::Executed,
        ];
        let (s, t, status) = aggregate_outcomes(&rows);
        assert_eq!((s, t), (2, 3));
        assert_eq!(status, "partial");
    }

    #[test]
    fn aggregate_skipped_not_counted_successful() {
        let rows = vec![EffectExecutionStatus::Skipped];
        let (s, t, status) = aggregate_outcomes(&rows);
        assert_eq!((s, t), (0, 1));
        assert_eq!(status, "failed");
    }

    #[test]
    fn aggregate_pending_not_counted_successful() {
        let rows = vec![EffectExecutionStatus::Pending];
        let (s, t, status) = aggregate_outcomes(&rows);
        assert_eq!((s, t), (0, 1));
        assert_eq!(status, "failed");
    }

    #[test]
    fn aggregate_mixed_with_skipped_partial() {
        let rows = vec![
            EffectExecutionStatus::Executed,
            EffectExecutionStatus::Skipped,
            EffectExecutionStatus::Failed,
        ];
        let (s, t, status) = aggregate_outcomes(&rows);
        assert_eq!((s, t), (1, 3));
        assert_eq!(status, "partial");
    }

    #[test]
    fn status_as_str() {
        assert_eq!(EffectExecutionStatus::Executed.as_str(), "executed");
        assert_eq!(EffectExecutionStatus::Failed.as_str(), "failed");
        assert_eq!(EffectExecutionStatus::Skipped.as_str(), "skipped");
        assert_eq!(EffectExecutionStatus::Pending.as_str(), "pending");
    }

    #[test]
    fn status_from_str_round_trip() {
        for s in [
            EffectExecutionStatus::Executed,
            EffectExecutionStatus::Failed,
            EffectExecutionStatus::Skipped,
            EffectExecutionStatus::Pending,
        ] {
            assert_eq!(EffectExecutionStatus::from_str(s.as_str()), Some(s));
        }
    }

    #[test]
    fn status_from_str_case_insensitive() {
        assert_eq!(EffectExecutionStatus::from_str("EXECUTED"), Some(EffectExecutionStatus::Executed));
        assert_eq!(EffectExecutionStatus::from_str(" Failed "), Some(EffectExecutionStatus::Failed));
    }

    #[test]
    fn status_from_str_unknown() {
        assert_eq!(EffectExecutionStatus::from_str("unknown"), None);
        assert_eq!(EffectExecutionStatus::from_str(""), None);
    }

    #[test]
    fn is_final_success_true() {
        assert!(is_final_success("succeeded"));
        assert!(!is_final_success("failed"));
        assert!(!is_final_success("partial"));
    }

    #[test]
    fn is_partial_success_true() {
        assert!(is_partial_success("partial"));
        assert!(!is_partial_success("succeeded"));
        assert!(!is_partial_success("failed"));
    }

    #[test]
    fn status_is_successful() {
        assert!(EffectExecutionStatus::Executed.is_successful());
        assert!(!EffectExecutionStatus::Failed.is_successful());
        assert!(!EffectExecutionStatus::Skipped.is_successful());
        assert!(!EffectExecutionStatus::Pending.is_successful());
    }
}
