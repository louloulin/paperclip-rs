//! 纯函数分类器 — 对应 Node `services/issue-visibility.ts` + 扩展 API。
//!
//! 单一职责：消费 `&IssueRow`，返回 visibility 分类结果。
//! 不依赖 DB / 网络 — 所有数据通过参数传入。

use std::collections::HashMap;

use pc_repos::issue::IssueRow;

use crate::types::{
    IssueVisibilityClassification, IssueVisibilityReason, VisibilityFilterConfig,
    VisibilityStats,
};

// -----------------------------------------------------------------------------
// Single-row helpers
// -----------------------------------------------------------------------------

/// 检查 issue 是否可见（与 Node 谓词 1:1）。
///
/// `true` 当 `hidden_at IS NULL AND harness_kind IS NULL`。
pub fn is_visible(row: &IssueRow) -> bool {
    row.hidden_at.is_none() && row.harness_kind.is_none()
}

/// 检查 issue 是否被隐藏。
pub fn is_hidden(row: &IssueRow) -> bool {
    row.hidden_at.is_some()
}

/// 检查 issue 是否属于 harness 子系统。
pub fn has_harness_kind(row: &IssueRow) -> bool {
    row.harness_kind.is_some()
}

/// 分类单个 issue。
pub fn classify(row: &IssueRow) -> IssueVisibilityClassification {
    IssueVisibilityClassification::from_row(row)
}

// -----------------------------------------------------------------------------
// Batch helpers
// -----------------------------------------------------------------------------

/// 批量分类。
pub fn classify_batch(rows: &[&IssueRow]) -> Vec<IssueVisibilityClassification> {
    rows.iter().map(|r| classify(*r)).collect()
}

/// 过滤出可见 issue（不修改原数据）。
pub fn filter_visible<'a>(rows: &'a [IssueRow]) -> Vec<&'a IssueRow> {
    rows.iter().filter(|r| is_visible(r)).collect()
}

/// 按 filter config 过滤。
pub fn filter_with_config<'a>(
    rows: &'a [IssueRow],
    config: &VisibilityFilterConfig,
) -> Vec<&'a IssueRow> {
    rows.iter()
        .filter(|r| config.accepts(&classify(r)))
        .collect()
}

/// 按 reason 分组并计数。
pub fn count_by_reason(rows: &[IssueRow]) -> HashMap<IssueVisibilityReason, usize> {
    let mut out = HashMap::new();
    for row in rows {
        *out.entry(classify(row).reason).or_insert(0) += 1;
    }
    out
}

/// 计算 visibility 统计。
pub fn stats(rows: &[IssueRow]) -> VisibilityStats {
    let mut stats = VisibilityStats {
        total: rows.len(),
        ..Default::default()
    };
    let mut by_reason: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let c = classify(row);
        *by_reason.entry(c.reason.as_str().to_string()).or_insert(0) += 1;
        if c.is_visible {
            stats.visible += 1;
        } else {
            match c.reason {
                IssueVisibilityReason::HiddenAt => stats.hidden += 1,
                IssueVisibilityReason::HasHarnessKind => stats.harness_kind += 1,
                IssueVisibilityReason::Visible => {}
            }
        }
    }
    stats.by_reason = by_reason;
    stats
}

// -----------------------------------------------------------------------------
// Edge cases
// -----------------------------------------------------------------------------

/// Issue 同时被 hidden 且有 harness_kind — 返回的 reason 优先 hidden。
#[cfg(test)]
mod edge_tests {
    use super::*;
    use pc_core::Timestamp;
    use uuid::Uuid;

    fn make_row(hidden_at: Option<Timestamp>, harness_kind: Option<&str>) -> IssueRow {
        IssueRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            parent_id: None,
            title: "test".to_string(),
            description: None,
            status: "todo".to_string(),
            work_mode: "default".to_string(),
            harness_kind: harness_kind.map(|s| s.to_string()),
            priority: "medium".to_string(),
            assignee_agent_id: None,
            assignee_user_id: None,
            checkout_run_id: None,
            execution_run_id: None,
            execution_agent_name_key: None,
            execution_locked_at: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            issue_number: None,
            identifier: Some("X-1".to_string()),
            origin_kind: "user".to_string(),
            origin_id: None,
            origin_run_id: None,
            origin_fingerprint: "fp".to_string(),
            request_depth: 0,
            billing_code: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_state: None,
            monitor_next_check_at: None,
            monitor_wake_requested_at: None,
            monitor_last_triggered_at: None,
            monitor_attempt_count: 0,
            monitor_notes: None,
            monitor_scheduled_by: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            source_trust: None,
            unblock_descriptor: None,
            blocked_transition_at: None,
            blocked_owner_notified_at: None,
            started_at: None,
            completed_at: None,
            cancelled_at: None,
            hidden_at,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn hidden_takes_precedence_over_harness_kind() {
        let row = make_row(Some(Timestamp::now()), Some("claude"));
        assert_eq!(classify(&row).reason, IssueVisibilityReason::HiddenAt);
        assert!(!is_visible(&row));
    }

    #[test]
    fn harness_kind_only_is_not_visible() {
        let row = make_row(None, Some("codex"));
        assert_eq!(classify(&row).reason, IssueVisibilityReason::HasHarnessKind);
        assert!(!is_visible(&row));
        assert!(has_harness_kind(&row));
    }

    #[test]
    fn visible_when_no_hidden_no_harness_kind() {
        let row = make_row(None, None);
        assert!(is_visible(&row));
        assert!(!is_hidden(&row));
        assert!(!has_harness_kind(&row));
        assert_eq!(classify(&row).reason, IssueVisibilityReason::Visible);
    }
}
