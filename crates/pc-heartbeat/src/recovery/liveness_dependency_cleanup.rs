//! Issue graph liveness 辅助函数：清理 / 加载 / lookback 判定。
//!
//! 对齐 Node `services/recovery/service.ts` 中的：
//! - `loadLivenessDependencyUpdatedAtByIssue` —— 按 issue 加载 `updated_at`
//! - `latestDependencyUpdatedAtForLivenessFinding` —— 取 finding dependency path 最新 updated_at
//! - `isLivenessFindingInsideAutoRecoveryLookback` —— lookback 窗口判定
//! - `normalizeIssueGraphLivenessAutoRecoveryLookbackHours` —— 钳制到合法区间
//! - `retireObsoleteLivenessRecoveryIssues` —— 清理 obsolete escalation issues
//! - `retireDoneLivenessRecoveryBlockers` —— 清理 done blockers 的关系
//! - `removeRecoveryBlockerFromSource` —— 移除某 recovery issue 在 source 上的 blocker 关系
//! - `hasActiveRunForIssueId` —— 检查 issue 是否还有 active run
//!
//! 设计：
//! - 纯函数：`normalize_lookback_hours` / `latest_dependency_updated_at_for_finding` /
//!   `is_finding_inside_auto_recovery_lookback` / `liveness_dependency_issue_key`
//! - DB 副作用集中：`retire_*` / `load_*` / `remove_*` / `has_*`
//!
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

use super::issue_graph_liveness::IssueLivenessFinding;
use super::issue_graph_liveness_db::existing_blocker_issue_ids;
use super::origins::{
    build_issue_graph_liveness_leaf_key, parse_issue_graph_liveness_incident_key, LeafKeyInput,
};

// ============================================================================
// Constants
// ============================================================================

/// Issue graph liveness lookback 默认 24 小时（与 Node `DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS` 对齐）。
pub const DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS: i64 = 24;

/// Lookback 最小值 1 小时。
pub const MIN_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS: i64 = 1;

/// Lookback 最大值 720 小时（30 天）。
pub const MAX_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS: i64 = 24 * 30;

const ESCALATION_ORIGIN_KIND: &str = "harness_liveness_escalation";
const TERMINAL_STATUSES: &[&str] = &["done", "cancelled"];
const ACTIVE_RUN_STATUSES: &[&str] = &["queued", "running", "scheduled_retry"];

// ============================================================================
// Public types
// ============================================================================

/// `retireObsoleteLivenessRecoveryIssues` 输出。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetireObsoleteResult {
    pub retired: i64,
    pub active_skipped: i64,
    pub blocker_relations_removed: i64,
    pub retired_issue_ids: Vec<Uuid>,
}

/// `retireDoneLivenessRecoveryBlockers` 输出。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetireDoneBlockersResult {
    pub blocker_relations_removed: i64,
}

// ============================================================================
// Pure helpers (no DB)
// ============================================================================

/// 钳制 lookback hours 到 [MIN, MAX] 区间，默认 DEFAULT（与 Node `normalizeIssueGraphLivenessAutoRecoveryLookbackHours` 对齐）。
pub fn normalize_lookback_hours(raw: Option<i64>) -> i64 {
    let raw_value = raw.unwrap_or(DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS);
    let clamped = raw_value
        .max(MIN_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS)
        .min(MAX_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS);
    clamped
}

/// `(company_id, issue_id)` → string key（用于 updatedAtByIssueKey Map）。
pub fn liveness_dependency_issue_key(company_id: Uuid, issue_id: Uuid) -> String {
    format!("{}:{}", company_id, issue_id)
}

/// 取 finding.dependency_path 中所有 issue 的最新 updated_at。
///
/// 若任一 issue 不在 map 中（即 DB 没有这个 issue），返回 None。
/// 若 dependency_path 为空，返回 None。
pub fn latest_dependency_updated_at_for_finding(
    finding: &IssueLivenessFinding,
    updated_at_by_issue_key: &std::collections::HashMap<String, DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let unique: std::collections::BTreeSet<Uuid> =
        finding.dependency_path.iter().map(|e| e.issue_id).collect();
    if unique.is_empty() {
        return None;
    }
    let mut latest: Option<DateTime<Utc>> = None;
    for issue_id in &unique {
        let key = liveness_dependency_issue_key(finding.company_id, *issue_id);
        match updated_at_by_issue_key.get(&key) {
            Some(ts) => {
                latest = Some(match latest {
                    Some(curr) if curr >= *ts => curr,
                    _ => *ts,
                });
            }
            None => return None,
        }
    }
    latest
}

/// 判定 finding 是否在 auto recovery lookback 窗口内。
///
/// 与 Node `isLivenessFindingInsideAutoRecoveryLookback` 对齐：finding 的最新
/// dependency updated_at >= cutoff。
pub fn is_finding_inside_auto_recovery_lookback(
    finding: &IssueLivenessFinding,
    cutoff: DateTime<Utc>,
    updated_at_by_issue_key: &std::collections::HashMap<String, DateTime<Utc>>,
) -> bool {
    match latest_dependency_updated_at_for_finding(finding, updated_at_by_issue_key) {
        Some(ts) => ts >= cutoff,
        None => false,
    }
}

// ============================================================================
// DB loaders
// ============================================================================

/// 加载 finding.dependency_path 中所有 issue 的 updated_at，返回 Map<key, updated_at>。
///
/// 与 Node `loadLivenessDependencyUpdatedAtByIssue` 对齐：
/// - 去重 issue_ids
/// - 返回 `Map<livenessDependencyIssueKey, updatedAt>`
pub async fn load_liveness_dependency_updated_at_by_issue(
    db: &Db,
    findings: &[IssueLivenessFinding],
) -> sqlx::Result<std::collections::HashMap<String, DateTime<Utc>>> {
    let unique: std::collections::BTreeSet<Uuid> = findings
        .iter()
        .flat_map(|f| f.dependency_path.iter().map(|e| e.issue_id))
        .collect();
    if unique.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let ids_vec: Vec<Uuid> = unique.iter().copied().collect();
    let rows =
        sqlx::query("SELECT id, company_id, updated_at FROM issues WHERE id = ANY($1::uuid[])")
            .bind(&ids_vec)
            .fetch_all(db.pool())
            .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let company_id: Uuid = row.try_get("company_id")?;
        let updated_at: DateTime<Utc> = row.try_get("updated_at")?;
        map.insert(liveness_dependency_issue_key(company_id, id), updated_at);
    }
    Ok(map)
}

// ============================================================================
// Retire / cleanup
// ============================================================================

/// 清理 obsolete escalation issues：open 但不在 current findings 中。
///
/// 与 Node `retireObsoleteLivenessRecoveryIssues` 对齐：
/// - 收集 current findings 的 incident_keys 和 leaf_keys
/// - 对每个 open `harness_liveness_escalation` issue：
///   - 若 origin_id 命中 current incident_key → skip
///   - 若 parse 失败 → skip
///   - 若 leaf_key 命中 → skip
///   - 若 source issue 存在且非 done/cancelled 且 blocker chain 包含此 recovery → active skipped
///   - 否则：remove blocker relation → 若还有 active run → active skipped → 否则 update status='cancelled'
pub async fn retire_obsolete_liveness_recovery_issues(
    db: &Db,
    findings: &[IssueLivenessFinding],
) -> sqlx::Result<RetireObsoleteResult> {
    let mut result = RetireObsoleteResult::default();

    // 1. 收集 current incident_keys 和 leaf_keys
    let current_incident_keys: std::collections::HashSet<String> =
        findings.iter().map(|f| f.incident_key.clone()).collect();
    let mut current_leaf_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for finding in findings {
        if let Some(leaf_id) = finding.recovery_issue_id {
            current_leaf_keys.insert(build_issue_graph_liveness_leaf_key(LeafKeyInput {
                company_id: &finding.company_id.to_string(),
                state: finding.state.as_str(),
                leaf_issue_id: &leaf_id.to_string(),
            }));
        }
    }

    // 2. 列出 open escalation issues
    let open_recoveries = sqlx::query(
        "SELECT id, company_id, origin_id FROM issues \
         WHERE origin_kind = $1 \
           AND hidden_at IS NULL \
           AND status::text != ALL($2)",
    )
    .bind(ESCALATION_ORIGIN_KIND)
    .bind(TERMINAL_STATUSES)
    .fetch_all(db.pool())
    .await?;

    for row in open_recoveries {
        let recovery_id: Uuid = row.try_get("id")?;
        let recovery_company_id: Uuid = row.try_get("company_id")?;
        let origin_id: Option<String> = row.try_get("origin_id").ok();
        let origin_id_str = match origin_id {
            Some(s) => s,
            None => continue,
        };

        // 命中 current incident_key → skip
        if current_incident_keys.contains(&origin_id_str) {
            continue;
        }

        // parse 失败 → skip
        let parsed = match parse_issue_graph_liveness_incident_key(Some(&origin_id_str)) {
            Some(p) => p,
            None => continue,
        };
        let parsed_company = match Uuid::parse_str(&parsed.company_id) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let parsed_issue = match Uuid::parse_str(&parsed.issue_id) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let parsed_leaf = match Uuid::parse_str(&parsed.leaf_issue_id) {
            Ok(u) => u,
            Err(_) => continue,
        };

        // 命中 current leaf_key → skip
        let leaf_key = build_issue_graph_liveness_leaf_key(LeafKeyInput {
            company_id: &parsed_company.to_string(),
            state: &parsed.state,
            leaf_issue_id: &parsed_leaf.to_string(),
        });
        if current_leaf_keys.contains(&leaf_key) {
            continue;
        }

        // 查 source issue 状态
        let source_row: Option<(String,)> =
            sqlx::query_as("SELECT status::text FROM issues WHERE company_id = $1 AND id = $2")
                .bind(parsed_company)
                .bind(parsed_issue)
                .fetch_optional(db.pool())
                .await?;
        let source_status = source_row.map(|(s,)| s);

        if let Some(status) = source_status {
            if !TERMINAL_STATUSES.contains(&status.as_str()) {
                // source issue 还活着 → 检查 blocker chain
                let blocker_ids =
                    existing_blocker_issue_ids(db, parsed_company, parsed_issue).await?;
                if blocker_ids.contains(&recovery_id) {
                    result.active_skipped += 1;
                    continue;
                }
            }
        }

        // remove blocker relation
        if remove_recovery_blocker_from_source(db, recovery_company_id, recovery_id).await? {
            result.blocker_relations_removed += 1;
        }

        // 检查 active run
        if has_active_run_for_issue_id(db, recovery_company_id, recovery_id).await? {
            result.active_skipped += 1;
            continue;
        }

        // update status='cancelled'
        sqlx::query("UPDATE issues SET status = 'cancelled', updated_at = now() WHERE id = $1")
            .bind(recovery_id)
            .execute(db.pool())
            .await?;
        result.retired += 1;
        result.retired_issue_ids.push(recovery_id);
    }

    Ok(result)
}

/// 清理 done/cancelled escalation issues 的 blocker relations。
///
/// 与 Node `retireDoneLivenessRecoveryBlockers` 对齐：移除所有 closed recoveries
/// 在 source issue 上的 'blocks' 关系。
pub async fn retire_done_liveness_recovery_blockers(
    db: &Db,
) -> sqlx::Result<RetireDoneBlockersResult> {
    let mut result = RetireDoneBlockersResult::default();
    let closed_recoveries = sqlx::query(
        "SELECT id, company_id, origin_id FROM issues \
         WHERE origin_kind = $1 \
           AND hidden_at IS NULL \
           AND status::text = ANY($2)",
    )
    .bind(ESCALATION_ORIGIN_KIND)
    .bind(TERMINAL_STATUSES)
    .fetch_all(db.pool())
    .await?;
    for row in closed_recoveries {
        let recovery_id: Uuid = row.try_get("id")?;
        let recovery_company_id: Uuid = row.try_get("company_id")?;
        if remove_recovery_blocker_from_source(db, recovery_company_id, recovery_id).await? {
            result.blocker_relations_removed += 1;
        }
    }
    Ok(result)
}

// ============================================================================
// Lower-level helpers
// ============================================================================

/// 移除某 recovery issue 在 source 上的 'blocks' 关系（与 Node `removeRecoveryBlockerFromSource` 对齐）。
///
/// 返回是否实际移除了关系（false = source 不存在 / blocker chain 不包含）。
async fn remove_recovery_blocker_from_source(
    db: &Db,
    recovery_company_id: Uuid,
    recovery_id: Uuid,
) -> sqlx::Result<bool> {
    // 先查 recovery 的 origin_id → parse → source issue id
    let origin_row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT origin_id FROM issues WHERE id = $1 AND company_id = $2")
            .bind(recovery_id)
            .bind(recovery_company_id)
            .fetch_optional(db.pool())
            .await?;
    let origin_id = match origin_row {
        Some((Some(s),)) => s,
        _ => return Ok(false),
    };
    let parsed = match parse_issue_graph_liveness_incident_key(Some(&origin_id)) {
        Some(p) => p,
        None => return Ok(false),
    };
    let parsed_company = match Uuid::parse_str(&parsed.company_id) {
        Ok(u) => u,
        Err(_) => return Ok(false),
    };
    let parsed_issue = match Uuid::parse_str(&parsed.issue_id) {
        Ok(u) => u,
        Err(_) => return Ok(false),
    };

    // 检查 source 存在
    let source_exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM issues WHERE company_id = $1 AND id = $2")
            .bind(parsed_company)
            .bind(parsed_issue)
            .fetch_optional(db.pool())
            .await?;
    if source_exists.is_none() {
        return Ok(false);
    }

    // 检查 blocker chain
    let blocker_ids = existing_blocker_issue_ids(db, parsed_company, parsed_issue).await?;
    if !blocker_ids.contains(&recovery_id) {
        return Ok(false);
    }

    // 删除 issue_relations 行
    let result = sqlx::query(
        "DELETE FROM issue_relations \
         WHERE company_id = $1 AND issue_id = $2 AND related_issue_id = $3 AND type = 'blocks'",
    )
    .bind(parsed_company)
    .bind(recovery_id)
    .bind(parsed_issue)
    .execute(db.pool())
    .await?;
    Ok(result.rows_affected() > 0)
}

/// 检查 issue 是否还有 active run（与 Node `hasActiveRunForIssueId` 对齐）。
///
/// 检查两条路径：
/// 1. heartbeat_runs.context_snapshot 中 issueId 或 taskId = issue_id
/// 2. issues.execution_run_id 指向 active heartbeat_run
async fn has_active_run_for_issue_id(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<bool> {
    // 路径 1: context_snapshot
    let row1: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM heartbeat_runs \
         WHERE company_id = $1 \
           AND status::text = ANY($2) \
           AND (context_snapshot->>'issueId' = $3 OR context_snapshot->>'taskId' = $3) \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(ACTIVE_RUN_STATUSES)
    .bind(issue_id.to_string())
    .fetch_optional(db.pool())
    .await?;
    if row1.is_some() {
        return Ok(true);
    }

    // 路径 2: issues.execution_run_id
    let row2: Option<(Uuid,)> = sqlx::query_as(
        "SELECT hr.id FROM issues i \
         INNER JOIN heartbeat_runs hr ON hr.id = i.execution_run_id \
         WHERE i.company_id = $1 \
           AND i.id = $2 \
           AND hr.status::text = ANY($3) \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(ACTIVE_RUN_STATUSES)
    .fetch_optional(db.pool())
    .await?;
    Ok(row2.is_some())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::issue_graph_liveness::{
        IssueLivenessDependencyPathEntry, IssueLivenessSeverity, IssueLivenessState,
    };
    use chrono::TimeZone;

    fn make_finding(company_id: Uuid, path_ids: &[Uuid]) -> IssueLivenessFinding {
        IssueLivenessFinding {
            company_id,
            incident_key: format!(
                "harness_liveness:{company_id}:{}:stuck:x",
                path_ids.first().copied().unwrap_or_default()
            ),
            state: IssueLivenessState::BlockedByUnassignedIssue,
            severity: IssueLivenessSeverity::Warning,
            source_issue_id: path_ids.first().copied().unwrap_or_default(),
            source_issue_label: "test".to_string(),
            reason: "test".to_string(),
            dependency_path: path_ids
                .iter()
                .map(|id| IssueLivenessDependencyPathEntry {
                    issue_id: *id,
                    identifier: None,
                    title: format!("issue-{id}"),
                    status: "todo".to_string(),
                })
                .collect(),
            recovery_issue_id: path_ids.first().copied(),
            blocker_issue_id: None,
            participant_agent_id: None,
            recommended_owner_agent_id: None,
            recommended_owner_candidate_agent_ids: vec![],
            recommended_owner_candidates: vec![],
            recommended_action: "test".to_string(),
        }
    }

    #[test]
    fn normalize_lookback_clamps_to_range() {
        assert_eq!(
            normalize_lookback_hours(None),
            DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS
        );
        assert_eq!(
            normalize_lookback_hours(Some(0)),
            MIN_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS
        );
        assert_eq!(
            normalize_lookback_hours(Some(-100)),
            MIN_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS
        );
        assert_eq!(normalize_lookback_hours(Some(48)), 48);
        assert_eq!(
            normalize_lookback_hours(Some(100_000)),
            MAX_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS
        );
    }

    #[test]
    fn dependency_issue_key_format() {
        let co = Uuid::nil();
        let id = Uuid::nil();
        assert_eq!(
            liveness_dependency_issue_key(co, id),
            "00000000-0000-0000-0000-000000000000:00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn latest_updated_at_returns_none_for_empty_path() {
        let co = Uuid::new_v4();
        let finding = make_finding(co, &[]);
        let map = std::collections::HashMap::new();
        assert!(latest_dependency_updated_at_for_finding(&finding, &map).is_none());
    }

    #[test]
    fn latest_updated_at_returns_none_when_issue_missing_from_map() {
        let co = Uuid::new_v4();
        let id = Uuid::new_v4();
        let finding = make_finding(co, &[id]);
        let map = std::collections::HashMap::new();
        assert!(latest_dependency_updated_at_for_finding(&finding, &map).is_none());
    }

    #[test]
    fn latest_updated_at_returns_max_timestamp() {
        let co = Uuid::new_v4();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        let finding = make_finding(co, &[id1, id2, id3]);
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2026, 1, 3, 0, 0, 0).unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(liveness_dependency_issue_key(co, id1), t1);
        map.insert(liveness_dependency_issue_key(co, id2), t2);
        map.insert(liveness_dependency_issue_key(co, id3), t3);
        assert_eq!(
            latest_dependency_updated_at_for_finding(&finding, &map),
            Some(t3)
        );
    }

    #[test]
    fn is_inside_lookback_returns_true_for_recent() {
        let co = Uuid::new_v4();
        let id = Uuid::new_v4();
        let finding = make_finding(co, &[id]);
        let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2026, 8, 7, 11, 0, 0).unwrap(); // 1h ago
        let cutoff = Utc.with_ymd_and_hms(2026, 8, 7, 0, 0, 0).unwrap(); // 12h ago
        let mut map = std::collections::HashMap::new();
        map.insert(liveness_dependency_issue_key(co, id), recent);
        assert!(is_finding_inside_auto_recovery_lookback(
            &finding, cutoff, &map
        ));
        // Drop map → returns false
        assert!(!is_finding_inside_auto_recovery_lookback(
            &finding,
            cutoff,
            &std::collections::HashMap::new()
        ));
        let _ = now;
    }

    #[test]
    fn lookback_constants_match_node() {
        assert_eq!(
            DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS,
            24
        );
        assert_eq!(MIN_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS, 1);
        assert_eq!(
            MAX_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS,
            24 * 30
        );
    }
}
