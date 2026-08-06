//! Stale issue lock sweeper.
//!
//! 对齐 Node `services/recovery/service.ts` 的 `sweepStaleIssueLocks`：
//! - Backstop 自愈：清理 issues 上指向已 terminal 或不存在 heartbeat_runs 的
//!   `checkout_run_id` / `execution_run_id` 锁列。
//! - 防止 release/adoption 主路径漏掉的 stale lock 永久卡住 issue。
//! - 幂等：第二次调用不会重复清理。
//!
//! 边界：
//! - 仅写 `issues.checkout_run_id / execution_run_id / execution_agent_name_key /
//!   execution_locked_at` + 一条 `issue.stale_lock_cleared` activity log。
//! - 不调用 scheduler、不发 wake、不写 recovery_action。
//! - WHERE 条件带 `eq(checkout_run_id, old)` 守卫防止并发修改 race。
//!
//! 复用：
//! - `IssueRepo::new` 仅用于 `get` 之外不需要的接口；此处全部用 sqlx 直查询，
//!   避免给 IssueRepo 加一次性接口。
//! - `ActivityRepo::record` 复用现有 activity_log 仓储。

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use pc_repos::activity::{ActivityRepo, ActorType, NewActivity};
use pc_repos::Db;

/// Node `TERMINAL_HEARTBEAT_RUN_STATUSES` 常量镜像（succeeded/interrupted/
/// failed/cancelled/timed_out）。`heartbeat_runs.status` 取值为 enum-as-text。
pub const TERMINAL_HEARTBEAT_RUN_STATUSES: &[&str] = &[
    "succeeded",
    "interrupted",
    "failed",
    "cancelled",
    "timed_out",
];

/// Stale issue lock sweeper 的结果。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SweepStaleIssueLocksResult {
    /// 实际清理（UPDATE 成功）的 issue 行数。
    pub cleared: u32,
    /// 被清理的 issue ids。
    pub issue_ids: Vec<Uuid>,
    /// 候选 issues 总数（用于观测/可调试）。
    pub candidates_considered: u32,
}

/// 单 issue 候选快照（DB 读取时最小可观察列）。
#[derive(Debug, Clone)]
struct CandidateIssue {
    id: Uuid,
    company_id: Uuid,
    checkout_run_id: Option<Uuid>,
    execution_run_id: Option<Uuid>,
}

/// 主入口：扫描 + 清理 stale issue lock 列。
///
/// 与 Node `sweepStaleIssueLocks` 完整行为对齐：
/// 1. SELECT 所有 checkout_run_id 或 execution_run_id 非空的 issues
/// 2. 收集 referenced run ids → 批量 SELECT run status（避免 N+1）
/// 3. 对每个 issue：若其 checkout_run_id 引用的 run 是 non-terminal → skip
///    若其 execution_run_id 引用的 run 是 non-terminal → skip
/// 4. 同时满足清理条件 → UPDATE issues SET lock 列全清 NULL WHERE 加守卫
/// 5. UPDATE 成功 → 写 `issue.stale_lock_cleared` activity log
///
/// 返回清理数 + issue ids + 候选总数。
pub async fn sweep_stale_issue_locks(db: &Db) -> sqlx::Result<SweepStaleIssueLocksResult> {
    let mut result = SweepStaleIssueLocksResult::default();
    let candidates = load_candidate_issues(db).await?;
    result.candidates_considered = candidates.len() as u32;
    if candidates.is_empty() {
        return Ok(result);
    }
    let run_status_by_id = load_run_statuses(db, &candidates).await?;
    for issue in &candidates {
        if !is_cleanable(&run_status_by_id, issue.checkout_run_id)
            || !is_cleanable(&run_status_by_id, issue.execution_run_id)
        {
            continue;
        }
        let cleared = clear_stale_locks_for_issue(db, issue).await?;
        if cleared {
            result.cleared += 1;
            result.issue_ids.push(issue.id);
            // 写 activity log
            let details = json!({
                "source": "recovery.sweep_stale_issue_locks",
                "clearedCheckoutRunId": issue.checkout_run_id,
                "clearedExecutionRunId": issue.execution_run_id,
                "referencedRunStatuses": run_status_by_id
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect::<std::collections::HashMap<_, _>>(),
            });
            let _ = ActivityRepo::new(db)
                .record(&NewActivity {
                    company_id: issue.company_id,
                    actor_type: ActorType::System,
                    actor_id: "system".to_string(),
                    action: "issue.stale_lock_cleared".to_string(),
                    entity_type: "issue".to_string(),
                    entity_id: issue.id.to_string(),
                    agent_id: None,
                    run_id: None,
                    responsible_user_id: None,
                    details: Some(details),
                })
                .await
                .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        }
    }
    Ok(result)
}

/// 加载所有 checkout_run_id 或 execution_run_id 非空的 issues。
async fn load_candidate_issues(db: &Db) -> sqlx::Result<Vec<CandidateIssue>> {
    let rows = sqlx::query(
        "SELECT id, company_id, checkout_run_id, execution_run_id \
         FROM issues \
         WHERE checkout_run_id IS NOT NULL OR execution_run_id IS NOT NULL",
    )
    .fetch_all(db.pool())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(CandidateIssue {
                id: row.try_get("id")?,
                company_id: row.try_get("company_id")?,
                checkout_run_id: row.try_get("checkout_run_id")?,
                execution_run_id: row.try_get("execution_run_id")?,
            })
        })
        .collect::<sqlx::Result<Vec<_>>>()
}

/// 批量加载 referenced runs 的 status。
async fn load_run_statuses(
    db: &Db,
    candidates: &[CandidateIssue],
) -> sqlx::Result<std::collections::HashMap<Uuid, String>> {
    let mut ids: Vec<Uuid> = Vec::with_capacity(candidates.len() * 2);
    for c in candidates {
        if let Some(id) = c.checkout_run_id {
            ids.push(id);
        }
        if let Some(id) = c.execution_run_id {
            ids.push(id);
        }
    }
    ids.sort();
    ids.dedup();
    let mut map: std::collections::HashMap<Uuid, String> =
        std::collections::HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(map);
    }
    let rows = sqlx::query(
        "SELECT id, status::text AS status FROM heartbeat_runs WHERE id = ANY($1::uuid[])",
    )
    .bind(&ids)
    .fetch_all(db.pool())
    .await?;
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let status: String = row.try_get("status")?;
        map.insert(id, status);
    }
    Ok(map)
}

/// 判断引用的 run 是否可清理（terminal 或缺失）。
fn is_cleanable(
    run_status_by_id: &std::collections::HashMap<Uuid, String>,
    run_id: Option<Uuid>,
) -> bool {
    let Some(id) = run_id else {
        // run_id is NULL → 没有真锁，无需清理。
        // 但因为我们在外层 WHERE 排除了全 NULL 的行，这里 None 实际上不会出现。
        // 仍保留语义：None → true（保守地视为可清理）。
        return true;
    };
    match run_status_by_id.get(&id) {
        None => true, // missing run row → no real claim
        Some(status) => TERMINAL_HEARTBEAT_RUN_STATUSES.contains(&status.as_str()),
    }
}

/// UPDATE 单 issue 的锁列；返回是否实际清理了一行。
///
/// WHERE 守卫：`id = $1 AND (checkout_run_id = $2 OR checkout_run_id IS NULL)
/// AND (execution_run_id = $3 OR execution_run_id IS NULL)` —— 防止并发修改 race。
async fn clear_stale_locks_for_issue(db: &Db, issue: &CandidateIssue) -> sqlx::Result<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE issues SET \
            checkout_run_id = NULL, \
            execution_run_id = NULL, \
            execution_agent_name_key = NULL, \
            execution_locked_at = NULL, \
            updated_at = now() \
         WHERE id = $1 \
           AND (checkout_run_id = $2 OR (checkout_run_id IS NULL AND $2 IS NULL)) \
           AND (execution_run_id = $3 OR (execution_run_id IS NULL AND $3 IS NULL)) \
         RETURNING id",
    )
    .bind(issue.id)
    .bind(issue.checkout_run_id)
    .bind(issue.execution_run_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.is_some())
}

/// 仅用于调试 / 测试的辅助函数：返回当前 sweep 候选总数。
pub async fn count_stale_lock_candidates(db: &Db) -> sqlx::Result<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE checkout_run_id IS NOT NULL OR execution_run_id IS NOT NULL",
    )
    .fetch_one(db.pool())
    .await?;
    Ok(count)
}

/// 用于单元测试的辅助：判断某 status 是否 terminal。
pub fn is_terminal_run_status(status: &str) -> bool {
    TERMINAL_HEARTBEAT_RUN_STATUSES.contains(&status)
}

/// 暴露给单元测试的辅助：构建带 details 的 Value（避免调用方重复实现）。
#[cfg(test)]
pub fn build_stale_lock_cleared_details(
    cleared_checkout_run_id: Option<Uuid>,
    cleared_execution_run_id: Option<Uuid>,
    run_status_by_id: &std::collections::HashMap<Uuid, String>,
) -> Value {
    json!({
        "source": "recovery.sweep_stale_issue_locks",
        "clearedCheckoutRunId": cleared_checkout_run_id,
        "clearedExecutionRunId": cleared_execution_run_id,
        "referencedRunStatuses": run_status_by_id
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<std::collections::HashMap<_, _>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_statuses_match_node_set() {
        assert!(is_terminal_run_status("succeeded"));
        assert!(is_terminal_run_status("interrupted"));
        assert!(is_terminal_run_status("failed"));
        assert!(is_terminal_run_status("cancelled"));
        assert!(is_terminal_run_status("timed_out"));
    }

    #[test]
    fn non_terminal_statuses_excluded() {
        assert!(!is_terminal_run_status("running"));
        assert!(!is_terminal_run_status("queued"));
        assert!(!is_terminal_run_status("claimed"));
        assert!(!is_terminal_run_status("paused"));
        assert!(!is_terminal_run_status(""));
    }

    #[test]
    fn cleanable_logic_handles_missing_and_terminal() {
        let mut map = std::collections::HashMap::new();
        let terminal_id = Uuid::new_v4();
        let running_id = Uuid::new_v4();
        map.insert(terminal_id, "failed".to_string());
        map.insert(running_id, "running".to_string());
        // terminal run → cleanable
        assert!(is_cleanable(&map, Some(terminal_id)));
        // running run → not cleanable
        assert!(!is_cleanable(&map, Some(running_id)));
        // missing run → cleanable
        let ghost_id = Uuid::new_v4();
        assert!(is_cleanable(&map, Some(ghost_id)));
        // None run_id → cleanable (defensive)
        assert!(is_cleanable(&map, None));
    }

    #[test]
    fn build_details_includes_all_fields() {
        let mut map = std::collections::HashMap::new();
        let run_id = Uuid::new_v4();
        map.insert(run_id, "failed".to_string());
        let details = build_stale_lock_cleared_details(Some(run_id), None, &map);
        assert_eq!(details["source"], "recovery.sweep_stale_issue_locks");
        assert_eq!(details["clearedCheckoutRunId"], json!(run_id));
        assert_eq!(details["clearedExecutionRunId"], Value::Null);
        let statuses = details["referencedRunStatuses"].as_object().unwrap();
        assert_eq!(statuses.len(), 1);
    }
}
