//! source issue 已由同一 run 终结后，收敛 active watchdog recovery action。
//!
//! 对齐 Node `recoveryActionsSvc.resolveActiveForIssue` 的 fold 专用调用：
//! 只处理 `active_run_watchdog`，避免误关闭其他恢复原因产生的 action。

use chrono::{DateTime, Utc};
use pc_repos::Db;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ResolveActiveRecoveryActionInput {
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub action_id: Option<Uuid>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActiveRecoveryAction {
    pub action_id: Uuid,
    pub status: String,
    pub outcome: String,
}

/// 解析 source issue 上当前 active/escalated 的 active-run watchdog action。
///
/// 返回 `None` 表示没有匹配 action；重复调用天然幂等。
pub async fn resolve_active_recovery_action_after_source_resolved(
    db: &Db,
    input: ResolveActiveRecoveryActionInput,
) -> sqlx::Result<Option<ResolvedActiveRecoveryAction>> {
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "UPDATE issue_recovery_actions SET status = 'resolved', outcome = 'false_positive', \
                resolution_note = $1, resolved_at = $2, updated_at = $2 \
         WHERE company_id = $3 AND source_issue_id = $4 \
           AND kind = 'active_run_watchdog' \
           AND status IN ('active', 'escalated') \
           AND ($5::uuid IS NULL OR id = $5) \
         RETURNING id, status, outcome",
    )
    .bind(
        "Source issue reached a terminal disposition through durable same-run activity; watchdog folded as source-resolved.",
    )
    .bind(input.now)
    .bind(input.company_id)
    .bind(input.source_issue_id)
    .bind(input.action_id)
    .fetch_optional(db.pool())
    .await?;

    Ok(row.map(
        |(action_id, status, outcome)| ResolvedActiveRecoveryAction {
            action_id,
            status,
            outcome,
        },
    ))
}
