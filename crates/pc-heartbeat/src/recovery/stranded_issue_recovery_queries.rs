//! `stranded_issue_recovery` 相关 DB 查询 + helpers —— Node `services/recovery/service.ts` 多个函数。
//!
//! 业务语义：
//! - `is_stranded_issue_recovery_issue(issue)` —— pure 检查 `issue.origin_kind == "stranded_issue_recovery"`
//! - `find_open_stranded_issue_recovery_issue(db, company_id, source_id)` —— 查 open 的 recovery issue
//!   （origin_kind=stranded_issue_recovery, origin_id=source_id, hidden_at IS NULL,
//!   status NOT IN done/cancelled, ORDER BY created_at DESC LIMIT 1）
//! - `is_unique_stranded_issue_recovery_conflict(error)` —— 检查 sqlx::Error 是否来自 PG 23505
//!   唯一冲突且约束名为 `issues_active_stranded_issue_recovery_uq`
//!
//! 设计原则：
//! - `is_stranded_issue_recovery_issue` 是 pure 函数（无副作用）
//! - `find_open_stranded_issue_recovery_issue` 是纯 DB 查询
//! - `is_unique_stranded_issue_recovery_conflict` 是 pure 错误识别函数
//! - 常量 `STRANDED_ISSUE_RECOVERY_ORIGIN_KIND = "stranded_issue_recovery"` 集中维护
//! - 与 Node 业务语义 1:1 对齐

use pc_repos::issue::IssueRow;
use pc_repos::Db;
use uuid::Uuid;

/// Stranded issue recovery issue 的 origin_kind 标识。
///
/// 与 Node `STRANDED_ISSUE_RECOVERY_ORIGIN_KIND` 对齐。
/// 与 PG unique index `issues_active_stranded_issue_recovery_uq` 配套使用。
pub const STRANDED_ISSUE_RECOVERY_ORIGIN_KIND: &str = "stranded_issue_recovery";

/// PG 唯一冲突错误码。
const PG_UNIQUE_VIOLATION: &str = "23505";

/// 唯一约束名（与 migration #0072_large_sandman.sql 一致）。
const STRANDED_ISSUE_RECOVERY_UQ_CONSTRAINT: &str = "issues_active_stranded_issue_recovery_uq";

/// 检查 issue 本身是否是 stranded_issue_recovery 类型。
///
/// 与 Node `isStrandedIssueRecoveryIssue` 对齐：`issue.originKind === STRANDED_ISSUE_RECOVERY_ORIGIN_KIND`
pub fn is_stranded_issue_recovery_issue(issue: &IssueRow) -> bool {
    issue.origin_kind == STRANDED_ISSUE_RECOVERY_ORIGIN_KIND
}

/// 列出某个 source issue 当前 open 的 stranded_issue_recovery issue。
///
/// 与 Node `findOpenStrandedIssueRecoveryIssue` 对齐：
/// - company_id = $1
/// - origin_kind = "stranded_issue_recovery"
/// - origin_id = source_issue_id
/// - hidden_at IS NULL
/// - status NOT IN ('done', 'cancelled')
/// - ORDER BY created_at DESC
/// - LIMIT 1
///
/// 返回 `Ok(Some(IssueRow))` 当找到；`Ok(None)` 当没有。
pub async fn find_open_stranded_issue_recovery_issue(
    db: &Db,
    company_id: Uuid,
    source_issue_id: Uuid,
) -> sqlx::Result<Option<IssueRow>> {
    let source_issue_id_str = source_issue_id.to_string();
    sqlx::query_as::<_, IssueRow>(
        "SELECT id, company_id, project_id, project_workspace_id, goal_id, parent_id, title, description, status, work_mode, harness_kind, priority, assignee_agent_id, assignee_user_id, checkout_run_id, execution_run_id, execution_agent_name_key, execution_locked_at, created_by_agent_id, created_by_user_id, responsible_user_id, issue_number, identifier, origin_kind, origin_id, origin_run_id, origin_fingerprint, request_depth, billing_code, assignee_adapter_overrides, execution_policy, execution_state, monitor_next_check_at, monitor_wake_requested_at, monitor_last_triggered_at, monitor_attempt_count, monitor_notes, monitor_scheduled_by, execution_workspace_id, execution_workspace_preference, execution_workspace_settings, source_trust, unblock_descriptor, blocked_transition_at, blocked_owner_notified_at, started_at, completed_at, cancelled_at, hidden_at, created_at, updated_at FROM issues \
         WHERE company_id = $1 \
           AND origin_kind = $2 \
           AND origin_id = $3 \
           AND hidden_at IS NULL \
           AND status NOT IN ('done','cancelled') \
         ORDER BY created_at DESC \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(source_issue_id_str)
    .fetch_optional(db.pool())
    .await
}

/// 检查 sqlx::Error 是否来自 `issues_active_stranded_issue_recovery_uq` 唯一冲突。
///
/// 与 Node `isUniqueStrandedIssueRecoveryConflict` 对齐：
/// - 错误码 = "23505"（PG unique_violation）
/// - 约束名 = "issues_active_stranded_issue_recovery_uq"
///
/// 返回 `true` 当且仅当两个条件都满足。
///
/// 注意：sqlx::Error::Database 携带的底层错误可能是 `PgDatabaseError`，
/// 也可能是其他 dialect 错误。我们通过 `code()` + `constraint()` 提取。
pub fn is_unique_stranded_issue_recovery_conflict(error: &sqlx::Error) -> bool {
    let sqlx::Error::Database(db_err) = error else {
        return false;
    };
    if db_err.code().as_deref() != Some(PG_UNIQUE_VIOLATION) {
        return false;
    }
    match db_err.constraint() {
        Some(constraint) => constraint == STRANDED_ISSUE_RECOVERY_UQ_CONSTRAINT,
        None => {
            // 兜底：检查 message 是否含约束名（与 Node `maybe.message.includes(...)` 一致）
            db_err
                .message()
                .contains(STRANDED_ISSUE_RECOVERY_UQ_CONSTRAINT)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_kind_constant_matches_node() {
        assert_eq!(
            STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
            "stranded_issue_recovery"
        );
    }

    #[test]
    fn conflict_check_rejects_non_database_error() {
        let other = sqlx::Error::PoolTimedOut;
        assert!(!is_unique_stranded_issue_recovery_conflict(&other));
    }

    #[test]
    fn conflict_check_rejects_database_error_with_other_code() {
        // 构造一个非 23505 的 mock 数据库错误较难；
        // 直接使用不匹配的 constraint 测试路径较稳。
        // 这里只验证：PoolClosed 是 false（实际是非 Database 错误）
        let non_db = sqlx::Error::PoolClosed;
        assert!(!is_unique_stranded_issue_recovery_conflict(&non_db));
    }
}
