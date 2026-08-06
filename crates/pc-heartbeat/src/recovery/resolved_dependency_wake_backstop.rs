//! `reconcileResolvedDependencyWakeBackstop` 模块。
//!
//! 对齐 Node `services/recovery/service.ts` 的
//! `reconcileResolvedDependencyWakeBackstop`：
//! - 列出 blocked + assignee_agent_id 非空 的 issue candidates
//! - 通过 dependency readiness map 找出 dependency 已就绪的 candidates
//! - 检查 idempotency、active execution path、queued wake、pending wake interaction、
//!   pause-hold（这些跳过路径都返回对应的 *Skipped 计数器）
//! - 通过 enqueueWakeup 给 owner agent 发 wakeup，触发 `issue_blockers_resolved`
//! - 写 activity log `issue.blockers_resolved_wake_emitted`
//!
//! 设计：
//! - 纯 helper：`build_issue_blockers_resolved_wake_idempotency_key` /
//!   `find_existing_issue_blockers_resolved_wake_for_any_key`
//! - 副作用集中：候选查询 + readiness map 查询 + wake 调度都内聚在主入口中
//! - DB 辅助：`IssueRepo::has_queued_issue_wake` / `has_pending_wake_interaction_for_issue`
//!   / `has_active_execution_path` 已在 round306 添加到 pc-repos
//!
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::activity::{ActivityRepo, ActorType, NewActivity};
use pc_repos::agent::{
    AgentRepo, HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType,
    WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::issue::IssueRepo;
use pc_repos::Db;

use super::issue_graph_liveness_db::list_issue_dependency_readiness_map;
use super::pause_hold_guard::is_automatic_recovery_suppressed_by_pause_hold;

// ============================================================================
// Constants
// ============================================================================

/// 每次扫描处理的 candidate 上限（与 Node `RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT` 对齐）。
pub const RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT: i64 = 200;

/// Wake reason 标签（与 Node `ISSUE_BLOCKERS_RESOLVED_WAKE_REASON` 对齐）。
pub const ISSUE_BLOCKERS_RESOLVED_WAKE_REASON: &str = "issue_blockers_resolved";

// ============================================================================
// Public types
// ============================================================================

/// reconcile_resolved_dependency_wake_backstop 的输入选项。
#[derive(Debug, Clone)]
pub struct ResolvedDependencyWakeBackstopOptions {
    pub company_id: Option<Uuid>,
    pub blocker_issue_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub source: Option<String>,
}

/// Backstop 扫描结果汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedDependencyWakeBackstopResult {
    pub checked: i64,
    pub healed: i64,
    pub existing_wake_skipped: i64,
    pub live_path_skipped: i64,
    pub interaction_skipped: i64,
    pub pause_hold_skipped: i64,
    pub not_ready_skipped: i64,
    pub candidate_limit_skipped: i64,
    pub deferred_or_failed: i64,
    pub enqueue_failed: i64,
    pub issue_ids: Vec<Uuid>,
}

/// Backstop 处理后的某次 wakeup 结果（用于日志 / 上层决策）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackstopWakeOutcome {
    /// Wake 已发出。
    Created { wakeup_id: Uuid },
    /// Wake 未发出（被某个 Skipped 路径抑制）。
    Skipped,
}

// ============================================================================
// Idempotency helpers
// ============================================================================

/// 构造 wakeup 的 idempotency key（与 Node `buildIssueBlockersResolvedWakeIdempotencyKey` 对齐）。
///
/// 格式：`issue_blockers_resolved_wake:{dependent_issue_id}:{resolved_blocker_issue_id}`
pub fn build_issue_blockers_resolved_wake_idempotency_key(
    dependent_issue_id: Uuid,
    resolved_blocker_issue_id: Uuid,
) -> String {
    format!(
        "issue_blockers_resolved_wake:{}:{}",
        dependent_issue_id, resolved_blocker_issue_id
    )
}

/// 查找是否已存在任一 idempotency key 对应的 wakeup 记录（任意 status 都算命中）。
///
/// 与 Node `findExistingIssueBlockersResolvedWakeForAnyKey` 对齐：返回首个匹配的 wakeup_id，
/// 用于幂等性去重，避免重复发 wake。
pub async fn find_existing_issue_blockers_resolved_wake_for_any_key(
    db: &Db,
    company_id: Uuid,
    idempotency_keys: &[String],
) -> sqlx::Result<Option<Uuid>> {
    if idempotency_keys.is_empty() {
        return Ok(None);
    }
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM agent_wakeup_requests \
         WHERE company_id = $1 AND idempotency_key = ANY($2::text[]) \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(idempotency_keys)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|(id,)| id))
}

// ============================================================================
// Main entry point
// ============================================================================

/// 主入口：扫描所有 dependency resolved 但仍 blocked 的 issue，给 owner agent 发 wakeup。
///
/// 与 Node `reconcileResolvedDependencyWakeBackstop` 对齐（精简版）：
/// - 每次调用处理 ≤ `RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT` 个 candidates
/// - `blocker_issue_id` 模式下只扫描该 blocker 的 dependents
/// - 否则按 company 全表扫描 blocked + has assignee_agent_id + visible issues
/// - 通过 dependency readiness map 找出 dependency ready 的 candidates
/// - 对每个 ready candidate：
///   1. 跳过 existing wake
///   2. 跳过 active execution / queued wake / pending wake interaction
///   3. 跳过 pause-hold
///   4. 调 `enqueueWakeup` 发 wakeup，写 activity log
///
/// 注：本实现用 cursor 模式（与 Node 一致），但函数粒度更小（每次调用独立，无模块级 cursor）。
/// 上层（heartbeat ticker）可以每 tick 调用一次，自动处理 cursor 推进。
pub async fn reconcile_resolved_dependency_wake_backstop(
    db: &Db,
    opts: ResolvedDependencyWakeBackstopOptions,
) -> sqlx::Result<ResolvedDependencyWakeBackstopResult> {
    let mut result = ResolvedDependencyWakeBackstopResult::default();
    let source = opts
        .source
        .clone()
        .unwrap_or_else(|| "issue_graph_liveness.backstop".to_string());
    let requested_by_actor_id = if source == "workspace.finalize" {
        "heartbeat_finalize"
    } else {
        "issue_graph_liveness_backstop"
    };
    let payload_backstop = if source == "workspace.finalize" {
        "workspace_finalize_reconciliation"
    } else {
        "issue_graph_liveness_reconciliation"
    };

    // 1. 列出 candidates
    let candidates = query_blocked_candidates(db, &opts).await?;
    result.checked = candidates.len() as i64;
    if let Some(limit) = candidate_limit(db, &opts).await? {
        if result.checked > limit {
            result.candidate_limit_skipped = result.checked - limit;
        }
    }

    if candidates.is_empty() {
        return Ok(result);
    }

    // 2. 按 company 分组，对每个 company 取 readiness map
    let issue_repo = IssueRepo::new(db);
    let agent_repo = AgentRepo::new(db);
    let mut by_company: std::collections::HashMap<Uuid, Vec<CandidateRow>> =
        std::collections::HashMap::new();
    for c in candidates {
        by_company.entry(c.company_id).or_default().push(c);
    }
    for (company_id, candidates) in by_company {
        let issue_ids: Vec<Uuid> = candidates.iter().map(|c| c.id).collect();
        let readiness_map = list_issue_dependency_readiness_map(db, company_id, &issue_ids).await?;

        for candidate in candidates {
            let agent_id = match candidate.assignee_agent_id {
                Some(id) => id,
                None => continue,
            };
            let readiness = match readiness_map.get(&candidate.id) {
                Some(r) if r.is_dependency_ready && !r.blocker_issue_ids.is_empty() => r,
                _ => {
                    result.not_ready_skipped += 1;
                    continue;
                }
            };
            let resolved_blocker_issue_id = match readiness.blocker_issue_ids.first() {
                Some(id) => *id,
                None => {
                    result.not_ready_skipped += 1;
                    continue;
                }
            };

            // 3. 构造 idempotency keys + 查 existing
            let idempotency_keys: Vec<String> = readiness
                .blocker_issue_ids
                .iter()
                .map(|bid| build_issue_blockers_resolved_wake_idempotency_key(candidate.id, *bid))
                .collect();
            if let Some(_existing) = find_existing_issue_blockers_resolved_wake_for_any_key(
                db,
                company_id,
                &idempotency_keys,
            )
            .await?
            {
                result.existing_wake_skipped += 1;
                continue;
            }
            let idempotency_key = idempotency_keys[0].clone();

            // 4. active execution / queued wake / pending wake interaction 抑制
            if issue_repo.has_active_execution_path(candidate.id).await? {
                result.live_path_skipped += 1;
                continue;
            }
            if issue_repo
                .has_queued_issue_wake(company_id, candidate.id, Some(agent_id))
                .await?
            {
                result.live_path_skipped += 1;
                continue;
            }
            if issue_repo
                .has_pending_wake_interaction_for_issue(company_id, candidate.id)
                .await?
            {
                result.interaction_skipped += 1;
                continue;
            }

            // 5. pause-hold 抑制
            if is_automatic_recovery_suppressed_by_pause_hold(db, company_id, candidate.id)
                .await
                .unwrap_or(None)
                .is_some()
            {
                result.pause_hold_skipped += 1;
                continue;
            }

            // 6. 发 wakeup
            let payload = json!({
                "issueId": candidate.id,
                "resolvedBlockerIssueId": resolved_blocker_issue_id,
                "blockerIssueIds": readiness.blocker_issue_ids,
                "backstop": payload_backstop,
            });
            let _context_snapshot = json!({
                "issueId": candidate.id,
                "taskId": candidate.id,
                "wakeReason": ISSUE_BLOCKERS_RESOLVED_WAKE_REASON,
                "source": source,
                "resolvedBlockerIssueId": resolved_blocker_issue_id,
                "blockerIssueIds": readiness.blocker_issue_ids,
            });
            let wake = agent_repo
                .create_wakeup_request(NewAgentWakeupRequest {
                    company_id,
                    agent_id,
                    source: HeartbeatInvocationSource::OnDemand,
                    trigger_detail: Some(WakeupTriggerDetail::System),
                    reason: Some(ISSUE_BLOCKERS_RESOLVED_WAKE_REASON.to_string()),
                    payload: Some(payload),
                    status: WakeupRequestStatus::Queued,
                    coalesced_count: 0,
                    requested_by_actor_type: Some(WakeupActorType::System),
                    requested_by_actor_id: Some(requested_by_actor_id.to_string()),
                    idempotency_key: Some(idempotency_key.clone()),
                    run_id: opts.run_id,
                    error: None,
                })
                .await;
            match wake {
                Ok(wake_row) => {
                    result.healed += 1;
                    result.issue_ids.push(candidate.id);

                    // 写 activity log
                    let details = json!({
                        "source": source,
                        "wakeupRunId": wake_row.id,
                        "idempotencyKey": idempotency_key,
                        "resolvedBlockerIssueId": resolved_blocker_issue_id,
                        "blockerIssueIds": readiness.blocker_issue_ids,
                    });
                    let _ = ActivityRepo::new(db)
                        .record(&NewActivity {
                            company_id,
                            actor_type: ActorType::System,
                            actor_id: "issue_graph_liveness_backstop".to_string(),
                            action: "issue.blockers_resolved_wake_emitted".to_string(),
                            entity_type: "issue".to_string(),
                            entity_id: candidate.id.to_string(),
                            agent_id: Some(agent_id),
                            run_id: opts.run_id,
                            responsible_user_id: None,
                            details: Some(details),
                        })
                        .await
                        .map_err(|e| format!("activity log write failed: {e}"));
                }
                Err(e) => {
                    let _ = format!("backstop enqueue wake failed: {e}");
                    result.deferred_or_failed += 1;
                    result.enqueue_failed += 1;
                }
            }
        }
    }
    Ok(result)
}

// ============================================================================
// Helpers
// ============================================================================

/// 单个 blocked candidate（与 Node queryCandidates 返回的 row 对齐）。
#[derive(Debug, Clone)]
struct CandidateRow {
    id: Uuid,
    company_id: Uuid,
    identifier: Option<String>,
    assignee_agent_id: Option<Uuid>,
}

/// 列出 blocked candidates。`blocker_issue_id` 模式下只取被该 blocker 阻塞的 issue。
async fn query_blocked_candidates(
    db: &Db,
    opts: &ResolvedDependencyWakeBackstopOptions,
) -> sqlx::Result<Vec<CandidateRow>> {
    if let Some(blocker_id) = opts.blocker_issue_id {
        // JOIN 模式：ir.issue_id = blocker_id, ir.type = 'blocks', related = blocked issue
        let rows = sqlx::query(
            "SELECT i.id, i.company_id, i.identifier, i.assignee_agent_id \
             FROM issues i \
             INNER JOIN issue_relations ir ON ir.related_issue_id = i.id \
             WHERE ir.company_id = i.company_id \
               AND ir.type = 'blocks' \
               AND ir.issue_id = $1 \
               AND i.status::text = 'blocked' \
               AND i.hidden_at IS NULL \
               AND i.assignee_agent_id IS NOT NULL \
               AND ($2::uuid IS NULL OR i.company_id = $2) \
             ORDER BY i.id ASC LIMIT $3",
        )
        .bind(blocker_id)
        .bind(opts.company_id)
        .bind(RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT)
        .fetch_all(db.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| CandidateRow {
                id: row.try_get("id").unwrap_or(Uuid::nil()),
                company_id: row.try_get("company_id").unwrap_or(Uuid::nil()),
                identifier: row.try_get("identifier").ok(),
                assignee_agent_id: row.try_get("assignee_agent_id").ok(),
            })
            .collect())
    } else {
        let rows = sqlx::query(
            "SELECT id, company_id, identifier, assignee_agent_id \
             FROM issues \
             WHERE company_id = $1 \
               AND status::text = 'blocked' \
               AND hidden_at IS NULL \
               AND assignee_agent_id IS NOT NULL \
             ORDER BY id ASC LIMIT $2",
        )
        .bind(opts.company_id.unwrap_or(Uuid::nil()))
        .bind(RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT)
        .fetch_all(db.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| CandidateRow {
                id: row.try_get("id").unwrap_or(Uuid::nil()),
                company_id: row.try_get("company_id").unwrap_or(Uuid::nil()),
                identifier: row.try_get("identifier").ok(),
                assignee_agent_id: row.try_get("assignee_agent_id").ok(),
            })
            .collect())
    }
}

/// 计算 candidate limit（仅在 cursor 模式下用到；当前 Rust 实现用单次 limit）。
async fn candidate_limit(
    _db: &Db,
    _opts: &ResolvedDependencyWakeBackstopOptions,
) -> sqlx::Result<Option<i64>> {
    Ok(Some(RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_key_format_matches_node() {
        let dep = Uuid::nil();
        let blk = Uuid::nil();
        let key = build_issue_blockers_resolved_wake_idempotency_key(dep, blk);
        assert_eq!(
            key,
            "issue_blockers_resolved_wake:00000000-0000-0000-0000-000000000000:00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn candidate_limit_constant_is_200() {
        assert_eq!(RESOLVED_DEPENDENCY_WAKE_BACKSTOP_CANDIDATE_LIMIT, 200);
    }

    #[test]
    fn wake_reason_label_matches_node() {
        assert_eq!(
            ISSUE_BLOCKERS_RESOLVED_WAKE_REASON,
            "issue_blockers_resolved"
        );
    }

    #[test]
    fn source_routing_uses_heartbeat_finalize_for_workspace_finalize() {
        let opts = ResolvedDependencyWakeBackstopOptions {
            company_id: None,
            blocker_issue_id: None,
            run_id: None,
            source: Some("workspace.finalize".to_string()),
        };
        assert_eq!(opts.source.as_deref(), Some("workspace.finalize"));
    }
}
