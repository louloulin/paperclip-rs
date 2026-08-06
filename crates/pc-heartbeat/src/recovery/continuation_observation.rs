//! Continuation observation 模块（Round 314）。
//!
//! 对齐 Node `services/recovery/service.ts` 的两个 helper：
//! - `getLatestAcceptedContinuationInteraction`：查询最近一个 status='accepted' 且
//!   continuation_policy IN ('wake_assignee','wake_assignee_on_accept') 的 interaction
//! - `hasSuccessfulIssueRunSince`：查询 since 时间之后该 issue 是否有成功的 heartbeat run
//!
//! 这两个 helper 是 reconcile_stranded_assigned_issues 中的 continuation 路径关键依赖：
//! - 有 accepted interaction + 没有 successful run since resolution → 需要 requeue continuation
//! - 有 successful run since resolution → productive_continuation_observed
//!
//! 设计：
//! - 纯 DB I/O：无业务规则，便于单元测试
//! - 单一职责：每个 helper 只做一件事
//! - 返回值是普通结构体，便于调用方决定后续动作
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

// ============================================================================
// Public types
// ============================================================================

/// Continuation interaction 快照。
#[derive(Debug, Clone)]
pub struct AcceptedContinuationInteraction {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub continuation_policy: String,
    pub source_run_id: Option<Uuid>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl AcceptedContinuationInteraction {
    /// 解析"interaction 被接受的时间点"：resolved_at 优先，其次 updated_at。
    ///
    /// 与 Node `acceptedInteractionResolvedAt` 对齐：
    /// `resolvedAt ?? updatedAt`
    pub fn effective_resolution_time(&self) -> DateTime<Utc> {
        self.resolved_at.unwrap_or(self.updated_at)
    }
}

// ============================================================================
// get_latest_accepted_continuation_interaction
// ============================================================================

/// 查找 issue 的最近一个 accepted continuation interaction。
///
/// 与 Node `getLatestAcceptedContinuationInteraction` 对齐：
/// - status = 'accepted'
/// - continuation_policy IN ('wake_assignee', 'wake_assignee_on_accept')
/// - 排序：resolved_at DESC NULLS LAST, updated_at DESC, id DESC
pub async fn get_latest_accepted_continuation_interaction(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Option<AcceptedContinuationInteraction>> {
    let row: Option<(
        Uuid,
        String,
        String,
        String,
        Option<Uuid>,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        "SELECT id, kind::text, status::text, continuation_policy::text, \
                source_run_id, resolved_at, updated_at \
         FROM issue_thread_interactions \
         WHERE company_id = $1 \
           AND issue_id = $2 \
           AND status = 'accepted' \
           AND continuation_policy IN ('wake_assignee', 'wake_assignee_on_accept') \
         ORDER BY resolved_at DESC NULLS LAST, updated_at DESC, id DESC \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(issue_id)
    .fetch_optional(db.pool())
    .await?;

    Ok(row.map(
        |(id, kind, status, continuation_policy, source_run_id, resolved_at, updated_at)| {
            AcceptedContinuationInteraction {
                id,
                kind,
                status,
                continuation_policy,
                source_run_id,
                resolved_at,
                updated_at,
            }
        },
    ))
}

// ============================================================================
// has_successful_run_since
// ============================================================================

/// 自指定时间以来，issue 是否有过成功的 heartbeat run（用于特定 agent + interaction）。
///
/// 与 Node `hasSuccessfulIssueRunSince` 对齐：
/// - status='succeeded'
/// - context_snapshot->>'issueId' = issue_id
/// - 可选 interaction_id 过滤
/// - created_at OR finished_at >= since
///
/// 返回 Some(run_id) 表示存在成功的 run；None 表示没有。
pub async fn has_successful_run_since(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    issue_id: Uuid,
    since: DateTime<Utc>,
    interaction_id: Option<Uuid>,
) -> sqlx::Result<Option<Uuid>> {
    let interaction_filter = if let Some(iid) = interaction_id {
        format!(" AND context_snapshot->>'interactionId' = '{}'", iid)
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT id FROM heartbeat_runs \
         WHERE company_id = $1 \
           AND agent_id = $2 \
           AND status = 'succeeded' \
           AND context_snapshot->>'issueId' = $3 \
           AND (created_at >= $4 OR finished_at >= $4) {} \
         ORDER BY COALESCE(finished_at, created_at) DESC LIMIT 1",
        interaction_filter
    );
    let row: Option<(Uuid,)> = sqlx::query_as(&sql)
        .bind(company_id)
        .bind(agent_id)
        .bind(issue_id.to_string())
        .bind(since)
        .fetch_optional(db.pool())
        .await?;
    Ok(row.map(|(id,)| id))
}
