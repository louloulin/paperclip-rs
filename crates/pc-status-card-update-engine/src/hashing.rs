//! Hashing —— SHA-256 hash 计算。
//!
//! 与 Node `statusCardChangesHash` / `statusCardFingerprintHash` 1:1 对齐。

use sha2::{Digest, Sha256};

use crate::types::{StatusCardDeltaChange, StatusCardFingerprint};

/// 计算 changes 集合的稳定 hash（与 Node `statusCardChangesHash` 1:1 对齐）。
///
/// ## 稳定化规则
///
/// - 取 `{ issueId, changeKind, from, to }` 四个字段；
/// - 按 `${issueId}:${changeKind}` 字典序排序；
/// - JSON 序列化后做 SHA-256。
pub fn status_card_changes_hash(changes: &[StatusCardDeltaChange]) -> String {
    let mut stable: Vec<(String, String, Option<String>, Option<String>)> = changes
        .iter()
        .map(|c| {
            (
                c.issue_id.clone(),
                c.change_kind.as_str().to_string(),
                c.from.clone(),
                c.to.clone(),
            )
        })
        .collect();
    stable.sort_by(|a, b| format!("{}:{}", a.0, a.1).cmp(&format!("{}:{}", b.0, b.1)));

    let json = serde_json::to_string(&stable).expect("serialize changes");
    let digest = Sha256::digest(json.as_bytes());
    format!("{:x}", digest)
}

/// 计算 fingerprint 的稳定 hash（与 Node `statusCardFingerprintHash` 1:1 对齐）。
///
/// ## 稳定化规则
///
/// - 按 key 字典序排序 entries；
/// - JSON 序列化后做 SHA-256。
pub fn status_card_fingerprint_hash(fingerprint: &StatusCardFingerprint) -> String {
    let mut entries: Vec<(&String, &crate::types::FingerprintEntry)> = fingerprint.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    // To serialize as a JSON object with sorted keys, build a serde_json::Value
    let mut obj = serde_json::Map::new();
    for (k, v) in entries {
        let val = serde_json::json!({
            "status": v.status,
            "updatedAt": v.updated_at,
            "latestHumanCommentAt": v.latest_human_comment_at,
            "identifier": v.identifier,
            "title": v.title,
            "assigneeAgentId": v.assignee_agent_id,
            "assigneeUserId": v.assignee_user_id,
        });
        obj.insert(k.clone(), val);
    }
    let json =
        serde_json::to_string(&serde_json::Value::Object(obj)).expect("serialize fingerprint");
    let digest = Sha256::digest(json.as_bytes());
    format!("{:x}", digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChangeKind, FingerprintEntry, StatusCardDeltaChange};
    use std::collections::HashMap;

    #[test]
    fn r676_changes_hash_differs_for_different_changes() {
        let a = vec![StatusCardDeltaChange {
            issue_id: "one".into(),
            identifier: "PAP-1".into(),
            title: "One".into(),
            from: Some("todo".into()),
            to: Some("done".into()),
            change_kind: ChangeKind::Status,
        }];
        let b = vec![StatusCardDeltaChange {
            issue_id: "two".into(),
            identifier: "PAP-2".into(),
            title: "Two".into(),
            from: Some("todo".into()),
            to: Some("done".into()),
            change_kind: ChangeKind::Status,
        }];
        assert_ne!(status_card_changes_hash(&a), status_card_changes_hash(&b));
    }

    #[test]
    fn r676_changes_hash_is_stable_regardless_of_input_order() {
        let a = vec![
            StatusCardDeltaChange {
                issue_id: "b".into(),
                identifier: "PAP-2".into(),
                title: "B".into(),
                from: None,
                to: None,
                change_kind: ChangeKind::New,
            },
            StatusCardDeltaChange {
                issue_id: "a".into(),
                identifier: "PAP-1".into(),
                title: "A".into(),
                from: None,
                to: None,
                change_kind: ChangeKind::New,
            },
        ];
        let b = vec![
            StatusCardDeltaChange {
                issue_id: "a".into(),
                identifier: "PAP-1".into(),
                title: "A".into(),
                from: None,
                to: None,
                change_kind: ChangeKind::New,
            },
            StatusCardDeltaChange {
                issue_id: "b".into(),
                identifier: "PAP-2".into(),
                title: "B".into(),
                from: None,
                to: None,
                change_kind: ChangeKind::New,
            },
        ];
        assert_eq!(status_card_changes_hash(&a), status_card_changes_hash(&b));
    }

    #[test]
    fn r676_changes_hash_ignores_identifier_and_title() {
        let a = vec![StatusCardDeltaChange {
            issue_id: "x".into(),
            identifier: "PAP-1".into(),
            title: "First title".into(),
            from: Some("a".into()),
            to: Some("b".into()),
            change_kind: ChangeKind::Status,
        }];
        let b = vec![StatusCardDeltaChange {
            issue_id: "x".into(),
            identifier: "PAP-99".into(),
            title: "Different title".into(),
            from: Some("a".into()),
            to: Some("b".into()),
            change_kind: ChangeKind::Status,
        }];
        // identifier / title 不参与 hash（与 Node `({ issueId, changeKind, from, to })` 一致）
        assert_eq!(status_card_changes_hash(&a), status_card_changes_hash(&b));
    }

    #[test]
    fn r676_fingerprint_hash_is_stable_regardless_of_input_order() {
        let mut a = HashMap::new();
        a.insert(
            "a".to_string(),
            FingerprintEntry {
                status: "todo".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                latest_human_comment_at: None,
                identifier: Some("PAP-1".into()),
                title: Some("A".into()),
                assignee_agent_id: None,
                assignee_user_id: None,
            },
        );
        a.insert(
            "b".to_string(),
            FingerprintEntry {
                status: "done".into(),
                updated_at: "2026-01-02T00:00:00Z".into(),
                latest_human_comment_at: None,
                identifier: Some("PAP-2".into()),
                title: Some("B".into()),
                assignee_agent_id: None,
                assignee_user_id: None,
            },
        );
        let mut b = HashMap::new();
        b.insert(
            "b".to_string(),
            FingerprintEntry {
                status: "done".into(),
                updated_at: "2026-01-02T00:00:00Z".into(),
                latest_human_comment_at: None,
                identifier: Some("PAP-2".into()),
                title: Some("B".into()),
                assignee_agent_id: None,
                assignee_user_id: None,
            },
        );
        b.insert(
            "a".to_string(),
            FingerprintEntry {
                status: "todo".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                latest_human_comment_at: None,
                identifier: Some("PAP-1".into()),
                title: Some("A".into()),
                assignee_agent_id: None,
                assignee_user_id: None,
            },
        );
        assert_eq!(
            status_card_fingerprint_hash(&a),
            status_card_fingerprint_hash(&b)
        );
    }
}
