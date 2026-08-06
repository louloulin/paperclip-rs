//! Run finished recovery cleanup。
//!
//! 对齐 Node `services/recovery/service.ts` 的两处 recovery action 收尾：
//! - `foldSourceResolvedStaleRun` 末尾 → 调 `recoveryActionsSvc.getActiveForIssue` + `resolveActiveForIssue`
//! - 任何 run 正常 finalize（success/fail/cancel）→ 同样的 active recovery action 处理
//!
//! 设计：
//! - 当 heartbeat run 完成（succeeded / failed / cancelled）时：
//!   1. 通过 `issues.execution_run_id` 找到 source issue
//!   2. 查 `issue_recovery_actions` 中 status='active'/'escalated' 的 action
//!   3. 根据 outcome 决定如何 resolve：
//!      - succeeded → resolved（成功完成）
//!      - failed → escalated（如果是 transient_failure，否则 failed）
//!      - cancelled → cancelled（用户取消）
//! - 纯函数：业务规则判定；DB 操作通过 pc-repos IssueRepo
//! - 复用 `IssueRepo::get_active_recovery_action` / `resolve_recovery_action_for_issue`
//! - 与现有 `fold_source_resolved_stale_run` 互补：那个是 source 已 terminal 时的 fold，
//!   本模块是 run 正常结束时的常规 cleanup
use serde::Serialize;
use uuid::Uuid;

use pc_repos::issue::{IssueRecoveryActionRow, IssueRepo};
use pc_repos::Db;

// ============================================================================
// Public types
// ============================================================================

/// Run final outcome（来自 HeartbeatStatus 枚举的 final 状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFinishedOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl RunFinishedOutcome {
    fn as_status_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// `resolve_recovery_action_on_run_finished` 输出。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFinishedCleanupResult {
    /// 找到 source issue id。
    pub source_issue_id: Option<Uuid>,
    /// 是否解析并 resolve 了 active recovery action。
    pub resolved_action_id: Option<Uuid>,
    /// 应用的 outcome 字符串（resolved / escalated / failed / cancelled）。
    pub applied_outcome: Option<String>,
    /// 应用的 status 字符串（resolved / escalated / failed）。
    pub applied_status: Option<String>,
    /// source issue 没有 active recovery action。
    pub skipped_reason: Option<RunFinishedSkipReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFinishedSkipReason {
    /// run 不存在。
    RunNotFound,
    /// run 没有 source issue（execution_run_id 为空）。
    NoSourceIssue,
    /// source issue 没有 active recovery action。
    NoActiveAction,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 当 run finished 时，清理 source issue 的 active recovery action。
///
/// 与 Node 行为对齐：
/// - 找到 issues.execution_run_id = run_id 的 source issue
/// - 查 active recovery action
/// - 根据 outcome 解析：
///   - succeeded → resolved
///   - failed → failed (注意：原 Node 在 transient_failure 时 escalated，但本实现
///     简化处理为 failed。调用方可在 outcome 之前判断后调 `resolve_with_outcome` 自己指定)
///   - cancelled → cancelled
/// - 写入 resolution_note
///
/// 返回 `RunFinishedCleanupResult`，调用方可根据 resolved_action_id 决定后续行为。
pub async fn resolve_recovery_action_on_run_finished(
    db: &Db,
    run_id: Uuid,
    outcome: RunFinishedOutcome,
) -> sqlx::Result<RunFinishedCleanupResult> {
    let mut result = RunFinishedCleanupResult::default();

    // Step 1: 找 source issue via execution_run_id
    let source_issue_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM issues \
         WHERE execution_run_id = $1 AND hidden_at IS NULL LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(db.pool())
    .await?;

    let source_issue_id = match source_issue_id {
        Some(id) => id,
        None => {
            // run 不存在或没有 source issue
            let run_exists: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM heartbeat_runs WHERE id = $1")
                    .bind(run_id)
                    .fetch_optional(db.pool())
                    .await?;
            if run_exists.is_none() {
                result.skipped_reason = Some(RunFinishedSkipReason::RunNotFound);
            } else {
                result.skipped_reason = Some(RunFinishedSkipReason::NoSourceIssue);
            }
            return Ok(result);
        }
    };
    result.source_issue_id = Some(source_issue_id);

    // Step 2: 查 active recovery action
    let repo = IssueRepo::new(db);
    let active_action: Option<IssueRecoveryActionRow> =
        repo.get_active_recovery_action(source_issue_id).await?;

    let action = match active_action {
        Some(a) => a,
        None => {
            result.skipped_reason = Some(RunFinishedSkipReason::NoActiveAction);
            return Ok(result);
        }
    };

    // Step 3: 根据 outcome 决定 action 的 status 和 outcome
    let (action_status, action_outcome, note) = match outcome {
        RunFinishedOutcome::Succeeded => (
            "resolved",
            "resolved",
            format!("Heartbeat run {} succeeded", run_id),
        ),
        RunFinishedOutcome::Failed => (
            "failed",
            "failed",
            format!("Heartbeat run {} failed", run_id),
        ),
        RunFinishedOutcome::Cancelled => (
            "cancelled",
            "cancelled",
            format!("Heartbeat run {} cancelled", run_id),
        ),
    };

    // Step 4: resolve action
    let updated = repo
        .resolve_recovery_action_for_issue(
            source_issue_id,
            action.id,
            Some(&note),
            action_outcome,
            action_status,
        )
        .await?;

    if let Some(_updated) = updated {
        result.resolved_action_id = Some(action.id);
        result.applied_outcome = Some(action_outcome.to_string());
        result.applied_status = Some(action_status.to_string());
    }

    Ok(result)
}

/// `resolve_recovery_action_on_run_finished` 的字符串入口（供 SqlHeartbeatExecutionSink 用）。
pub fn outcome_from_status_str(s: &str) -> Option<RunFinishedOutcome> {
    match s {
        "succeeded" => Some(RunFinishedOutcome::Succeeded),
        "failed" => Some(RunFinishedOutcome::Failed),
        "cancelled" => Some(RunFinishedOutcome::Cancelled),
        _ => None,
    }
}

/// `resolve_recovery_action_on_run_finished` 的辅助：从 outcome enum 取字符串。
pub fn outcome_to_status_str(outcome: RunFinishedOutcome) -> &'static str {
    outcome.as_status_str()
}
