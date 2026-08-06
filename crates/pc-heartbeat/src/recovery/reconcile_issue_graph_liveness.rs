//! `reconcileIssueGraphLiveness` 顶级编排器。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `reconcileIssueGraphLiveness`：
//! - 收集 findings（`collect_issue_graph_liveness_findings`）
//! - 可选 `issueCreatedAtGte` 过滤
//! - 决定 `autoRecoveryEnabled`（force 或设置）
//! - 计算 `cutoff` = `now - lookbackHours*1h`
//! - 清理 obsolete / done-blockers（`retire_*`）
//! - 加载 dependency updated_at map（`load_*`）
//! - 跑 resolved-dependency-wake backstop
//! - 对每个 finding：lookback 判定 + 调 `create_issue_graph_liveness_escalation`
//!
//! 设计：
//! - 主入口函数 `reconcile_issue_graph_liveness(db, opts)`：胶水代码
//! - 输出 `ReconcileIssueGraphLivenessResult` 包含所有计数器（与 Node 对齐字段名）
//! - 默认值常量集中在文件顶部（与 Node 字段名 1:1 对齐）
//!
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use pc_repos::Db;

use super::collect_issue_graph_liveness_findings::{
    collect_issue_graph_liveness_findings, CollectFindingsOptions,
};
use super::escalation_creation::{
    create_issue_graph_liveness_escalation, CreateEscalationInput, EscalationOutcome,
    DEFAULT_REESCALATION_COOLDOWN_MS,
};
use super::issue_graph_liveness::IssueLivenessFinding;
use super::liveness_dependency_cleanup::{
    is_finding_inside_auto_recovery_lookback, load_liveness_dependency_updated_at_by_issue,
    retire_done_liveness_recovery_blockers, retire_obsolete_liveness_recovery_issues,
    RetireDoneBlockersResult, RetireObsoleteResult,
    DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS,
};
use super::resolved_dependency_wake_backstop::{
    reconcile_resolved_dependency_wake_backstop, ResolvedDependencyWakeBackstopOptions,
    ResolvedDependencyWakeBackstopResult,
};

// ============================================================================
// Public types
// ============================================================================

/// `reconcile_issue_graph_liveness` 的输入选项。
///
/// 与 Node `ReconcileIssueGraphLivenessOptions` 对齐（精简版）。
#[derive(Debug, Clone, Default)]
pub struct ReconcileIssueGraphLivenessOptions {
    /// 调用方提供的 run_id（会传给 `create_issue_graph_liveness_escalation` 和 backstop）。
    pub run_id: Option<Uuid>,
    /// Force enable auto recovery（无视 instance settings）。
    pub force: bool,
    /// 自定义 lookback 小时数。None → 使用默认 DEFAULT_LOOKBACK_HOURS。
    pub lookback_hours: Option<i64>,
    /// 仅处理 created_at >= 此时间的 source/recovery issues。
    pub issue_created_at_gte: Option<DateTime<Utc>>,
    /// 自定义 now（用于测试）。None → 使用 chrono::Utc::now()。
    pub now: Option<DateTime<Utc>>,
    /// 自定义 reescalation cooldown 毫秒。None → 使用 `DEFAULT_REESCALATION_COOLDOWN_MS` (1 hour)。
    pub reescalation_cooldown_ms: Option<i64>,
    /// `auto_recovery_enabled` override：Some(true) → enabled；Some(false) → disabled；None → 默认 true。
    pub auto_recovery_enabled: Option<bool>,
    /// 单次扫描的 company 过滤（None → 全部）。
    pub company_id: Option<Uuid>,
}

/// `reconcile_issue_graph_liveness` 的输出结果。
///
/// 字段名与 Node `IssueGraphLivenessReconcileResult` 1:1 对齐。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileIssueGraphLivenessResult {
    pub findings: i64,
    pub auto_recovery_enabled: bool,
    pub lookback_hours: i64,
    pub cutoff: DateTime<Utc>,
    pub escalations_created: i64,
    pub existing_escalations: i64,
    pub skipped: i64,
    pub skipped_auto_recovery_disabled: i64,
    pub skipped_outside_lookback: i64,
    pub skipped_reescalation_cooldown: i64,
    pub obsolete_recoveries_retired: i64,
    pub obsolete_recoveries_active_skipped: i64,
    pub obsolete_recovery_blocker_relations_removed: i64,
    pub done_recovery_blocker_relations_removed: i64,
    pub dependency_wake_backstop_checked: i64,
    pub dependency_wakes_healed: i64,
    pub dependency_wake_existing_skipped: i64,
    pub dependency_wake_live_path_skipped: i64,
    pub dependency_wake_interaction_skipped: i64,
    pub dependency_wake_pause_hold_skipped: i64,
    pub dependency_wake_not_ready_skipped: i64,
    pub dependency_wake_candidate_limit_skipped: i64,
    pub dependency_wake_deferred_or_failed: i64,
    pub dependency_wake_enqueue_failed: i64,
    pub dependency_wake_issue_ids: Vec<Uuid>,
    pub issue_ids: Vec<Uuid>,
    pub escalation_issue_ids: Vec<Uuid>,
    pub retired_recovery_issue_ids: Vec<Uuid>,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 顶级编排器：collect → retire → load → backstop → escalate。
///
/// 与 Node `reconcileIssueGraphLiveness` 对齐（精简版：无 instance_settings 读取，
/// 通过 `auto_recovery_enabled` 选项强制指定）。
pub async fn reconcile_issue_graph_liveness(
    db: &Db,
    opts: ReconcileIssueGraphLivenessOptions,
) -> sqlx::Result<ReconcileIssueGraphLivenessResult> {
    let now = opts.now.unwrap_or_else(Utc::now);

    // 1. Collect findings
    let mut findings = collect_issue_graph_liveness_findings(
        db,
        CollectFindingsOptions {
            company_id: opts.company_id,
            issue_limit: None,
        },
    )
    .await?;

    // 2. 可选 issueCreatedAtGte 过滤
    if let Some(gte) = opts.issue_created_at_gte {
        findings = filter_findings_by_created_at(db, &findings, gte).await?;
    }

    // 3. 决定 auto_recovery_enabled（force 选项优先于 settings）
    let auto_recovery_enabled = opts.auto_recovery_enabled.unwrap_or(true) || opts.force;

    // 4. 决定 lookback_hours（opts 优先，否则 DEFAULT）
    let lookback_hours = opts
        .lookback_hours
        .unwrap_or(DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS);

    // 5. 计算 reescalation_cooldown_ms（opts 优先，否则 DEFAULT）
    let reescalation_cooldown_ms = opts
        .reescalation_cooldown_ms
        .unwrap_or(DEFAULT_REESCALATION_COOLDOWN_MS)
        .max(0);

    // 6. cutoff
    let cutoff = now - chrono::Duration::hours(lookback_hours);

    // 7. retire obsolete + retire done blockers
    let obsolete_recovery_cleanup: RetireObsoleteResult =
        retire_obsolete_liveness_recovery_issues(db, &findings).await?;
    let done_recovery_blocker_cleanup: RetireDoneBlockersResult =
        retire_done_liveness_recovery_blockers(db).await?;

    // 8. load dependency updated_at map
    let updated_at_by_issue_key =
        load_liveness_dependency_updated_at_by_issue(db, &findings).await?;

    // 9. 初始化 result
    let mut result = ReconcileIssueGraphLivenessResult {
        findings: findings.len() as i64,
        auto_recovery_enabled,
        lookback_hours,
        cutoff,
        obsolete_recoveries_retired: obsolete_recovery_cleanup.retired,
        obsolete_recoveries_active_skipped: obsolete_recovery_cleanup.active_skipped,
        obsolete_recovery_blocker_relations_removed: obsolete_recovery_cleanup
            .blocker_relations_removed,
        done_recovery_blocker_relations_removed: done_recovery_blocker_cleanup
            .blocker_relations_removed,
        retired_recovery_issue_ids: obsolete_recovery_cleanup.retired_issue_ids,
        ..Default::default()
    };

    // 10. 跑 resolved-dependency-wake backstop
    let backstop_result: ResolvedDependencyWakeBackstopResult =
        reconcile_resolved_dependency_wake_backstop(
            db,
            ResolvedDependencyWakeBackstopOptions {
                company_id: opts.company_id,
                blocker_issue_id: None,
                run_id: opts.run_id,
                source: None,
            },
        )
        .await?;
    apply_backstop_to_result(&mut result, &backstop_result);

    // 11. 若 auto_recovery disabled → 直接返回
    if !auto_recovery_enabled {
        result.skipped_auto_recovery_disabled = findings.len() as i64;
        return Ok(result);
    }

    // 12. 对每个 finding：lookback 判定 + escalate
    for finding in &findings {
        if !is_finding_inside_auto_recovery_lookback(finding, cutoff, &updated_at_by_issue_key) {
            result.skipped_outside_lookback += 1;
            result.skipped += 1;
            continue;
        }
        let outcome = create_issue_graph_liveness_escalation(
            db,
            CreateEscalationInput {
                company_id: finding.company_id,
                finding,
                run_id: opts.run_id,
                now,
                reescalation_cooldown_ms,
            },
        )
        .await?;
        apply_escalation_outcome(&mut result, finding.source_issue_id, &outcome);
    }

    Ok(result)
}

// ============================================================================
// Helpers
// ============================================================================

/// `opts.issueCreatedAtGte` 过滤：保留 recoveryIssueId 的 issue.created_at >= gte 的 findings。
async fn filter_findings_by_created_at(
    db: &Db,
    findings: &[IssueLivenessFinding],
    gte: DateTime<Utc>,
) -> sqlx::Result<Vec<IssueLivenessFinding>> {
    use std::collections::HashSet;
    let finding_issue_ids: HashSet<Uuid> = findings
        .iter()
        .filter_map(|f| f.recovery_issue_id)
        .collect();
    if finding_issue_ids.is_empty() {
        return Ok(vec![]);
    }
    let ids_vec: Vec<Uuid> = finding_issue_ids.iter().copied().collect();
    let rows = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM issues WHERE id = ANY($1::uuid[]) AND created_at >= $2",
    )
    .bind(&ids_vec)
    .bind(gte)
    .fetch_all(db.pool())
    .await?;
    let eligible: HashSet<Uuid> = rows.into_iter().map(|(id,)| id).collect();
    Ok(findings
        .iter()
        .filter(|f| {
            f.recovery_issue_id
                .map(|id| eligible.contains(&id))
                .unwrap_or(false)
        })
        .cloned()
        .collect())
}

/// 把 backstop 结果填入 reconcile result。
fn apply_backstop_to_result(
    result: &mut ReconcileIssueGraphLivenessResult,
    backstop: &ResolvedDependencyWakeBackstopResult,
) {
    result.dependency_wake_backstop_checked = backstop.checked;
    result.dependency_wakes_healed = backstop.healed;
    result.dependency_wake_existing_skipped = backstop.existing_wake_skipped;
    result.dependency_wake_live_path_skipped = backstop.live_path_skipped;
    result.dependency_wake_interaction_skipped = backstop.interaction_skipped;
    result.dependency_wake_pause_hold_skipped = backstop.pause_hold_skipped;
    result.dependency_wake_not_ready_skipped = backstop.not_ready_skipped;
    result.dependency_wake_candidate_limit_skipped = backstop.candidate_limit_skipped;
    result.dependency_wake_deferred_or_failed = backstop.deferred_or_failed;
    result.dependency_wake_enqueue_failed = backstop.enqueue_failed;
    result.dependency_wake_issue_ids = backstop.issue_ids.clone();
}

/// 把 escalation outcome 分类计入 reconcile result。
fn apply_escalation_outcome(
    result: &mut ReconcileIssueGraphLivenessResult,
    source_issue_id: Uuid,
    outcome: &EscalationOutcome,
) {
    match outcome {
        EscalationOutcome::Created {
            escalation_issue_id,
        } => {
            result.escalations_created += 1;
            result.issue_ids.push(source_issue_id);
            result.escalation_issue_ids.push(*escalation_issue_id);
        }
        EscalationOutcome::Existing {
            escalation_issue_id,
        } => {
            result.existing_escalations += 1;
            result.issue_ids.push(source_issue_id);
            result.escalation_issue_ids.push(*escalation_issue_id);
        }
        EscalationOutcome::Cooldown => {
            result.skipped_reescalation_cooldown += 1;
            result.skipped += 1;
        }
        EscalationOutcome::Skipped => {
            result.skipped += 1;
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_default_is_zero_everywhere() {
        let r = ReconcileIssueGraphLivenessResult::default();
        assert_eq!(r.findings, 0);
        assert_eq!(r.escalations_created, 0);
        assert_eq!(r.existing_escalations, 0);
        assert_eq!(r.skipped, 0);
        assert_eq!(r.dependency_wakes_healed, 0);
        assert!(r.issue_ids.is_empty());
        assert!(r.escalation_issue_ids.is_empty());
    }

    #[test]
    fn apply_backstop_copies_all_fields() {
        let mut result = ReconcileIssueGraphLivenessResult::default();
        let backstop = ResolvedDependencyWakeBackstopResult {
            checked: 5,
            healed: 2,
            existing_wake_skipped: 1,
            live_path_skipped: 1,
            interaction_skipped: 1,
            pause_hold_skipped: 0,
            not_ready_skipped: 0,
            candidate_limit_skipped: 0,
            deferred_or_failed: 0,
            enqueue_failed: 0,
            issue_ids: vec![Uuid::new_v4(), Uuid::new_v4()],
        };
        apply_backstop_to_result(&mut result, &backstop);
        assert_eq!(result.dependency_wake_backstop_checked, 5);
        assert_eq!(result.dependency_wakes_healed, 2);
        assert_eq!(result.dependency_wake_existing_skipped, 1);
        assert_eq!(result.dependency_wake_live_path_skipped, 1);
        assert_eq!(result.dependency_wake_interaction_skipped, 1);
        assert_eq!(result.dependency_wake_issue_ids.len(), 2);
    }

    #[test]
    fn apply_escalation_outcome_distinguishes_kinds() {
        let mut result = ReconcileIssueGraphLivenessResult::default();
        let src1 = Uuid::new_v4();
        let src2 = Uuid::new_v4();
        let src3 = Uuid::new_v4();
        let esc1 = Uuid::new_v4();
        let esc2 = Uuid::new_v4();

        apply_escalation_outcome(
            &mut result,
            src1,
            &EscalationOutcome::Created {
                escalation_issue_id: esc1,
            },
        );
        apply_escalation_outcome(
            &mut result,
            src2,
            &EscalationOutcome::Existing {
                escalation_issue_id: esc2,
            },
        );
        apply_escalation_outcome(&mut result, src3, &EscalationOutcome::Cooldown);

        assert_eq!(result.escalations_created, 1);
        assert_eq!(result.existing_escalations, 1);
        assert_eq!(result.skipped_reescalation_cooldown, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.issue_ids.len(), 2); // created + existing, NOT cooldown
        assert_eq!(result.escalation_issue_ids, vec![esc1, esc2]);
    }

    #[test]
    fn apply_escalation_outcome_skipped_increments_only_skipped() {
        let mut result = ReconcileIssueGraphLivenessResult::default();
        apply_escalation_outcome(&mut result, Uuid::new_v4(), &EscalationOutcome::Skipped);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.escalations_created, 0);
        assert_eq!(result.existing_escalations, 0);
        assert!(result.issue_ids.is_empty());
    }
}
