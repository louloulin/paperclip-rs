//! Pause-hold 抑制闸门：判断 issue（及其祖先）是否被 pause-hold 抑制。
//!
//! 对齐 Node `services/recovery/pause-hold-guard.ts` +
//! `issueTreeControlService.getActivePauseHoldGate`：
//! - SELECT 所有 `status='active' AND mode='pause'` 的 holds
//! - 沿 `issues.parent_id` 链向上遍历，找到祖先中第一个 active pause hold
//! - 若找到（含自身） → 返回 true（suppress escalate）
//!
//! 边界：
// - 纯函数 `walk_pause_hold_chain` 不依赖 DB，给定 holds map + 起始 issue_id + parent lookup → bool
//! - DB 接入层 `is_automatic_recovery_suppressed_by_pause_hold` 查 DB 后委托给纯函数

use std::collections::HashMap;
use uuid::Uuid;

use pc_repos::issue_tree_hold::{IssueTreeHoldRepo, IssueTreeHoldRow};
use pc_repos::Db;

/// Node `MAX_PAUSE_HOLD_ANCESTOR_DEPTH` 常量镜像。
pub const MAX_PAUSE_HOLD_ANCESTOR_DEPTH: usize = 32;

/// Pause hold 闸门命中结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseHoldGateHit {
    pub hold_id: Uuid,
    pub root_issue_id: Uuid,
    pub issue_id: Uuid,
    pub is_root: bool,
    pub reason: Option<String>,
    pub release_policy: Option<serde_json::Value>,
}

/// 纯函数：在给定的祖先链（self-first 顺序）+ active pause holds 中，
/// 沿链向上寻找是否被 pause-hold 抑制。
///
/// 返回 `Some(hit)` 表示命中祖先（含自身）的 active pause-hold。
/// 返回 `None` 表示无抑制。
pub fn walk_pause_hold_chain(
    ancestors_self_first: &[Uuid],
    holds_by_root: &HashMap<Uuid, &IssueTreeHoldRow>,
) -> Option<PauseHoldGateHit> {
    if ancestors_self_first.len() > MAX_PAUSE_HOLD_ANCESTOR_DEPTH {
        return None;
    }
    let issue_id = ancestors_self_first.first().copied()?;
    for ancestor in ancestors_self_first {
        if let Some(hold) = holds_by_root.get(ancestor) {
            return Some(PauseHoldGateHit {
                hold_id: hold.id,
                root_issue_id: hold.root_issue_id,
                issue_id,
                is_root: hold.root_issue_id == issue_id,
                reason: hold.reason.clone(),
                release_policy: Some(hold.release_policy.clone()),
            });
        }
    }
    None
}

/// DB 辅助：沿 parent_id 链向上取 issue 祖先（含自身），最多 MAX_PAUSE_HOLD_ANCESTOR_DEPTH 层。
/// 检测到循环（parent 指回已访问节点）时立即终止。
async fn collect_ancestor_chain(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Vec<Uuid>> {
    let mut chain = Vec::new();
    let mut visited: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut current = Some(issue_id);
    while let Some(issue_id_value) = current {
        if !visited.insert(issue_id_value) || chain.len() > MAX_PAUSE_HOLD_ANCESTOR_DEPTH {
            break;
        }
        chain.push(issue_id_value);
        let row: Option<(Option<Uuid>,)> =
            sqlx::query_as("SELECT parent_id FROM issues WHERE id=$1 AND company_id=$2")
                .bind(issue_id_value)
                .bind(company_id)
                .fetch_optional(db.pool())
                .await?;
        current = row.and_then(|(p,)| p);
    }
    Ok(chain)
}

/// DB 接入层：检查 issue 是否被 pause-hold 抑制。
///
/// 与 Node `isAutomaticRecoverySuppressedByPauseHold` 对齐：
/// - SELECT active pause holds for company
/// - 沿 issues.parent_id 链向上遍历
pub async fn is_automatic_recovery_suppressed_by_pause_hold(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Option<PauseHoldGateHit>> {
    let holds = IssueTreeHoldRepo::new(db)
        .list_active_pause_holds_for_company(company_id)
        .await?;
    let holds_by_root: HashMap<Uuid, &IssueTreeHoldRow> =
        holds.iter().map(|h| (h.root_issue_id, h)).collect();
    let ancestors = collect_ancestor_chain(db, company_id, issue_id).await?;
    Ok(walk_pause_hold_chain(&ancestors, &holds_by_root))
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pc_core::Timestamp;

    fn hold(id_byte: u8, root_byte: u8) -> IssueTreeHoldRow {
        IssueTreeHoldRow {
            id: Uuid::from_bytes([id_byte; 16]),
            root_issue_id: Uuid::from_bytes([root_byte; 16]),
            mode: "pause".into(),
            status: "active".into(),
            reason: Some("user paused".into()),
            release_policy: serde_json::json!({}),
            created_at: Timestamp::from_dt(Utc::now()),
            updated_at: Timestamp::from_dt(Utc::now()),
        }
    }

    #[test]
    fn walk_returns_none_when_no_hold() {
        let leaf = Uuid::from_bytes([3; 16]);
        let mut holds = HashMap::new();
        let h = hold(99, 99);
        holds.insert(h.root_issue_id, &h);
        let ancestors = vec![leaf, Uuid::from_bytes([2; 16])];
        assert!(walk_pause_hold_chain(&ancestors, &holds).is_none());
    }

    #[test]
    fn walk_returns_hit_when_self_is_root_of_pause_hold() {
        let id = Uuid::from_bytes([1; 16]);
        let h = hold(99, 1);
        let mut holds = HashMap::new();
        holds.insert(h.root_issue_id, &h);
        let ancestors = vec![id];
        let hit = walk_pause_hold_chain(&ancestors, &holds).expect("hit");
        assert!(hit.is_root);
        assert_eq!(hit.hold_id, h.id);
    }

    #[test]
    fn walk_traverses_parent_chain_to_find_ancestor_hold() {
        let leaf = Uuid::from_bytes([3; 16]);
        let mid = Uuid::from_bytes([2; 16]);
        let root = Uuid::from_bytes([1; 16]);
        let h = hold(99, 1);
        let mut holds = HashMap::new();
        holds.insert(h.root_issue_id, &h);
        let ancestors = vec![leaf, mid, root];
        let hit = walk_pause_hold_chain(&ancestors, &holds).expect("hit");
        assert_eq!(hit.root_issue_id, root);
        assert!(!hit.is_root);
    }

    #[test]
    fn walk_stops_at_max_ancestor_depth() {
        let h = hold(99, 200); // never reached
        let mut holds = HashMap::new();
        holds.insert(h.root_issue_id, &h);
        // 100-elem chain exceeds MAX_PAUSE_HOLD_ANCESTOR_DEPTH (32)
        let mut ancestors = Vec::new();
        for i in 1..100u8 {
            ancestors.push(Uuid::from_bytes([i; 16]));
        }
        let result = walk_pause_hold_chain(&ancestors, &holds);
        assert!(result.is_none(), "depth limit must terminate traversal");
    }

    #[test]
    fn walk_returns_none_for_empty_chain() {
        let h = hold(99, 1);
        let mut holds = HashMap::new();
        holds.insert(h.root_issue_id, &h);
        assert!(walk_pause_hold_chain(&[], &holds).is_none());
    }
}
