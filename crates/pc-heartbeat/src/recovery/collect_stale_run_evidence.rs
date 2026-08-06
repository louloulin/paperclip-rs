//! `collectStaleRunEvidence` —— Node `services/recovery/service.ts:1852` 对齐。
//!
//! 业务语义：
//! - 收集 stale active run 的 evidence 集合，用于 description builder
//! - 包含：
//!   - `safe_tail`: run log tail（暂未实现 redaction，简化为 raw tail）
//!   - `recent_events`: heartbeat_run_events LIMIT 8 (reverse 后按时间升序)
//!   - `child_issues`: issues WHERE parent_id = source_issue.id LIMIT 8
//!   - `blockers`: issueRelations.type='blocks' WHERE related_issue_id = source_issue.id LIMIT 8
//!   - `silence_age_ms`: (now - silence_started_at).num_milliseconds()
//!
//! 设计意图：
//! - DB-only helper：直接 SQL（避免依赖完整 HeartbeatRow struct）
//! - 4 个 query 可并发执行（Node 用 Promise.all）；当前 Rust 实现 sequential，
//!   后续可优化为 tokio::try_join! 但优先级低
//! - redaction (`redactWatchdogEvidenceText` + `getCurrentUserRedactionOptions`) 暂未实现：
//!   safe_tail 直接使用空字符串，event message 原样返回。后续 Round 接入
//!
//! 调用方：`create_or_update_stale_run_evaluation_full` 的 handle_create（Round 343 接入）

use chrono::{DateTime, Utc};
use uuid::Uuid;

use pc_repos::Db;

use super::build_stale_run_evaluation_description::{StaleIssueLinkView, StaleRunEventView};

/// `collect_stale_run_evidence` 入参（简化版：不要求完整 HeartbeatRow）。
#[derive(Debug, Clone)]
pub struct CollectStaleRunEvidenceInput {
    pub company_id: Uuid,
    pub run_id: Uuid,
    /// source_issue_id（None 时跳过 child_issues / blockers 查询）
    pub source_issue_id: Option<Uuid>,
    pub now: DateTime<Utc>,
}

/// `collect_stale_run_evidence` 输出。
///
/// 与 Node `collectStaleRunEvidence` 返回类型对齐。
/// 字段类型与 description builder 期望的 `StaleRunEvidenceView` 完全一致。
#[derive(Debug, Clone)]
pub struct CollectedStaleRunEvidence {
    pub safe_tail: Option<String>,
    pub silence_age_ms: i64,
    pub recent_events: Vec<StaleRunEventView>,
    pub child_issues: Vec<StaleIssueLinkView>,
    pub blockers: Vec<StaleIssueLinkView>,
}

/// 收集 stale active run 的 evidence。
///
/// 流程（与 Node `collectStaleRunEvidence` 完全对齐）：
/// 1. 计算 silence_age_ms（基于 last_output_at 或 process_started_at 或 started_at 或 created_at）
/// 2. SELECT recent_events (heartbeat_run_events WHERE run_id ORDER BY id DESC LIMIT 8)
///    - reverse 后按时间升序
/// 3. 若 source_issue 存在：SELECT child_issues (issues WHERE parent_id LIMIT 8)
/// 4. 若 source_issue 存在：SELECT blockers (issueRelations type='blocks' LIMIT 8)
///
/// 简化点（与 Node 的差异）：
/// - safe_tail 暂返回 None（Node 实现 read_run_log_tail_for_evidence + redact）
/// - event message 不 redact（Node 实现 redactWatchdogEvidenceText）
/// - 4 个 query sequential（Node 用 Promise.all 并发）
pub async fn collect_stale_run_evidence(
    db: &Db,
    input: CollectStaleRunEvidenceInput,
) -> sqlx::Result<CollectedStaleRunEvidence> {
    // Step 1: silence_age_ms
    let silence_started_at = load_silence_started_at(db, input.run_id).await?;
    let silence_age_ms = if let Some(started_at) = silence_started_at {
        (input.now - started_at).num_milliseconds().max(0)
    } else {
        0
    };

    // Step 2: recent_events (DESC LIMIT 8, then reverse)
    let mut recent_events: Vec<StaleRunEventView> = sqlx::query_as(
        "SELECT event_type, level::text AS level, created_at::text AS created_at, message \
         FROM heartbeat_run_events \
         WHERE company_id = $1 AND run_id = $2 \
         ORDER BY id DESC LIMIT 8",
    )
    .bind(input.company_id)
    .bind(input.run_id)
    .fetch_all(db.pool())
    .await?
    .into_iter()
    .map(
        |(event_type, level, created_at, message): (
            String,
            Option<String>,
            String,
            Option<String>,
        )| {
            StaleRunEventView {
                event_type,
                level,
                created_at,
                message,
            }
        },
    )
    .collect();
    // Node: recentEvents.reverse() —— 按时间升序
    recent_events.reverse();

    // Step 3 + 4: child_issues / blockers (only if source_issue)
    let (child_issues, blockers) = if let Some(source_id) = input.source_issue_id {
        let children = sqlx::query_as(
            "SELECT id, identifier, title, status::text \
             FROM issues \
             WHERE company_id = $1 AND parent_id = $2 AND hidden_at IS NULL \
             ORDER BY updated_at DESC LIMIT 8",
        )
        .bind(input.company_id)
        .bind(source_id)
        .fetch_all(db.pool())
        .await?
        .into_iter()
        .map(
            |(id, identifier, title, status): (Uuid, Option<String>, String, String)| {
                StaleIssueLinkView {
                    id,
                    identifier,
                    title,
                    status,
                }
            },
        )
        .collect();

        let blockers = sqlx::query_as(
            "SELECT i.id, i.identifier, i.title, i.status::text \
             FROM issue_relations r \
             INNER JOIN issues i ON i.id = r.issue_id \
             WHERE r.company_id = $1 AND r.related_issue_id = $2 AND r.type = 'blocks' \
               AND i.hidden_at IS NULL \
             LIMIT 8",
        )
        .bind(input.company_id)
        .bind(source_id)
        .fetch_all(db.pool())
        .await?
        .into_iter()
        .map(
            |(id, identifier, title, status): (Uuid, Option<String>, String, String)| {
                StaleIssueLinkView {
                    id,
                    identifier,
                    title,
                    status,
                }
            },
        )
        .collect();

        (children, blockers)
    } else {
        (vec![], vec![])
    };

    Ok(CollectedStaleRunEvidence {
        safe_tail: None, // 暂未实现 redaction；后续 Round 接入
        silence_age_ms,
        recent_events,
        child_issues,
        blockers,
    })
}

/// 取 silence started_at（last_output_at → process_started_at → started_at → created_at）。
///
/// 复用 scan_silent_active_runs_db 的语义。
async fn load_silence_started_at(db: &Db, run_id: Uuid) -> sqlx::Result<Option<DateTime<Utc>>> {
    let row: Option<(
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        "SELECT last_output_at, process_started_at, started_at, created_at \
         FROM heartbeat_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(
        row.and_then(|(last_output, process_started, started, created)| {
            last_output
                .or(process_started)
                .or(started)
                .or(Some(created))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // 单元测试有限：实际 DB 测试在 round343 集成测试中
}
