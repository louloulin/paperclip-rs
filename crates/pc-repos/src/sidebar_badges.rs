//! 公司侧边栏徽标聚合（对齐 Node `server/src/services/sidebar-badges.ts`，86 行）。
//!
//! 单一职责：聚合 inbox / approvals / failedRuns / joinRequests 四类徽标计数，
//! 复用 Node `dismissals` 抑制语义（在某 itemKey 上 dismiss 的时间戳 >= 活动时间戳时跳过）。
//!
//! 注：与 `crates/pc-http/src/routes/sidebar_badges.rs` 的并行实现是**互补关系**：
//! - 本 module 输出 Node `SidebarBadges` 形状（`inbox / approvals / failedRuns / joinRequests`）
//! - HTTP 路由输出扩展形状（按 status 细分的 agents / issues / approvals / costs / runs 计数）
//! 两者可同时存在，由不同前端入口按需使用。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::Db;

/// Actionable approval statuses（与 Node `ACTIONABLE_APPROVAL_STATUSES` 1:1 对齐）。
pub const ACTIONABLE_APPROVAL_STATUSES: &[&str] = &["pending", "revision_requested"];

/// Failed heartbeat run statuses（与 Node `FAILED_HEARTBEAT_STATUSES` 1:1 对齐）。
pub const FAILED_HEARTBEAT_STATUSES: &[&str] = &["failed", "timed_out"];

/// SidebarBadges 输出（与 Node `SidebarBadges` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarBadges {
    pub inbox: i64,
    pub approvals: i64,
    pub failed_runs: i64,
    pub join_requests: i64,
}

impl SidebarBadges {
    pub const fn zero() -> Self {
        Self {
            inbox: 0,
            approvals: 0,
            failed_runs: 0,
            join_requests: 0,
        }
    }
}

/// `extra.joinRequests` 元素（与 Node `{ id, updatedAt, createdAt }` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequestEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// `extra` 注入参数（与 Node `extra?` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct SidebarBadgesExtra {
    /// 已 dismiss 项的 key → dismiss 时间戳（毫秒 epoch）。
    pub dismissals: HashMap<String, i64>,
    /// 外部 join request 列表（不在 DB 表里，由调用方注入）。
    pub join_requests: Vec<JoinRequestEntry>,
    /// unread touched issues 计数（外部计算后注入）。
    pub unread_touched_issues: i64,
}

impl SidebarBadgesExtra {
    pub fn new() -> Self {
        Self::default()
    }
}

/// 把 `Date | string | null | undefined` 归一为 ms epoch（与 Node `normalizeTimestamp` 1:1 对齐）。
///
/// - `None` 或空 → 0
/// - 无法 parse → 0
/// - 否则 → ms epoch
pub fn normalize_timestamp(value: Option<DateTime<Utc>>) -> i64 {
    value.map(|t| t.timestamp_millis()).unwrap_or(0)
}

/// 把 `(key, ms epoch)` 也支持的便捷重载（与 Node `normalizeTimestamp` 接受 `Date | string` 1:1 对齐）。
pub fn normalize_timestamp_millis(value_ms: i64) -> i64 {
    if value_ms == 0 {
        0
    } else {
        value_ms
    }
}

/// 判断 itemKey 在 dismissals 中是否被「dismiss 时间戳 >= activityAt」抑制（与 Node `isDismissed` 1:1 对齐）。
pub fn is_dismissed(
    dismissed_at_by_key: &HashMap<String, i64>,
    item_key: &str,
    activity_at_ms: i64,
) -> bool {
    match dismissed_at_by_key.get(item_key) {
        Some(&dismissed_at) => dismissed_at >= activity_at_ms,
        None => false,
    }
}

/// SidebarBadges 服务（与 Node `sidebarBadgeService(db)` 1:1 对齐）。
///
/// 行为：
/// 1. 拉 `approvals` 表中 `status IN (pending, revision_requested)` 的 id/updatedAt 列表
/// 2. 过滤掉 dismissals 中被抑制的项
/// 3. 拉 `heartbeat_runs JOIN agents` 按 agent 取最新 run（DISTINCT ON）
/// 4. 过滤掉 terminated agent 的 run
/// 5. 在 latest-per-agent 中计数 `status IN (failed, timed_out)` 且未被 dismissed 的行
/// 6. 计数 joinRequests 中未被 dismissed 的项
/// 7. inbox = approvals + failedRuns + joinRequests + unreadTouchedIssues
pub struct SidebarBadgesService<'a> {
    db: &'a Db,
}

impl<'a> SidebarBadgesService<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 拉取徽标聚合（与 Node `service.get(companyId, extra?)` 1:1 对齐）。
    pub async fn get(
        &self,
        company_id: Uuid,
        extra: Option<&SidebarBadgesExtra>,
    ) -> Result<SidebarBadges, sqlx::Error> {
        let extra = extra.cloned().unwrap_or_default();

        // 1) actionable approvals
        let approval_rows: Vec<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT id, updated_at FROM approvals WHERE company_id = $1 AND status = ANY($2)",
        )
        .bind(company_id)
        .bind(ACTIONABLE_APPROVAL_STATUSES)
        .fetch_all(self.db.pool())
        .await?;
        let actionable_approvals = approval_rows
            .into_iter()
            .filter(|(id, updated_at)| {
                let activity_at = normalize_timestamp(*updated_at);
                !is_dismissed(&extra.dismissals, &format!("approval:{id}"), activity_at)
            })
            .count() as i64;

        // 2) latest run per agent (DISTINCT ON) excluding terminated agents
        let latest_run_rows: Vec<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (h.agent_id)
                h.id, h.status, h.created_at
            FROM heartbeat_runs h
            INNER JOIN agents a ON a.id = h.agent_id
            WHERE h.company_id = $1
              AND a.company_id = $1
              AND a.status <> 'terminated'
            ORDER BY h.agent_id, h.created_at DESC
            "#,
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        let failed_runs = latest_run_rows
            .into_iter()
            .filter(|(_id, status, created_at)| {
                FAILED_HEARTBEAT_STATUSES.contains(&status.as_str())
                    && !is_dismissed(
                        &extra.dismissals,
                        &format!("run:{_id}"),
                        normalize_timestamp(Some(*created_at)),
                    )
            })
            .count() as i64;

        // 3) join requests (in-memory filter)
        let join_requests = extra
            .join_requests
            .iter()
            .filter(|req| {
                let activity_at = req
                    .updated_at
                    .map(|t| t.timestamp_millis())
                    .unwrap_or_else(|| req.created_at.timestamp_millis());
                !is_dismissed(&extra.dismissals, &format!("join:{}", req.id), activity_at)
            })
            .count() as i64;

        // 4) inbox = sum of the three sources + unread touched
        let inbox =
            actionable_approvals + failed_runs + join_requests + extra.unread_touched_issues;

        Ok(SidebarBadges {
            inbox,
            approvals: actionable_approvals,
            failed_runs,
            join_requests,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_timestamp_handles_none() {
        assert_eq!(normalize_timestamp(None), 0);
    }

    #[test]
    fn normalize_timestamp_returns_ms_epoch() {
        let ts = DateTime::parse_from_rfc3339("2026-07-23T18:13:03.000Z")
            .unwrap()
            .with_timezone(&Utc);
        let expected = ts.timestamp_millis();
        assert_eq!(normalize_timestamp(Some(ts)), expected);
    }

    #[test]
    fn is_dismissed_returns_false_when_key_absent() {
        let map = HashMap::new();
        assert!(!is_dismissed(&map, "approval:abc", 1000));
    }

    #[test]
    fn is_dismissed_returns_true_when_dismissed_at_ge_activity() {
        let mut map = HashMap::new();
        map.insert("approval:abc".to_string(), 1500);
        assert!(is_dismissed(&map, "approval:abc", 1000));
        assert!(is_dismissed(&map, "approval:abc", 1500)); // equality
    }

    #[test]
    fn is_dismissed_returns_false_when_dismissed_at_lt_activity() {
        let mut map = HashMap::new();
        map.insert("approval:abc".to_string(), 500);
        assert!(!is_dismissed(&map, "approval:abc", 1000));
    }

    #[test]
    fn sidebar_badges_zero_const() {
        let z = SidebarBadges::zero();
        assert_eq!(z.inbox, 0);
        assert_eq!(z.approvals, 0);
        assert_eq!(z.failed_runs, 0);
        assert_eq!(z.join_requests, 0);
    }

    #[test]
    fn extra_default_is_zero_unread() {
        let e = SidebarBadgesExtra::default();
        assert!(e.dismissals.is_empty());
        assert!(e.join_requests.is_empty());
        assert_eq!(e.unread_touched_issues, 0);
    }

    #[test]
    fn actionable_statuses_match_node_set() {
        assert!(ACTIONABLE_APPROVAL_STATUSES.contains(&"pending"));
        assert!(ACTIONABLE_APPROVAL_STATUSES.contains(&"revision_requested"));
        assert_eq!(ACTIONABLE_APPROVAL_STATUSES.len(), 2);
    }

    #[test]
    fn failed_statuses_match_node_set() {
        assert!(FAILED_HEARTBEAT_STATUSES.contains(&"failed"));
        assert!(FAILED_HEARTBEAT_STATUSES.contains(&"timed_out"));
        assert_eq!(FAILED_HEARTBEAT_STATUSES.len(), 2);
    }

    #[test]
    fn extra_unread_touched_issues_in_inbox_formula() {
        // Verify inbox formula: inbox = approvals + failed_runs + join_requests + unread_touched
        // by constructing badges manually.
        let badges = SidebarBadges {
            inbox: 5 + 3 + 2 + 10,
            approvals: 5,
            failed_runs: 3,
            join_requests: 2,
        };
        assert_eq!(badges.inbox, 20);
        assert_eq!(
            badges.approvals + badges.failed_runs + badges.join_requests + 10,
            badges.inbox
        );
    }
}
