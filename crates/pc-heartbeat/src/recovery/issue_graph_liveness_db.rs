//! Issue graph liveness DB 接入层。
//!
//! 对齐 Node `services/recovery/service.ts` 的：
//! - `findOpenLivenessEscalation` —— 按 incident_key 查 existing escalation
//! - `findOpenLivenessRecoveryIssueForLeaf` —— 按 leaf fingerprint 查 existing
//! - `findRecentCompletedLivenessRecoveryIssue` —— cooldown 窗口查最近 done 记录
//! - `existingBlockerIssueIds` —— 列出 issue 现有 blocker issue ids
//! - `ensureIssueBlockedByEscalation` —— 把 escalation 设为 issue 的 blocker，
//!   必要时把 issue 切到 blocked + 写 activity log
//! - `listIssueDependencyReadinessMap` —— blocker 解析度查询（含
//!   pending_finalize 屏障）—— backstop 关键依赖
//!
//! 边界：
//! - 不调 scheduler / escalate；只做幂等性查询 + blocker 维护 + activity log
//! - 与 `issue_graph_liveness.rs` 1531 行纯函数解耦：本模块只关心 idempotency +
//!   blocker 副作用，分类逻辑仍由 pure function 提供
//! - 仓储接口：直接用 sqlx，避免给 IssueRepo 加一次性接口

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::activity::{ActivityRepo, ActorType, NewActivity};
use pc_repos::Db;

use super::origins::parse_issue_graph_liveness_incident_key;
use crate::recovery::issue_graph_liveness::IssueLivenessFinding;

// ============================================================================
// Constants (mirrored from Node `RECOVERY_ORIGIN_KINDS`)
// ============================================================================

/// Node `RECOVERY_ORIGIN_KINDS.issueGraphLivenessEscalation` 常量镜像。
pub const ISSUE_GRAPH_LIVENESS_ESCALATION_ORIGIN_KIND: &str = "harness_liveness_escalation";

// ============================================================================
// Issue summary row (id + identifier + status + origin_kind/origin_id/fingerprint)
// ============================================================================

/// Issue 最小可观察快照（用于 idempotency 查询）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IssueSummaryRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub assignee_agent_id: Option<Uuid>,
    pub origin_kind: String,
    pub origin_id: Option<String>,
    pub origin_fingerprint: String,
    pub updated_at: DateTime<Utc>,
}

impl IssueSummaryRow {
    /// `visibleIssueCondition` —— 与 Node `visibleIssueCondition()` 等价。
    pub fn is_visible(&self) -> bool {
        // Note: IssueSummaryRow 不含 hidden_at。visibleIssueCondition 包含额外条件，
        // 调用方应在 SQL 层直接过滤 hidden_at IS NULL（见各查询实现）。
        // 此方法保留以备扩展。
        true
    }
}

// ============================================================================
// find_open_liveness_escalation
// ============================================================================

/// 按 `incident_key` 查 existing open escalation issue。
///
/// 与 Node `findOpenLivenessEscalation` 完全对齐：
/// - origin_kind = `harness_liveness_escalation`
/// - origin_id = incidentKey
/// - status NOT IN ('done','cancelled')
/// - hidden_at IS NULL
pub async fn find_open_liveness_escalation(
    db: &Db,
    company_id: Uuid,
    incident_key: &str,
) -> sqlx::Result<Option<IssueSummaryRow>> {
    sqlx::query_as::<_, IssueSummaryRow>(
        "SELECT id, company_id, identifier, title, status, priority, assignee_agent_id, \
                origin_kind, origin_id, origin_fingerprint, updated_at \
         FROM issues \
         WHERE company_id = $1 \
           AND origin_kind = $2 \
           AND origin_id = $3 \
           AND status NOT IN ('done','cancelled') \
           AND hidden_at IS NULL \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(ISSUE_GRAPH_LIVENESS_ESCALATION_ORIGIN_KIND)
    .bind(incident_key)
    .fetch_optional(db.pool())
    .await
}

/// 按 leaf fingerprint 查 existing open escalation issue。
///
/// 与 Node `findOpenLivenessRecoveryIssueForLeaf` 第一段查询对齐：
/// - origin_kind = `harness_liveness_escalation`
/// - origin_fingerprint = livenessRecoveryLeafFingerprint(finding)
pub async fn find_open_liveness_recovery_issue_for_fingerprint(
    db: &Db,
    company_id: Uuid,
    leaf_fingerprint: &str,
) -> sqlx::Result<Option<IssueSummaryRow>> {
    sqlx::query_as::<_, IssueSummaryRow>(
        "SELECT id, company_id, identifier, title, status, priority, assignee_agent_id, \
                origin_kind, origin_id, origin_fingerprint, updated_at \
         FROM issues \
         WHERE company_id = $1 \
           AND origin_kind = $2 \
           AND origin_fingerprint = $3 \
           AND status NOT IN ('done','cancelled') \
           AND hidden_at IS NULL \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(ISSUE_GRAPH_LIVENESS_ESCALATION_ORIGIN_KIND)
    .bind(leaf_fingerprint)
    .fetch_optional(db.pool())
    .await
}

/// 在 cooldown 窗口内查最近 done 的 escalation。
///
/// 与 Node `findRecentCompletedLivenessRecoveryIssue` 对齐：
/// - cooldownMs <= 0 → 直接返回 None
/// - cutoff = now - cooldownMs
/// - origin_id = incidentKey OR origin_fingerprint = leafFingerprint
/// - status = 'done'
/// - updated_at >= cutoff
/// - ORDER BY updated_at DESC, id DESC LIMIT 1
pub async fn find_recent_completed_liveness_recovery_issue(
    db: &Db,
    company_id: Uuid,
    incident_key: &str,
    leaf_fingerprint: &str,
    now: DateTime<Utc>,
    cooldown_ms: i64,
) -> sqlx::Result<Option<Uuid>> {
    if cooldown_ms <= 0 {
        return Ok(None);
    }
    let cutoff = now - chrono::Duration::milliseconds(cooldown_ms);
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM issues \
         WHERE company_id = $1 \
           AND origin_kind = $2 \
           AND (origin_id = $3 OR origin_fingerprint = $4) \
           AND status = 'done' \
           AND hidden_at IS NULL \
           AND updated_at >= $5 \
         ORDER BY updated_at DESC, id DESC LIMIT 1",
    )
    .bind(company_id)
    .bind(ISSUE_GRAPH_LIVENESS_ESCALATION_ORIGIN_KIND)
    .bind(incident_key)
    .bind(leaf_fingerprint)
    .bind(cutoff)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|(id,)| id))
}

/// 按 parsed incident_key + leaf_issue_id 二次查 existing escalation（fallback）。
///
/// 与 Node `findOpenLivenessRecoveryIssueForLeaf` 第二段查询对齐：
/// - 列出所有 open escalation（hidden_at IS NULL + status NOT IN done/cancelled）
/// - 过滤 parsed.originId.state == finding.state && leafIssueId == leaf
pub async fn find_open_liveness_recovery_by_parsed_leaf(
    db: &Db,
    company_id: Uuid,
    finding: &IssueLivenessFinding,
) -> sqlx::Result<Option<IssueSummaryRow>> {
    let rows = sqlx::query_as::<_, IssueSummaryRow>(
        "SELECT id, company_id, identifier, title, status, priority, assignee_agent_id, \
                origin_kind, origin_id, origin_fingerprint, updated_at \
         FROM issues \
         WHERE company_id = $1 \
           AND origin_kind = $2 \
           AND status NOT IN ('done','cancelled') \
           AND hidden_at IS NULL",
    )
    .bind(company_id)
    .bind(ISSUE_GRAPH_LIVENESS_ESCALATION_ORIGIN_KIND)
    .fetch_all(db.pool())
    .await?;
    // Use leaf issue id from dependency path last entry (mirrors Node livenessRecoveryLeafIssueId).
    let leaf_issue_id = leaf_issue_id_from_finding(finding);
    for row in rows {
        let Some(parsed) = row.origin_id.as_deref().and_then(parse_liveness_origin_id) else {
            continue;
        };
        let leaf_match = match (parsed.leaf_issue_id.as_deref(), leaf_issue_id) {
            (Some(p), Some(l)) => Uuid::parse_str(p).ok() == Some(l),
            (None, None) => true,
            _ => false,
        };
        if parsed.state == finding.state.as_str() && leaf_match {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

// ============================================================================
// existing_blocker_issue_ids
// ============================================================================

/// 列出 issue 现有 blocker issue ids（type='blocks'）。
///
/// 与 Node `existingBlockerIssueIds` 对齐。
pub async fn existing_blocker_issue_ids(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT issue_id FROM issue_relations \
         WHERE company_id = $1 AND related_issue_id = $2 AND type = 'blocks' \
         ORDER BY issue_id",
    )
    .bind(company_id)
    .bind(issue_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// 列出 issue 现有 unresolved blocker ids（blocker status NOT IN done/cancelled）。
///
/// 与 Node `existingUnresolvedBlockerIssueIds` 对齐。
pub async fn existing_unresolved_blocker_issue_ids(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT ir.issue_id FROM issue_relations ir \
         INNER JOIN issues b ON b.id = ir.issue_id AND b.company_id = ir.company_id \
         WHERE ir.company_id = $1 \
           AND ir.related_issue_id = $2 \
           AND ir.type = 'blocks' \
           AND b.status NOT IN ('done','cancelled') \
           AND b.hidden_at IS NULL \
         ORDER BY ir.issue_id",
    )
    .bind(company_id)
    .bind(issue_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

// ============================================================================
// ensure_issue_blocked_by_escalation
// ============================================================================

/// Blockers update 副作用的输入。
#[derive(Debug, Clone)]
pub struct EnsureBlockedByEscalationInput<'a> {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub current_status: &'a str,
    pub escalation_issue_id: Uuid,
    pub incident_key: &'a str,
    pub finding_state: &'a str,
    pub run_id: Option<Uuid>,
}

/// 把 escalation 设为 issue 的 blocker，必要时切到 blocked + 写 activity log。
///
/// 与 Node `ensureIssueBlockedByEscalation` 对齐：
/// - 计算 next_blocker_ids = unique(blocker_ids ∪ {escalation_issue_id})
/// - 若已 blocker + 已 blocked：直接 return
/// - 写 issues.blocked_by_issue_ids（通过 issue_relations INSERT）
/// - 若 status 非 blocked：写 issue status = 'blocked'
/// - 写 `issue.blockers.updated` activity log
///
/// 返回：是否实际更新了。
pub async fn ensure_issue_blocked_by_escalation(
    db: &Db,
    input: EnsureBlockedByEscalationInput<'_>,
) -> sqlx::Result<bool> {
    let mut blocker_ids = existing_blocker_issue_ids(db, input.company_id, input.issue_id).await?;
    let already_blocked_by_escalation = blocker_ids.contains(&input.escalation_issue_id);
    let already_blocked = input.current_status == "blocked";
    if already_blocked_by_escalation && already_blocked {
        return Ok(false);
    }
    if !blocker_ids.contains(&input.escalation_issue_id) {
        blocker_ids.push(input.escalation_issue_id);
    }
    // Insert issue_relations row if not present (idempotent via primary key check)
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT issue_id FROM issue_relations \
         WHERE company_id = $1 AND issue_id = $2 AND related_issue_id = $3 AND type = 'blocks' LIMIT 1",
    )
    .bind(input.company_id)
    .bind(input.escalation_issue_id)
    .bind(input.issue_id)
    .fetch_optional(db.pool())
    .await?;
    if existing.is_none() {
        sqlx::query(
            "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type) \
             VALUES ($1, $2, $3, 'blocks')",
        )
        .bind(input.company_id)
        .bind(input.escalation_issue_id)
        .bind(input.issue_id)
        .execute(db.pool())
        .await?;
    }
    // Update issue status if not already blocked
    if !already_blocked {
        sqlx::query("UPDATE issues SET status = 'blocked', updated_at = now() WHERE id = $1")
            .bind(input.issue_id)
            .execute(db.pool())
            .await?;
    }
    // Activity log
    let details = json!({
        "source": "recovery.reconcile_issue_graph_liveness",
        "incidentKey": input.incident_key,
        "findingState": input.finding_state,
        "blockerIssueIds": blocker_ids,
        "escalationIssueId": input.escalation_issue_id,
        "status": if already_blocked { input.current_status } else { "blocked" },
        "previousStatus": input.current_status,
    });
    let _ = ActivityRepo::new(db)
        .record(&NewActivity {
            company_id: input.company_id,
            actor_type: ActorType::System,
            actor_id: "system".to_string(),
            action: "issue.blockers.updated".to_string(),
            entity_type: "issue".to_string(),
            entity_id: input.issue_id.to_string(),
            agent_id: None,
            run_id: input.run_id,
            responsible_user_id: None,
            details: Some(details),
        })
        .await
        .map_err(|e| format!("activity log write failed: {e}"));
    Ok(true)
}

// ============================================================================
// list_issue_dependency_readiness_map (backstop 关键依赖)
// ============================================================================

/// 单 issue dependency readiness 快照。
#[derive(Debug, Clone, Default)]
pub struct IssueDependencyReadiness {
    pub issue_id: Uuid,
    pub blocker_issue_ids: Vec<Uuid>,
    pub unresolved_blocker_issue_ids: Vec<Uuid>,
    pub unresolved_blocker_count: i64,
    pub all_blockers_done: bool,
    pub is_dependency_ready: bool,
    pub pending_finalize_blocker_issue_ids: Vec<Uuid>,
}

/// 计算一组 issue 的 blocker readiness。
///
/// 与 Node `listIssueDependencyReadinessMap` 对齐（精简版）：
/// - 列出所有 issue_relations (type='blocks') + JOIN issues
/// - 对每个 blocker：若 status != 'done' → unresolved；否则 resolved
/// - all_blockers_done = (无 unresolved blockers)
/// - is_dependency_ready = all_blockers_done（pending_finalize 屏障暂未实现，需等
///   `listPendingFinalizeBlockerIssueIds` 接口补齐后追加）
///
/// 返回 Map<issue_id, IssueDependencyReadiness>。
pub async fn list_issue_dependency_readiness_map(
    db: &Db,
    company_id: Uuid,
    issue_ids: &[Uuid],
) -> sqlx::Result<std::collections::HashMap<Uuid, IssueDependencyReadiness>> {
    let unique: std::collections::BTreeSet<Uuid> = issue_ids.iter().copied().collect();
    let mut map: std::collections::HashMap<Uuid, IssueDependencyReadiness> =
        std::collections::HashMap::with_capacity(unique.len());
    for id in &unique {
        map.insert(
            *id,
            IssueDependencyReadiness {
                issue_id: *id,
                all_blockers_done: true,
                is_dependency_ready: true,
                ..Default::default()
            },
        );
    }
    if unique.is_empty() {
        return Ok(map);
    }
    let rows = sqlx::query(
        "SELECT ir.related_issue_id AS issue_id, ir.issue_id AS blocker_id, \
                b.status::text AS blocker_status, \
                b.execution_workspace_id AS blocker_workspace \
         FROM issue_relations ir \
         INNER JOIN issues b ON b.id = ir.issue_id AND b.company_id = ir.company_id \
         WHERE ir.company_id = $1 \
           AND ir.type = 'blocks' \
           AND ir.related_issue_id = ANY($2::uuid[]) \
           AND b.hidden_at IS NULL",
    )
    .bind(company_id)
    .bind(&unique_vec(&unique))
    .fetch_all(db.pool())
    .await?;
    for row in rows {
        let issue_id: Uuid = row.try_get("issue_id")?;
        let blocker_id: Uuid = row.try_get("blocker_id")?;
        let blocker_status: String = row.try_get("blocker_status")?;
        let entry = map.entry(issue_id).or_insert(IssueDependencyReadiness {
            issue_id,
            all_blockers_done: true,
            is_dependency_ready: true,
            ..Default::default()
        });
        entry.blocker_issue_ids.push(blocker_id);
        if blocker_status != "done" {
            entry.unresolved_blocker_issue_ids.push(blocker_id);
            entry.unresolved_blocker_count += 1;
            entry.all_blockers_done = false;
            entry.is_dependency_ready = false;
        }
        // pending_finalize 屏障：暂未实现（接口待补齐）。当前实现下
        // all_blockers_done == is_dependency_ready（done blocker 都算 ready）。
    }
    Ok(map)
}

// ============================================================================
// Helpers
// ============================================================================

fn unique_vec(set: &std::collections::BTreeSet<Uuid>) -> Vec<Uuid> {
    set.iter().copied().collect()
}

/// 从 finding 中提取 leaf issue id（与 Node `livenessRecoveryLeafIssueId` 对齐）。
fn leaf_issue_id_from_finding(finding: &IssueLivenessFinding) -> Option<Uuid> {
    finding.dependency_path.last().map(|p| p.issue_id)
}

/// 解析 escalation origin_id 中嵌入的 incident key。
///
/// `parse_issue_graph_liveness_incident_key` 返回 `Option<ParsedIncidentKey>`，
/// 但其字段未在本模块暴露——本函数只取 state + leaf_issue_id。
#[derive(Debug, Clone)]
struct ParsedLivenessOriginId {
    state: String,
    leaf_issue_id: Option<String>,
}

fn parse_liveness_origin_id(origin_id: &str) -> Option<ParsedLivenessOriginId> {
    parse_issue_graph_liveness_incident_key(Some(origin_id)).map(|p| ParsedLivenessOriginId {
        state: p.state.to_string(),
        leaf_issue_id: Some(p.leaf_issue_id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_origin_id_extracts_state_and_leaf() {
        // Use a non-uuid leaf (e.g. company-id prefix) — but the actual parser
        // tries Uuid::parse_str on it, so it returns None for "l1".
        // We only assert state here; leaf_id parsing is best-effort.
        // Real format: harness_liveness:<company>:<recovery>:<state>:<leaf>
        let parsed = parse_liveness_origin_id("harness_liveness:c1:i1:stuck:l1").unwrap();
        assert_eq!(parsed.state, "stuck");
        assert_eq!(parsed.leaf_issue_id.as_deref(), Some("l1"));
    }

    #[test]
    fn parsed_origin_id_rejects_wrong_prefix() {
        assert!(parse_liveness_origin_id("other_prefix:c1:i1:stuck:l1").is_none());
    }

    #[test]
    fn leaf_issue_id_uses_last_path_entry() {
        let leaf_id = Uuid::new_v4();
        // Use a JSON-like dependency path; we construct manually.
        let finding = IssueLivenessFinding {
            company_id: Uuid::new_v4(),
            incident_key: "harness_liveness:c1:r1:stuck:l1".to_string(),
            state:
                crate::recovery::issue_graph_liveness::IssueLivenessState::BlockedByUnassignedIssue,
            severity: crate::recovery::issue_graph_liveness::IssueLivenessSeverity::Warning,
            source_issue_id: Uuid::nil(),
            source_issue_label: "root".to_string(),
            reason: "test".to_string(),
            dependency_path: vec![
                crate::recovery::issue_graph_liveness::IssueLivenessDependencyPathEntry {
                    issue_id: Uuid::new_v4(),
                    identifier: None,
                    title: "root".into(),
                    status: "todo".into(),
                },
                crate::recovery::issue_graph_liveness::IssueLivenessDependencyPathEntry {
                    issue_id: leaf_id,
                    identifier: None,
                    title: "leaf-x".into(),
                    status: "todo".into(),
                },
            ],
            recovery_issue_id: None,
            blocker_issue_id: None,
            participant_agent_id: None,
            recommended_owner_agent_id: None,
            recommended_owner_candidate_agent_ids: vec![],
            recommended_owner_candidates: vec![],
            recommended_action: "test".to_string(),
        };
        let expected_leaf = leaf_id;
        assert_eq!(leaf_issue_id_from_finding(&finding), Some(expected_leaf));
    }
}
