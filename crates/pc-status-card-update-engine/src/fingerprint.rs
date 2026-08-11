//! Fingerprint —— 构建 + diff + filter。
//!
//! 与 Node `buildStatusCardFingerprint` / `diffStatusCardFingerprint` /
//! `filterStatusCardChanges` 1:1 对齐。

use std::collections::HashMap;

use crate::types::{
    ChangeKind, FingerprintEntry, RefreshTriggers, StatusCardDeltaChange, StatusCardFingerprint,
    StatusCardRefreshPolicy,
};

// ============================================================================
// Build fingerprint
// ============================================================================

/// 直接从 HashMap 构造 [`StatusCardFingerprint`]。
///
/// 与 Node 端 `Object.fromEntries(issues.map(issue => [issue.id, {...}])` 1:1 对齐。
///
/// 调用方应传入 `HashMap<issue_id, FingerprintEntry>`。
pub fn build_status_card_fingerprint(
    entries: HashMap<String, FingerprintEntry>,
) -> StatusCardFingerprint {
    entries
}

// ============================================================================
// Diff
// ============================================================================

/// Diff 两个 fingerprints，输出 delta changes（与 Node `diffStatusCardFingerprint` 1:1 对齐）。
///
/// 规则：
/// 1. 对 `current` 中每条 issue：若 `previous` 不存在 → `new`。
/// 2. 若存在且 `status` 不同 → `status` change。
/// 3. 若 assignee 改变 → `assignee` change（从 null 视角）。
/// 4. 若 `latest_human_comment_at` 改变且 new 非空 → `human_comment` change。
/// 5. 若 `updated_at` 改变且前面没有 specific change → `updated` change。
/// 6. 对 `previous` 中存在但 `current` 中不存在 → `removed` change。
pub fn diff_status_card_fingerprint(
    previous: Option<&StatusCardFingerprint>,
    current: &StatusCardFingerprint,
) -> Vec<StatusCardDeltaChange> {
    let mut changes: Vec<StatusCardDeltaChange> = Vec::new();
    let before = previous.cloned().unwrap_or_default();

    for (issue_id, next) in current {
        let prior = before.get(issue_id);

        let identifier = next.identifier.clone().unwrap_or_else(|| issue_id.clone());
        let title = next.title.clone().unwrap_or_default();

        match prior {
            None => {
                changes.push(StatusCardDeltaChange {
                    issue_id: issue_id.clone(),
                    identifier,
                    title,
                    from: None,
                    to: Some(next.status.clone()),
                    change_kind: ChangeKind::New,
                });
            }
            Some(prior) => {
                let mut has_specific_change = false;

                if prior.status != next.status {
                    changes.push(StatusCardDeltaChange {
                        issue_id: issue_id.clone(),
                        identifier: identifier.clone(),
                        title: title.clone(),
                        from: Some(prior.status.clone()),
                        to: Some(next.status.clone()),
                        change_kind: ChangeKind::Status,
                    });
                    has_specific_change = true;
                }

                if prior.assignee_agent_id != next.assignee_agent_id
                    || prior.assignee_user_id != next.assignee_user_id
                {
                    changes.push(StatusCardDeltaChange {
                        issue_id: issue_id.clone(),
                        identifier: identifier.clone(),
                        title: title.clone(),
                        from: None,
                        to: None,
                        change_kind: ChangeKind::Assignee,
                    });
                    has_specific_change = true;
                }

                if prior.latest_human_comment_at != next.latest_human_comment_at
                    && next.latest_human_comment_at.is_some()
                {
                    changes.push(StatusCardDeltaChange {
                        issue_id: issue_id.clone(),
                        identifier: identifier.clone(),
                        title: title.clone(),
                        from: prior.latest_human_comment_at.clone(),
                        to: next.latest_human_comment_at.clone(),
                        change_kind: ChangeKind::HumanComment,
                    });
                    has_specific_change = true;
                }

                if prior.updated_at != next.updated_at && !has_specific_change {
                    changes.push(StatusCardDeltaChange {
                        issue_id: issue_id.clone(),
                        identifier,
                        title,
                        from: Some(prior.status.clone()),
                        to: Some(next.status.clone()),
                        change_kind: ChangeKind::Updated,
                    });
                }
            }
        }
    }

    for (issue_id, prior) in &before {
        if current.contains_key(issue_id) {
            continue;
        }
        let identifier = prior.identifier.clone().unwrap_or_else(|| issue_id.clone());
        let title = prior.title.clone().unwrap_or_default();
        changes.push(StatusCardDeltaChange {
            issue_id: issue_id.clone(),
            identifier,
            title,
            from: Some(prior.status.clone()),
            to: None,
            change_kind: ChangeKind::Removed,
        });
    }

    changes
}

// ============================================================================
// Filter
// ============================================================================

/// 根据 policy 的 triggers 过滤 change 列表（与 Node `filterStatusCardChanges` 1:1 对齐）。
pub fn filter_status_card_changes(
    changes: Vec<StatusCardDeltaChange>,
    policy: &StatusCardRefreshPolicy,
) -> Vec<StatusCardDeltaChange> {
    changes
        .into_iter()
        .filter(|c| match_change(c, &policy.triggers))
        .collect()
}

fn match_change(c: &StatusCardDeltaChange, triggers: &RefreshTriggers) -> bool {
    if triggers.any_update {
        return true;
    }
    if (c.change_kind == ChangeKind::New || c.change_kind == ChangeKind::Removed)
        && triggers.membership_changes
    {
        return true;
    }
    if c.change_kind == ChangeKind::Assignee && triggers.assignee_changes {
        return true;
    }
    if c.change_kind == ChangeKind::HumanComment && triggers.human_comments {
        return true;
    }
    if c.change_kind == ChangeKind::Status && triggers.status_transitions {
        return true;
    }
    false
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: &str, updated_at: &str) -> FingerprintEntry {
        FingerprintEntry {
            status: status.to_string(),
            updated_at: updated_at.to_string(),
            latest_human_comment_at: None,
            identifier: None,
            title: None,
            assignee_agent_id: None,
            assignee_user_id: None,
        }
    }

    fn entry_with_id(
        id: &str,
        status: &str,
        updated_at: &str,
        identifier: &str,
        title: &str,
    ) -> (String, FingerprintEntry) {
        (
            id.to_string(),
            FingerprintEntry {
                status: status.to_string(),
                updated_at: updated_at.to_string(),
                latest_human_comment_at: None,
                identifier: Some(identifier.to_string()),
                title: Some(title.to_string()),
                assignee_agent_id: None,
                assignee_user_id: None,
            },
        )
    }

    #[test]
    fn r676_diff_marks_new_status_removed() {
        let mut prev = StatusCardFingerprint::new();
        prev.insert(
            "churn".to_string(),
            entry("todo", "2026-07-23T10:00:00.000Z"),
        );
        prev.insert(
            "done".to_string(),
            entry("in_progress", "2026-07-23T10:00:00.000Z"),
        );
        prev.insert(
            "removed".to_string(),
            entry("blocked", "2026-07-23T10:00:00.000Z"),
        );

        let mut curr = StatusCardFingerprint::new();
        curr.insert(
            "churn".to_string(),
            entry("in_progress", "2026-07-23T10:01:00.000Z"),
        );
        curr.insert(
            "done".to_string(),
            entry("done", "2026-07-23T10:01:00.000Z"),
        );
        curr.insert(
            "added".to_string(),
            entry("todo", "2026-07-23T10:01:00.000Z"),
        );

        let changes = diff_status_card_fingerprint(Some(&prev), &curr);
        assert_eq!(changes.len(), 4);
        let kinds: Vec<_> = changes.iter().map(|c| c.change_kind).collect();
        assert!(kinds.contains(&ChangeKind::Status));
        assert!(kinds.contains(&ChangeKind::Status));
        assert!(kinds.contains(&ChangeKind::New));
        assert!(kinds.contains(&ChangeKind::Removed));
    }

    #[test]
    fn r676_diff_tracks_human_comment_independently() {
        let mut prev = StatusCardFingerprint::new();
        prev.insert(
            "issue".to_string(),
            FingerprintEntry {
                status: "in_progress".to_string(),
                updated_at: "2026-07-23T10:00:00.000Z".to_string(),
                latest_human_comment_at: None,
                identifier: None,
                title: None,
                assignee_agent_id: None,
                assignee_user_id: None,
            },
        );
        let mut curr = StatusCardFingerprint::new();
        curr.insert(
            "issue".to_string(),
            FingerprintEntry {
                status: "done".to_string(),
                updated_at: "2026-07-23T10:01:00.000Z".to_string(),
                latest_human_comment_at: Some("2026-07-23T10:01:00.000Z".to_string()),
                identifier: None,
                title: None,
                assignee_agent_id: None,
                assignee_user_id: None,
            },
        );
        let changes = diff_status_card_fingerprint(Some(&prev), &curr);
        assert_eq!(changes.len(), 2);
        let kinds: Vec<_> = changes.iter().map(|c| c.change_kind).collect();
        assert!(kinds.contains(&ChangeKind::Status));
        assert!(kinds.contains(&ChangeKind::HumanComment));
    }

    #[test]
    fn r676_diff_handles_null_previous() {
        let mut curr = StatusCardFingerprint::new();
        curr.insert("x".to_string(), entry("todo", "2026-07-23T10:00:00.000Z"));
        let changes = diff_status_card_fingerprint(None, &curr);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_kind, ChangeKind::New);
    }

    #[test]
    fn r676_filter_keeps_status_membership_by_default() {
        let policy = StatusCardRefreshPolicy::default_manual();
        let changes = vec![
            StatusCardDeltaChange {
                issue_id: "a".into(),
                identifier: "PAP-1".into(),
                title: "A".into(),
                from: None,
                to: Some("todo".into()),
                change_kind: ChangeKind::New,
            },
            StatusCardDeltaChange {
                issue_id: "b".into(),
                identifier: "PAP-2".into(),
                title: "B".into(),
                from: None,
                to: None,
                change_kind: ChangeKind::Assignee,
            },
            StatusCardDeltaChange {
                issue_id: "c".into(),
                identifier: "PAP-3".into(),
                title: "C".into(),
                from: None,
                to: None,
                change_kind: ChangeKind::HumanComment,
            },
        ];
        let filtered = filter_status_card_changes(changes, &policy);
        // default 全部 true，除 any_update=false
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn r676_filter_respects_comment_only_policy() {
        let policy = StatusCardRefreshPolicy {
            mode: crate::types::RefreshMode::Interval,
            interval_minutes: Some(15),
            debounce_seconds: None,
            max_updates_per_hour: None,
            triggers: RefreshTriggers {
                status_transitions: false,
                membership_changes: false,
                human_comments: true,
                assignee_changes: false,
                any_update: false,
            },
            active_hours: None,
            daily_token_cap: None,
        };
        let changes = vec![
            StatusCardDeltaChange {
                issue_id: "a".into(),
                identifier: "PAP-1".into(),
                title: "A".into(),
                from: Some("todo".into()),
                to: Some("done".into()),
                change_kind: ChangeKind::Status,
            },
            StatusCardDeltaChange {
                issue_id: "b".into(),
                identifier: "PAP-2".into(),
                title: "B".into(),
                from: None,
                to: None,
                change_kind: ChangeKind::HumanComment,
            },
        ];
        let filtered = filter_status_card_changes(changes, &policy);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].change_kind, ChangeKind::HumanComment);
    }

    #[test]
    fn r676_filter_any_update_overrides_others() {
        let policy = StatusCardRefreshPolicy {
            triggers: RefreshTriggers {
                any_update: true,
                ..RefreshTriggers::default()
            },
            ..StatusCardRefreshPolicy::default_manual()
        };
        let changes = vec![StatusCardDeltaChange {
            issue_id: "a".into(),
            identifier: "PAP-1".into(),
            title: "A".into(),
            from: None,
            to: None,
            change_kind: ChangeKind::Updated,
        }];
        let filtered = filter_status_card_changes(changes, &policy);
        assert_eq!(filtered.len(), 1);
    }
}
