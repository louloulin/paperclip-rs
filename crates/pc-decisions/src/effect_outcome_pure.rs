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

    // ---- Round 760: pc-decisions effect_outcome_pure 集成测试 ----

    /// aggregate_outcomes: 空数组 -> (0, 0, "succeeded")，符合 Node parity。
    #[test]
    fn r760_aggregate_outcomes_empty() {
        let (success, total, status) = aggregate_outcomes(&[]);
        assert_eq!(success, 0);
        assert_eq!(total, 0);
        assert_eq!(status, "succeeded");
    }

    /// aggregate_outcomes: 全部 executed -> succeeded。
    #[test]
    fn r760_aggregate_outcomes_all_executed() {
        let rows = vec![EffectExecutionStatus::Executed, EffectExecutionStatus::Executed, EffectExecutionStatus::Executed];
        let (success, total, status) = aggregate_outcomes(&rows);
        assert_eq!(success, 3);
        assert_eq!(total, 3);
        assert_eq!(status, "succeeded");
        assert!(is_final_success(&status));
        assert!(!is_partial_success(&status));
    }

    /// aggregate_outcomes: 全部 failed -> failed。
    #[test]
    fn r760_aggregate_outcomes_all_failed() {
        let rows = vec![EffectExecutionStatus::Failed, EffectExecutionStatus::Failed];
        let (success, total, status) = aggregate_outcomes(&rows);
        assert_eq!(success, 0);
        assert_eq!(total, 2);
        assert_eq!(status, "failed");
        assert!(!is_final_success(&status));
        assert!(!is_partial_success(&status));
    }

    /// aggregate_outcomes: 部分成功 -> partial。
    #[test]
    fn r760_aggregate_outcomes_partial() {
        let rows = vec![
            EffectExecutionStatus::Executed,
            EffectExecutionStatus::Failed,
            EffectExecutionStatus::Skipped,
        ];
        let (success, total, status) = aggregate_outcomes(&rows);
        assert_eq!(success, 1);
        assert_eq!(total, 3);
        assert_eq!(status, "partial");
        assert!(!is_final_success(&status));
        assert!(is_partial_success(&status));
    }

    /// aggregate_outcomes: Skipped 不算 success，触发 partial 判定。
    #[test]
    fn r760_aggregate_outcomes_with_skipped() {
        let rows = vec![EffectExecutionStatus::Executed, EffectExecutionStatus::Skipped];
        let (success, total, status) = aggregate_outcomes(&rows);
        assert_eq!(success, 1);
        assert_eq!(total, 2);
        assert_eq!(status, "partial");
    }
}


#[cfg(test)]
mod internal_tests_r771 {
    use super::*;

    // ---- Round 771: pc-decisions::effect_outcome_pure 边缘测试 ----

    /// EffectExecutionStatus 4 个变体字符串稳定。
    #[test]
    fn r771_effect_status_as_str() {
        assert_eq!(EffectExecutionStatus::Executed.as_str(), "executed");
        assert_eq!(EffectExecutionStatus::Failed.as_str(), "failed");
        assert_eq!(EffectExecutionStatus::Skipped.as_str(), "skipped");
        assert_eq!(EffectExecutionStatus::Pending.as_str(), "pending");
    }

    /// from_str: 4 个 + 大小写 + 未知。
    #[test]
    fn r771_effect_status_from_str() {
        assert_eq!(EffectExecutionStatus::from_str("executed"), Some(EffectExecutionStatus::Executed));
        assert_eq!(EffectExecutionStatus::from_str("FAILED"), Some(EffectExecutionStatus::Failed), "case insensitive");
        assert_eq!(EffectExecutionStatus::from_str("  Skipped  "), Some(EffectExecutionStatus::Skipped), "trimmed");
        assert_eq!(EffectExecutionStatus::from_str("pending"), Some(EffectExecutionStatus::Pending));
        assert_eq!(EffectExecutionStatus::from_str("unknown"), None);
    }

    /// is_successful: 仅 Executed。
    #[test]
    fn r771_is_successful() {
        assert!(EffectExecutionStatus::Executed.is_successful());
        assert!(!EffectExecutionStatus::Failed.is_successful());
        assert!(!EffectExecutionStatus::Skipped.is_successful());
        assert!(!EffectExecutionStatus::Pending.is_successful());
    }

    /// is_final_success / is_partial_success: 4 种状态。
    #[test]
    fn r771_is_final_success_and_partial() {
        assert!(is_final_success("succeeded"));
        assert!(!is_final_success("failed"));
        assert!(!is_final_success("partial"));
        assert!(!is_final_success("unknown"));

        assert!(!is_partial_success("succeeded"));
        assert!(!is_partial_success("failed"));
        assert!(is_partial_success("partial"));
    }

    /// aggregate_outcomes: 仅 Pending (不是 executed / failed / skipped)。
    #[test]
    fn r771_aggregate_only_pending() {
        let rows = vec![EffectExecutionStatus::Pending, EffectExecutionStatus::Pending];
        let (succ, total, label) = aggregate_outcomes(&rows);
        assert_eq!(succ, 0);
        assert_eq!(total, 2);
        assert_eq!(label, "failed", "0 successful = failed");
    }

    /// aggregate_outcomes: 混合 executed + skipped (不算成功)。
    #[test]
    fn r771_aggregate_executed_with_skipped() {
        let rows = vec![
            EffectExecutionStatus::Executed,
            EffectExecutionStatus::Skipped,
            EffectExecutionStatus::Skipped,
        ];
        let (succ, total, label) = aggregate_outcomes(&rows);
        assert_eq!(succ, 1);
        assert_eq!(total, 3);
        assert_eq!(label, "partial", "1 < 3 → partial");
    }
}
