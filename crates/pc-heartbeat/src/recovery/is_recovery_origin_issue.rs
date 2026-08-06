//! `isRecoveryOriginIssue` + `output_stale_recovery_recursion_refused` activity log ——
//! Node `services/recovery/service.ts:2073` + `:1329` 对齐。
//!
//! 业务语义：
//! - 当 `scan_silent_active_runs` 检测到 source_issue 是 recovery issue 时
//!   （origin_kind ∈ RECOVERY_ORIGIN_KINDS），必须 short-circuit 避免自我递归：
//!   1. 写 `heartbeat.output_stale_recovery_recursion_refused` activity_log 行
//!   2. 返回 Skipped（不创建 evaluation issue）
//!
//! RECOVERY_ORIGIN_KINDS（Node `recovery/origins.ts:1`）：
//! - `harness_liveness_escalation`
//! - `issue_productivity_review`
//! - `stranded_issue_recovery`
//! - `stale_active_run_evaluation`
//!
//! 设计意图：
//! - pure 函数 `is_recovery_origin_issue_str` + 常量 `RECOVERY_ORIGIN_KINDS`：可单测
//! - DB helper `log_recovery_recursion_refused_activity`：直接 SQL 避免 RepoError 类型转换
//! - 调用方：`create_or_update_stale_run_evaluation_full` 入口（Round 338）

use serde_json::json;
use uuid::Uuid;

use pc_repos::Db;

/// Node `RECOVERY_ORIGIN_KINDS` 的 Rust 等价。
///
/// 与 Node `recovery/origins.ts:1` 完全对齐。后续若 Node 新增 origin_kind，需同步更新此处。
pub const RECOVERY_ORIGIN_KINDS: &[&str] = &[
    "harness_liveness_escalation",
    "issue_productivity_review",
    "stranded_issue_recovery",
    "stale_active_run_evaluation",
];

/// Node `isRecoveryOriginIssue` 的 Rust 等价。
///
/// 输入：source_issue.origin_kind 字符串
/// 返回：true 当 origin_kind ∈ RECOVERY_ORIGIN_KINDS
pub fn is_recovery_origin_issue_str(origin_kind: &str) -> bool {
    RECOVERY_ORIGIN_KINDS.contains(&origin_kind)
}

/// `output_stale_recovery_recursion_refused` activity log 入参。
///
/// 与 Node `logActivity` 调用对齐：
/// - companyId / actorType="system" / actorId="system" / agentId / runId
/// - action=`heartbeat.output_stale_recovery_recursion_refused`
/// - entityType=`heartbeat_run` / entityId=run.id
/// - details: source / sourceIssueId / sourceIssueIdentifier / sourceIssueOriginKind / existingEvaluationIssueId
#[derive(Debug, Clone)]
pub struct LogRecursionRefusedInput<'a> {
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub agent_id: Uuid,
    pub source_issue_id: Uuid,
    pub source_issue_identifier: Option<&'a str>,
    pub source_issue_origin_kind: &'a str,
    pub existing_evaluation_issue_id: Option<Uuid>,
}

/// 写 `heartbeat.output_stale_recovery_recursion_refused` activity_log 行。
///
/// 直接 SQL（避免 RepoError 与 sqlx::Error 类型转换）。
///
/// 与 Node `logActivity(db, {...})` 调用字段对齐。
pub async fn log_recovery_recursion_refused_activity(
    db: &Db,
    input: &LogRecursionRefusedInput<'_>,
) -> sqlx::Result<()> {
    let details = json!({
        "source": "recovery.scan_silent_active_runs",
        "sourceIssueId": input.source_issue_id,
        "sourceIssueIdentifier": input.source_issue_identifier,
        "sourceIssueOriginKind": input.source_issue_origin_kind,
        "existingEvaluationIssueId": input.existing_evaluation_issue_id,
    });
    sqlx::query(
        "INSERT INTO activity_log \
         (company_id, actor_type, actor_id, action, entity_type, entity_id, agent_id, run_id, details) \
         VALUES ($1, 'system', 'system', 'heartbeat.output_stale_recovery_recursion_refused', \
                 'heartbeat_run', $2, $3, $4, $5)",
    )
    .bind(input.company_id)
    .bind(input.run_id.to_string())
    .bind(input.agent_id)
    .bind(input.run_id)
    .bind(details)
    .execute(db.pool())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_origin_kinds_list_matches_node() {
        let expected = vec![
            "harness_liveness_escalation",
            "issue_productivity_review",
            "stranded_issue_recovery",
            "stale_active_run_evaluation",
        ];
        assert_eq!(RECOVERY_ORIGIN_KINDS.len(), expected.len());
        for kind in expected {
            assert!(
                RECOVERY_ORIGIN_KINDS.contains(&kind),
                "missing origin kind: {kind}"
            );
        }
    }

    #[test]
    fn is_recovery_origin_issue_recognizes_all_known_kinds() {
        for kind in RECOVERY_ORIGIN_KINDS {
            assert!(is_recovery_origin_issue_str(kind));
        }
    }

    #[test]
    fn is_recovery_origin_issue_rejects_unrelated_kinds() {
        assert!(!is_recovery_origin_issue_str(""));
        assert!(!is_recovery_origin_issue_str("todo"));
        assert!(!is_recovery_origin_issue_str("user_input"));
        assert!(!is_recovery_origin_issue_str("stranded")); // partial
        assert!(!is_recovery_origin_issue_str("recovery")); // partial
    }
}
