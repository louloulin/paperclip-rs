//! `loadLatestHeartbeatRunForIssue` —— 给定 issue 取最近一条 heartbeat_run 行。
//!
//! 业务语义：
//! - 用于 `escalate_stranded_recovery_issue_in_place` 构建完整 escalation comment
//! - 排序：`ORDER BY COALESCE(started_at, created_at) DESC NULLS LAST LIMIT 1`
//! - 若 issue 无任何关联 run → 返回 None（caller 用 missing-branch 渲染）
//!
//! 关联方式：
//! - 通过 `heartbeat_runs.context_snapshot->>'issueId'` 关联（与 Node `LatestIssueRun` 选择逻辑一致）
//! - 与 `HeartbeatRepo::find_active_run_by_issue` 区别：那条只查 active 状态，本 helper 不限状态
//!
//! 设计意图：
//! - 纯 DB helper，返回 `Option<EscalationRunView>`（只取必要字段）
//! - 高内聚：只做一件事——从 heartbeat_runs 表读一行
//! - 低耦合：不依赖其他 recovery 模块
//!
//! 调用方：
//! - `escalate_stranded_recovery_issue_in_place`（escalate_db.rs）—— in-place 升级时

use pc_repos::heartbeat::HeartbeatRow;
use pc_repos::Db;
use serde_json::Value;
use uuid::Uuid;

use super::build_recovery_issue_in_place_escalation_comment::EscalationRunView;

/// 加载某 issue 最近一次 heartbeat_run（任意状态），仅返回 comment builder 所需字段。
///
/// 返回 `None` 当 issue 无任何关联 run。
///
/// 注意：上下文匹配通过 `context_snapshot->>'issueId'`（即 heartbeat_run 显式绑定到该 issue），
/// 这与 Node `LatestIssueRun` 的语义一致。
pub async fn load_latest_heartbeat_run_for_issue(
    db: &Db,
    issue_id: Uuid,
) -> sqlx::Result<Option<EscalationRunView>> {
    let row: Option<HeartbeatRow> = sqlx::query_as::<_, HeartbeatRow>(
        "SELECT id, company_id, agent_id, invocation_source, trigger_detail, status,                 responsible_user_id, started_at, finished_at, error, wakeup_request_id,                 exit_code, signal, usage_json, result_json, session_id_before, session_id_after,                 log_store, log_ref, log_bytes, log_sha256, log_compressed, stdout_excerpt,                 stderr_excerpt, error_code, external_run_id, process_pid, process_group_id,                 process_started_at, last_output_at, last_output_seq, last_output_stream,                 last_output_bytes, retry_of_run_id, process_loss_retry_count,                 scheduled_retry_at, scheduled_retry_attempt, scheduled_retry_reason,                 issue_comment_status, issue_comment_satisfied_by_comment_id,                 issue_comment_retry_queued_at, liveness_state, liveness_reason,                 continuation_attempt, last_useful_action_at, next_action, context_snapshot,                 created_at, updated_at          FROM heartbeat_runs          WHERE context_snapshot->>'issueId' = $1          ORDER BY COALESCE(started_at, created_at) DESC NULLS LAST          LIMIT 1",
    )
    .bind(issue_id.to_string())
    .fetch_optional(db.pool())
    .await?;

    Ok(row.map(|r| EscalationRunView {
        id: r.id,
        agent_id: Some(r.agent_id),
        status: r.status,
        error: r.error,
        error_code: r.error_code,
        context_snapshot: Some(r.context_snapshot.unwrap_or(Value::Null)),
    }))
}

// (lib 测试覆盖在 `tests/round328_*` 与 `tests/round329_*` 集成测试中)
