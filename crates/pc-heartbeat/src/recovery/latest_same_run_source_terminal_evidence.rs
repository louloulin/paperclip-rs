//! `latestSameRunSourceTerminalEvidence` —— Node `services/recovery/service.ts:1522` 对齐。
//!
//! 业务语义：
//! - 当 source_issue 状态为 terminal（done/cancelled）时，检查是否在 run 启动后
//!   （或 silence_started_at 后）有 durable same-run evidence（activity_log 行）。
//! - evidence 是 activity_log 中：
//!   - company_id = run.company_id
//!   - run_id = run.id
//!   - action = 'issue.updated'
//!   - entity_type = 'issue'
//!   - entity_id = source_issue.id
//!   - details->>'status' = source_issue.status
//!   - created_at >= evidence_after（run.started_at 或 silence_started_at）
//! - 返回 latest 一条；None 当无 evidence
//!
//! 设计意图：
//! - DB-only helper：直接 SQL 查询
//! - 与 Node 完全对齐（predicate list + ORDER BY DESC LIMIT 1）
//! - 返回值类型 `LatestSameRunSourceTerminalEvidence`（结构化，方便 caller 使用）
//!
//! 调用方：`create_or_update_stale_run_evaluation_full`（Round 339）

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use pc_repos::Db;

/// Same-run terminal evidence 结果。
///
/// 与 Node `latestSameRunSourceTerminalEvidence` 返回值对齐：
/// - `kind`: "activity"（当前唯一 kind，Node 后续可能扩展）
/// - `id`: activity_log.id
/// - `created_at`: 写入时间
/// - `action`: activity_log.action
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestSameRunSourceTerminalEvidence {
    pub kind: String,
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub action: String,
}

/// 查询 latest same-run terminal evidence for source_issue.
///
/// 入参：
/// - run_id, company_id: 限定 run 范围
/// - source_issue_id: 限定 issue
/// - source_issue_status: 仅匹配 details->>'status' = 该值
/// - evidence_after: 可选的时间下限（None → 不限定 created_at）
///
/// 返回 Option<LatestSameRunSourceTerminalEvidence>：
/// - None → 无 evidence
/// - Some → latest 的一条 activity_log 行
///
/// 与 Node `latestSameRunSourceTerminalEvidence` 完全对齐。
pub async fn latest_same_run_source_terminal_evidence(
    db: &Db,
    run_id: Uuid,
    company_id: Uuid,
    source_issue_id: Uuid,
    source_issue_status: &str,
    evidence_after: Option<DateTime<Utc>>,
) -> sqlx::Result<Option<LatestSameRunSourceTerminalEvidence>> {
    let row: Option<(Uuid, DateTime<Utc>, String)> = if let Some(after) = evidence_after {
        sqlx::query_as(
            "SELECT id, created_at, action::text FROM activity_log \
             WHERE company_id = $1 AND run_id = $2 \
               AND action = 'issue.updated' \
               AND entity_type = 'issue' \
               AND entity_id = $3::text \
               AND details->>'status' = $4 \
               AND created_at >= $5 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(company_id)
        .bind(run_id)
        .bind(source_issue_id.to_string())
        .bind(source_issue_status)
        .bind(after)
        .fetch_optional(db.pool())
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, created_at, action::text FROM activity_log \
             WHERE company_id = $1 AND run_id = $2 \
               AND action = 'issue.updated' \
               AND entity_type = 'issue' \
               AND entity_id = $3::text \
               AND details->>'status' = $4 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(company_id)
        .bind(run_id)
        .bind(source_issue_id.to_string())
        .bind(source_issue_status)
        .fetch_optional(db.pool())
        .await?
    };

    Ok(row.map(
        |(id, created_at, action)| LatestSameRunSourceTerminalEvidence {
            kind: "activity".to_owned(),
            id,
            created_at,
            action,
        },
    ))
}

/// 兼容性 helper：支持 `(run, source_issue, evidence_after)` struct 输入（便于将来 caller）。
///
/// 当前实现直接转发到基础函数。保留供后续扩展。
#[allow(dead_code)]
pub async fn latest_same_run_source_terminal_evidence_for(
    db: &Db,
    run_id: Uuid,
    company_id: Uuid,
    source_issue_id: Uuid,
    source_issue_status: &str,
    evidence_after: Option<DateTime<Utc>>,
) -> sqlx::Result<Option<LatestSameRunSourceTerminalEvidence>> {
    latest_same_run_source_terminal_evidence(
        db,
        run_id,
        company_id,
        source_issue_id,
        source_issue_status,
        evidence_after,
    )
    .await
}

// Avoid unused import warning
const _: Option<Value> = None;
