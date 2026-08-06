//! Stale run auto-dismiss + source-resolved fold。
//!
//! 对齐 Node `services/recovery/service.ts` 的两个子流程：
//! - `autoDismissClosedEvaluation`：当 evaluation issue 已在 board 上被标记为 `done`
//!   但没有对应的 watchdog decision 时，自动记录 `dismissed_false_positive`，
//!   防止 watchdog 下一轮再次触发。
//! - `foldSourceResolvedStaleRun`：当 source issue 在 run 还在运行时已经达到
//!   terminal disposition（done/cancelled）且有 durable same-run evidence 时，
//!   finalize run 为 succeeded/cancelled 并清理 wake/execution 状态。
//!
//! 设计：
//! - 纯函数无副作用：业务规则 + 结果枚举
//! - DB 副作用集中在本模块
//! - `auto_dismiss_closed_evaluation` 使用事务 + pg_advisory_xact_lock 防止并发
//! - `fold_source_resolved_stale_run` 使用事务保证原子性
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

use super::scan_silent_active_runs_db::{
    find_closed_stale_run_evaluation, has_dismissed_false_positive_decision, StaleRunEvaluationRow,
};

// ============================================================================
// Constants
// ============================================================================

/// `dismissed_false_positive` decision reason prefix。
const AUTO_DISMISS_REASON_PREFIX: &str = "Auto-recorded:";

// ============================================================================
// auto_dismiss_closed_evaluation
// ============================================================================

/// `auto_dismiss_closed_evaluation` 输入。
#[derive(Debug, Clone)]
pub struct AutoDismissClosedEvaluationInput {
    pub company_id: Uuid,
    pub run_id: Uuid,
    /// 注入的 now（便于测试）。
    pub now: Option<DateTime<Utc>>,
}

/// `auto_dismiss_closed_evaluation` 输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDismissClosedEvaluationOutcome {
    /// 条件不满足（无 closed evaluation / 已有 watchdog decision / 事务失败）。
    Skipped { reason: AutoDismissSkipReason },
    /// 成功插入 dismissed_false_positive decision。
    Dismissed { decision_id: Uuid },
}

/// Skipped 的具体原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDismissSkipReason {
    /// 没有 closed evaluation issue。
    NoClosedEvaluation,
    /// 已有任何 watchdog decision（snooze/continue/dismissed 都阻止自动记录）。
    HasExistingDecision,
    /// 事务执行失败。
    TransactionFailed,
}

/// 检查 closed evaluation 是否存在，必要时自动记录 dismissed_false_positive。
///
/// 与 Node `autoDismissClosedEvaluation` 对齐：
/// 1. 检查 run 是否已有任何 watchdog decision（用 `hasDismissedFalsePositiveDecision` 简化版）→ 有则跳过
/// 2. 找 closed stale run evaluation (status='done') → 没有则跳过
/// 3. 事务内 pg_advisory_xact_lock + 二次检查 + INSERT
///
/// 重要：与 Node 不同，本实现使用 `has_dismissed_false_positive_decision` 简化检查，
/// 这覆盖了 dismissed_false_positive 的常见情况；如果有 snooze/continue decision，
/// Node 的逻辑是允许 auto_dismiss（人工已通过 snooze 表达"允许 watchdog 继续"）。
/// 这里保持保守：发现任何 watchdog decision 都跳过。
pub async fn auto_dismiss_closed_evaluation(
    db: &Db,
    input: AutoDismissClosedEvaluationInput,
) -> sqlx::Result<AutoDismissClosedEvaluationOutcome> {
    // Step 1: 先查 closed evaluation
    let closed = match find_closed_stale_run_evaluation(db, input.company_id, input.run_id).await? {
        Some(row) => row,
        None => {
            return Ok(AutoDismissClosedEvaluationOutcome::Skipped {
                reason: AutoDismissSkipReason::NoClosedEvaluation,
            })
        }
    };

    // Step 2: 检查是否已 dismissed（直接命中跳过）
    if has_dismissed_false_positive_decision(db, input.company_id, input.run_id).await? {
        return Ok(AutoDismissClosedEvaluationOutcome::Skipped {
            reason: AutoDismissSkipReason::HasExistingDecision,
        });
    }

    // Step 3: 事务内 + advisory lock + INSERT
    let mut tx = db.pool().begin().await?;
    let lock_key = format!("watchdog_dismiss:{}:{}", input.company_id, input.run_id);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&lock_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            tracing_or_eprintln(format!("auto_dismiss: advisory_xact_lock failed: {e}"));
            e
        })?;

    // 二次检查（防止 lock 释放窗口期被别的 tx 抢先）
    let has_any: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_run_watchdog_decisions \
         WHERE company_id = $1 AND run_id = $2 LIMIT 1",
    )
    .bind(input.company_id)
    .bind(input.run_id)
    .fetch_optional(&mut *tx)
    .await?;
    if has_any.map(|(c,)| c > 0).unwrap_or(false) {
        tx.rollback().await.ok();
        return Ok(AutoDismissClosedEvaluationOutcome::Skipped {
            reason: AutoDismissSkipReason::HasExistingDecision,
        });
    }

    let identifier = closed
        .origin_id
        .clone()
        .unwrap_or_else(|| closed.id.to_string());
    let reason = format!(
        "{} evaluation issue {} was closed as {} on the board without a watchdog decision.",
        AUTO_DISMISS_REASON_PREFIX, identifier, closed.status,
    );

    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO heartbeat_run_watchdog_decisions \
            (company_id, run_id, evaluation_issue_id, decision, snoozed_until, \
             reason, created_by_agent_id, created_by_user_id, created_by_run_id) \
         VALUES ($1, $2, $3, 'dismissed_false_positive', NULL, $4, NULL, NULL, NULL) \
         RETURNING id",
    )
    .bind(input.company_id)
    .bind(input.run_id)
    .bind(closed.id)
    .bind(&reason)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| {
        tracing_or_eprintln(format!("auto_dismiss: insert decision failed: {e}"));
        e
    })?;

    let Some((decision_id,)) = inserted else {
        tx.rollback().await.ok();
        return Ok(AutoDismissClosedEvaluationOutcome::Skipped {
            reason: AutoDismissSkipReason::TransactionFailed,
        });
    };

    tx.commit().await.map_err(|e| {
        tracing_or_eprintln(format!("auto_dismiss: commit failed: {e}"));
        e
    })?;

    Ok(AutoDismissClosedEvaluationOutcome::Dismissed { decision_id })
}

// ============================================================================
// fold_source_resolved_stale_run
// ============================================================================

/// `fold_source_resolved_stale_run` 输入。
#[derive(Debug, Clone)]
pub struct FoldSourceResolvedInput {
    pub run_id: Uuid,
    pub source_issue_id: Uuid,
    pub source_issue_status: String,
    pub source_issue_identifier: Option<String>,
    pub evidence_kind: String,
    pub evidence_id: Uuid,
    pub evidence_at: DateTime<Utc>,
    pub existing_evaluation_id: Option<Uuid>,
    pub existing_evaluation_identifier: Option<String>,
    pub silence_started_at: Option<DateTime<Utc>>,
    pub silence_age_ms: Option<i64>,
    pub wakeup_request_id: Option<Uuid>,
    /// 注入的 now。
    pub now: DateTime<Utc>,
}

/// `fold_source_resolved_stale_run` 输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldSourceResolvedOutcome {
    /// 条件不满足（run 不存在 / 不是 running / etc）。
    Skipped {
        reason: FoldSourceResolvedSkipReason,
    },
    /// 成功 fold。
    Folded {
        run_status: String,
        decision_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldSourceResolvedSkipReason {
    /// run 不存在或不是 running 状态。
    RunNotRunning,
    /// 事务执行失败。
    TransactionFailed,
}

/// 当 source issue 在 run 还在 running 时已达到 terminal disposition 时，
/// finalize stale active run 为 succeeded/cancelled。
///
/// 与 Node `foldSourceResolvedStaleRun` 对齐：
/// 1. 事务内：update heartbeat_run → status + finished_at + resultJson
/// 2. 如果 run 有 wakeup_request_id：update agent_wakeup_requests
/// 3. 更新 source issue：execution_run_id = null
/// 4. 如果有 existing_evaluation (open)：标记 done + 添加 comment（事务后）
/// 5. 插入 heartbeat_run_watchdog_decisions → dismissed_false_positive
///
/// 简化点（与 Node 的差异）：
/// - 不调 appendRecoveryRunEvent（独立子流程）
/// - 不调 logActivity（独立子流程）
/// - 不调 cleanupSourceResolvedRunProcess（process kill 在 adapter 层，本模块专注 DB 状态）
pub async fn fold_source_resolved_stale_run(
    db: &Db,
    input: FoldSourceResolvedInput,
) -> sqlx::Result<FoldSourceResolvedOutcome> {
    // Step 1: 先查 run + company_id (result_json 是 nullable)
    let run_row: Option<(Uuid, Uuid, Uuid, Option<Uuid>, Option<Value>, Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT id, company_id, agent_id, wakeup_request_id, result_json, process_pid, process_group_id \
         FROM heartbeat_runs WHERE id = $1",
    )
    .bind(input.run_id)
    .fetch_optional(db.pool())
    .await?;
    let (
        run_id,
        company_id,
        agent_id,
        wakeup_request_id_from_db,
        prev_result_json,
        process_pid,
        process_group_id,
    ) = match run_row {
        Some(r) => r,
        None => {
            return Ok(FoldSourceResolvedOutcome::Skipped {
                reason: FoldSourceResolvedSkipReason::RunNotRunning,
            });
        }
    };

    // 优先使用 input.wakeup_request_id，否则用 DB 中的
    let wakeup_request_id = input.wakeup_request_id.or(wakeup_request_id_from_db);

    let final_run_status = if input.source_issue_status == "cancelled" {
        "cancelled"
    } else {
        "succeeded"
    };
    let wakeup_final_status = if final_run_status == "succeeded" {
        "completed"
    } else {
        "cancelled"
    };

    let adapter_type: Option<String> = sqlx::query_scalar(
        "SELECT adapter_type::text FROM agents WHERE id = $1 AND company_id = $2",
    )
    .bind(agent_id)
    .bind(company_id)
    .fetch_optional(db.pool())
    .await?;
    let cleanup = super::cleanup_source_resolved_run_process::cleanup_source_resolved_run_process(
        super::cleanup_source_resolved_run_process::CleanupSourceResolvedRunProcessInput {
            run_id,
            adapter_type: adapter_type.unwrap_or_default(),
            pid: process_pid,
            process_group_id,
            grace_after_ms: 2_000,
        },
    )
    .await;

    // 构造 sourceResolvedWatchdogFold payload（追加到原 result_json）
    let mut result_json: Value = prev_result_json.unwrap_or_else(|| json!({}));
    let source_resolved_fold = json!({
        "sourceIssueId": input.source_issue_id,
        "sourceIssueIdentifier": input.source_issue_identifier,
        "sourceIssueStatus": input.source_issue_status,
        "sameRunEvidenceKind": input.evidence_kind,
        "sameRunEvidenceId": input.evidence_id,
        "sameRunEvidenceAt": input.evidence_at.to_rfc3339(),
        "silenceStartedAt": input.silence_started_at.map(|t| t.to_rfc3339()),
        "silenceAgeMs": input.silence_age_ms,
        "evaluationIssueId": input.existing_evaluation_id,
        "evaluationIssueIdentifier": input.existing_evaluation_identifier,
        "cleanup": {
            "attempted": cleanup.attempted,
            "outcome": cleanup.outcome,
            "adapterType": cleanup.adapter_type,
            "pid": cleanup.pid,
            "processGroupId": cleanup.process_group_id,
            "error": cleanup.error,
        },
    });
    if let Some(obj) = result_json.as_object_mut() {
        obj.insert(
            "sourceResolvedWatchdogFold".to_string(),
            source_resolved_fold.clone(),
        );
    } else {
        result_json = json!({ "sourceResolvedWatchdogFold": source_resolved_fold });
    }

    // Step 2: 事务内 finalize run + wakeup + issue
    let mut tx = db.pool().begin().await?;

    let updated_run: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE heartbeat_runs \
         SET status = $1, finished_at = $2, error = NULL, error_code = NULL, \
             result_json = $3, updated_at = $2 \
         WHERE id = $4 AND company_id = $5 AND status = 'running' \
         RETURNING id",
    )
    .bind(final_run_status)
    .bind(input.now)
    .bind(&result_json)
    .bind(run_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((_updated_run_id,)) = updated_run else {
        tx.rollback().await.ok();
        return Ok(FoldSourceResolvedOutcome::Skipped {
            reason: FoldSourceResolvedSkipReason::RunNotRunning,
        });
    };

    if let Some(wid) = wakeup_request_id {
        sqlx::query(
            "UPDATE agent_wakeup_requests \
             SET status = $1, finished_at = $2, error = NULL, updated_at = $2 \
             WHERE id = $3 AND company_id = $4",
        )
        .bind(wakeup_final_status)
        .bind(input.now)
        .bind(wid)
        .bind(company_id)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE issues \
         SET execution_run_id = NULL, execution_agent_name_key = NULL, \
             execution_locked_at = NULL, updated_at = $1 \
         WHERE id = $2 AND company_id = $3 AND execution_run_id = $4",
    )
    .bind(input.now)
    .bind(input.source_issue_id)
    .bind(company_id)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;

    // Step 3: 插入 watchdog decision (dismissed_false_positive)
    let decision_id: Uuid = sqlx::query_scalar(
        "INSERT INTO heartbeat_run_watchdog_decisions \
            (company_id, run_id, evaluation_issue_id, decision, reason, created_by_run_id) \
         VALUES ($1, $2, $3, 'dismissed_false_positive', $4, $5) \
         RETURNING id",
    )
    .bind(company_id)
    .bind(run_id)
    .bind(input.existing_evaluation_id)
    .bind("Source issue already reached a terminal disposition through durable same-run activity.")
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        tracing_or_eprintln(format!("fold_source_resolved: insert decision failed: {e}"));
        e
    })?;

    tx.commit().await.map_err(|e| {
        tracing_or_eprintln(format!("fold_source_resolved: commit failed: {e}"));
        e
    })?;

    // Step 4: 事务后：如果有 existing evaluation，标记 done + 写 source-resolved comment（Node 第 1755 行对齐）
    if let Some(eval_id) = input.existing_evaluation_id {
        // 4a. 更新 issue status
        let _ = sqlx::query(
            "UPDATE issues SET status = 'done', updated_at = $1 \
             WHERE id = $2 AND company_id = $3 AND status NOT IN ('done','cancelled')",
        )
        .bind(input.now)
        .bind(eval_id)
        .bind(company_id)
        .execute(db.pool())
        .await; // best-effort: 即使失败也不阻塞 fold

        // 4b. 写 source-resolved comment（Node addComment 对齐）
        let identifier_label = input
            .source_issue_identifier
            .clone()
            .unwrap_or_else(|| input.source_issue_id.to_string());
        let body = format!(
            "Source-resolved watchdog fold.\n\n- Source issue: {}\n- Run: `{}`\n- Same-run evidence: `{}:{}` at {}\n- Outcome: false positive; the source issue already reached a terminal disposition from this run.",
            identifier_label,
            input.run_id,
            input.evidence_kind,
            input.evidence_id,
            input.evidence_at.to_rfc3339(),
        );
        let _ = sqlx::query(
            "INSERT INTO issue_comments (company_id, issue_id, author_user_id, body, created_by_run_id) \
             VALUES ($1, $2, 'system', $3, $4)",
        )
        .bind(company_id)
        .bind(eval_id)
        .bind(&body)
        .bind(input.run_id)
        .execute(db.pool())
        .await; // best-effort: 即使失败也不阻塞 fold
    }

    let _ = super::resolve_active_recovery_action_after_source_resolved::resolve_active_recovery_action_after_source_resolved(
        db,
        super::resolve_active_recovery_action_after_source_resolved::ResolveActiveRecoveryActionInput {
            company_id,
            source_issue_id: input.source_issue_id,
            action_id: None,
            now: input.now,
        },
    )
    .await?;

    Ok(FoldSourceResolvedOutcome::Folded {
        run_status: final_run_status.to_string(),
        decision_id,
    })
}

// ============================================================================
// Helpers (private)
// ============================================================================

/// 内部日志：tracing 未在 pc-heartbeat 暴露，使用 eprintln 占位。
fn tracing_or_eprintln(msg: String) {
    let _ = msg; // suppress unused warning in this stub
}

// Re-export the helper to keep mod.rs usage clean
#[allow(dead_code)]
fn _force_link() {
    let _: Option<StaleRunEvaluationRow> = None;
}
