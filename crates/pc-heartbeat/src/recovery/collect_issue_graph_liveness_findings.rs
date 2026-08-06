//! `collectIssueGraphLivenessFindings` DB→Finding 收集器。
//!
//! 对齐 Node `services/recovery/service.ts` 的
//! `collectIssueGraphLivenessFindings`：从 8 个数据源（issues / relations /
//! agents / active runs / queued wakes / pending interactions / pending
//! approvals / open recovery issues + actions）聚合 `IssueGraphLivenessInput`，
//! 然后调 `classify_issue_graph_liveness` 得到 `Vec<IssueLivenessFinding>`。
//!
//! 设计：
//! - 8 个独立 helper 查询（每个函数单一职责）
//! - 主入口只做并行收集 + 字段映射
//! - 纯函数：parse 函数单独抽出（parse_object_field / parse_issue_id_*）
//! - 公司可选择性过滤（company_id: Option<Uuid>）
//!
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

use super::issue_graph_liveness::{
    classify_issue_graph_liveness, IssueGraphLivenessInput, IssueLivenessAgentInput,
    IssueLivenessExecutionPathInput, IssueLivenessIssueInput, IssueLivenessRelationInput,
    IssueLivenessWaitingPathInput,
};
use super::origins::parse_issue_graph_liveness_incident_key;

// ============================================================================
// Constants
// ============================================================================

/// Issue visibility 条件：hidden_at IS NULL AND harness_kind IS NULL
/// （与 Node `visibleIssueCondition()` 对齐）。
///
/// 注：当前 collect 函数通过 SQL 显式 WHERE 子句实现（`hidden_at IS NULL`），
/// `harness_kind` 列在 issue_graph_liveness 收集中按 Node 语义应也过滤 NULL。
const ESCALATION_ORIGIN_KIND: &str = "harness_liveness_escalation";
const STRANDED_RECOVERY_ORIGIN_KIND: &str = "stranded_issue_recovery";

/// Heartbeat run statuses that count as "active execution path"。
/// 与 Node `EXECUTION_PATH_HEARTBEAT_RUN_STATUSES` 对齐：`[queued, running, scheduled_retry]`。
const EXECUTION_PATH_HEARTBEAT_RUN_STATUSES: &[&str] = &["queued", "running", "scheduled_retry"];

/// Wakeup request statuses that count as "queued wake"。
/// 与 Node 中 collect 里的 `agentWakeupRequests` 过滤对齐：
/// `["queued", "deferred_issue_execution"]`。
const QUEUED_WAKE_STATUSES: &[&str] = &["queued", "deferred_issue_execution"];

/// Approval statuses that count as "pending approval"。
const PENDING_APPROVAL_STATUSES: &[&str] = &["pending", "revision_requested"];

/// Recovery action statuses that count as "open recovery issue source"。
const OPEN_RECOVERY_ACTION_STATUSES: &[&str] = &["active", "escalated"];

/// Issue statuses that count as terminal（exclude from open recovery issues）。
const TERMINAL_ISSUE_STATUSES: &[&str] = &["done", "cancelled"];

/// Wake payload 嵌套 context key（与 Node `DEFERRED_WAKE_CONTEXT_KEY` 对齐）。
const DEFERRED_WAKE_CONTEXT_KEY: &str = "_paperclipWakeContext";

// ============================================================================
// Public types
// ============================================================================

/// `collect_issue_graph_liveness_findings` 的输入选项。
#[derive(Debug, Clone, Default)]
pub struct CollectFindingsOptions {
    pub company_id: Option<Uuid>,
    /// 限制 issues 查询的 limit（默认：unbounded）。
    pub issue_limit: Option<i64>,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 主入口：从 DB 收集 `IssueGraphLivenessInput` 并跑 `classify_issue_graph_liveness`。
///
/// 与 Node `collectIssueGraphLivenessFindings` 对齐（精简版）：
/// - 8 个数据源并行收集（无 Promise.all：sqlx 不支持同连接并行，所以串行）
/// - `company_id` 过滤可选
/// - 输出 `Vec<IssueLivenessFinding>`（可能为空）
pub async fn collect_issue_graph_liveness_findings(
    db: &Db,
    opts: CollectFindingsOptions,
) -> sqlx::Result<Vec<super::issue_graph_liveness::IssueLivenessFinding>> {
    let issues = query_issues(db, &opts).await?;
    let relations = query_block_relations(db, &opts).await?;
    let agents = query_agents(db, &opts).await?;
    let active_runs = query_active_runs(db, &opts).await?;
    let queued_wake_requests = query_queued_wake_requests(db, &opts).await?;
    let pending_interactions = query_pending_interactions(db, &opts).await?;
    let pending_approvals = query_pending_approvals(db, &opts).await?;
    let open_recovery_issues = query_open_recovery_issues(db, &opts).await?;

    let input = IssueGraphLivenessInput {
        issues,
        relations,
        agents,
        active_runs,
        queued_wake_requests,
        pending_interactions,
        pending_approvals,
        open_recovery_issues,
        now: chrono::Utc::now(),
    };
    Ok(classify_issue_graph_liveness(&input))
}

// ============================================================================
// Query 1: issues
// ============================================================================

async fn query_issues(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessIssueInput>> {
    let mut conds: Vec<String> = vec![
        "hidden_at IS NULL".to_string(),
        "harness_kind IS NULL".to_string(),
        format!("origin_kind != '{}'", ESCALATION_ORIGIN_KIND),
    ];
    if let Some(cid) = opts.company_id {
        conds.push(format!("company_id = '{}'", cid));
    }
    let limit_clause = match opts.issue_limit {
        Some(n) => format!("LIMIT {}", n.max(1)),
        None => String::new(),
    };
    let sql = format!(
        "SELECT id, company_id, identifier, title, status::text AS status_text, \
                project_id, goal_id, parent_id, assignee_agent_id, assignee_user_id, \
                created_by_agent_id, created_by_user_id, execution_policy, \
                execution_state, monitor_next_check_at, monitor_attempt_count \
         FROM issues \
         WHERE {} \
         ORDER BY id ASC \
         {}",
        conds.join(" AND "),
        limit_clause
    );
    let rows = sqlx::query(&sql).fetch_all(db.pool()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(IssueLivenessIssueInput {
            id: row.try_get("id")?,
            company_id: row.try_get("company_id")?,
            identifier: row.try_get("identifier").ok(),
            title: row.try_get("title")?,
            status: row.try_get("status_text")?,
            project_id: row.try_get("project_id").ok(),
            goal_id: row.try_get("goal_id").ok(),
            parent_id: row.try_get("parent_id").ok(),
            assignee_agent_id: row.try_get("assignee_agent_id").ok(),
            assignee_user_id: row.try_get("assignee_user_id").ok(),
            created_by_agent_id: row.try_get("created_by_agent_id").ok(),
            created_by_user_id: row.try_get("created_by_user_id").ok(),
            execution_policy: row.try_get("execution_policy").ok(),
            execution_state: row.try_get("execution_state").ok(),
            monitor_next_check_at: row.try_get("monitor_next_check_at").ok(),
            monitor_attempt_count: row.try_get("monitor_attempt_count").ok(),
        });
    }
    Ok(out)
}

// ============================================================================
// Query 2: relations (blocks)
// ============================================================================

async fn query_block_relations(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessRelationInput>> {
    let company_filter = match opts.company_id {
        Some(cid) => format!("AND company_id = '{}'", cid),
        None => String::new(),
    };
    let sql = format!(
        "SELECT company_id, issue_id AS blocker_issue_id, related_issue_id AS blocked_issue_id \
         FROM issue_relations \
         WHERE type = 'blocks' {}",
        company_filter
    );
    let rows = sqlx::query(&sql).fetch_all(db.pool()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(IssueLivenessRelationInput {
            company_id: row.try_get("company_id")?,
            blocker_issue_id: row.try_get("blocker_issue_id")?,
            blocked_issue_id: row.try_get("blocked_issue_id")?,
        });
    }
    Ok(out)
}

// ============================================================================
// Query 3: agents
// ============================================================================

async fn query_agents(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessAgentInput>> {
    let company_filter = match opts.company_id {
        Some(cid) => format!("WHERE company_id = '{}'", cid),
        None => String::new(),
    };
    let sql = format!(
        "SELECT id, company_id, name, role, title, status::text AS status_text, reports_to \
         FROM agents {}",
        company_filter
    );
    let rows = sqlx::query(&sql).fetch_all(db.pool()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(IssueLivenessAgentInput {
            id: row.try_get("id")?,
            company_id: row.try_get("company_id")?,
            name: row.try_get("name")?,
            role: row.try_get("role").ok(),
            title: row.try_get("title").ok(),
            status: row.try_get("status_text")?,
            reports_to: row.try_get("reports_to").ok(),
        });
    }
    Ok(out)
}

// ============================================================================
// Query 4: active runs (heartbeat_runs with active statuses)
// ============================================================================

async fn query_active_runs(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessExecutionPathInput>> {
    let company_filter = match opts.company_id {
        Some(cid) => format!("AND company_id = '{}'", cid),
        None => String::new(),
    };
    let sql = format!(
        "SELECT company_id, agent_id, status::text AS status_text, context_snapshot \
         FROM heartbeat_runs \
         WHERE status::text = ANY($1) {}",
        company_filter
    );
    let rows = sqlx::query(&sql)
        .bind(EXECUTION_PATH_HEARTBEAT_RUN_STATUSES)
        .fetch_all(db.pool())
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let ctx: Option<Value> = row.try_get("context_snapshot").ok();
        let issue_id = issue_id_from_run_context(ctx.as_ref());
        out.push(IssueLivenessExecutionPathInput {
            company_id: row.try_get("company_id")?,
            issue_id,
            agent_id: row.try_get("agent_id").ok(),
            status: row.try_get("status_text").ok(),
        });
    }
    Ok(out)
}

// ============================================================================
// Query 5: queued wake requests
// ============================================================================

async fn query_queued_wake_requests(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessExecutionPathInput>> {
    let company_filter = match opts.company_id {
        Some(cid) => format!("AND company_id = '{}'", cid),
        None => String::new(),
    };
    let sql = format!(
        "SELECT company_id, agent_id, status::text AS status_text, payload \
         FROM agent_wakeup_requests \
         WHERE status::text = ANY($1) {}",
        company_filter
    );
    let rows = sqlx::query(&sql)
        .bind(QUEUED_WAKE_STATUSES)
        .fetch_all(db.pool())
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let payload: Option<Value> = row.try_get("payload").ok();
        let issue_id = issue_id_from_wake_payload(payload.as_ref());
        out.push(IssueLivenessExecutionPathInput {
            company_id: row.try_get("company_id")?,
            issue_id,
            agent_id: row.try_get("agent_id").ok(),
            status: row.try_get("status_text").ok(),
        });
    }
    Ok(out)
}

// ============================================================================
// Query 6: pending interactions
// ============================================================================

async fn query_pending_interactions(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessWaitingPathInput>> {
    let company_filter = match opts.company_id {
        Some(cid) => format!("AND company_id = '{}'", cid),
        None => String::new(),
    };
    let sql = format!(
        "SELECT company_id, issue_id, status::text AS status_text \
         FROM issue_thread_interactions \
         WHERE status::text = 'pending' {}",
        company_filter
    );
    let rows = sqlx::query(&sql).fetch_all(db.pool()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(IssueLivenessWaitingPathInput {
            company_id: row.try_get("company_id")?,
            issue_id: row.try_get("issue_id")?,
            status: row.try_get("status_text").ok(),
        });
    }
    Ok(out)
}

// ============================================================================
// Query 7: pending approvals (issue_approvals INNER JOIN approvals)
// ============================================================================

async fn query_pending_approvals(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessWaitingPathInput>> {
    let company_filter = match opts.company_id {
        Some(cid) => format!("AND ia.company_id = '{}'", cid),
        None => String::new(),
    };
    let sql = format!(
        "SELECT ia.company_id, ia.issue_id, a.status::text AS status_text \
         FROM issue_approvals ia \
         INNER JOIN approvals a ON a.id = ia.approval_id \
         WHERE a.status::text = ANY($1) {}",
        company_filter
    );
    let rows = sqlx::query(&sql)
        .bind(PENDING_APPROVAL_STATUSES)
        .fetch_all(db.pool())
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(IssueLivenessWaitingPathInput {
            company_id: row.try_get("company_id")?,
            issue_id: row.try_get("issue_id")?,
            status: row.try_get("status_text").ok(),
        });
    }
    Ok(out)
}

// ============================================================================
// Query 8: open recovery issues (stranded + escalation) + active recovery actions
// ============================================================================

async fn query_open_recovery_issues(
    db: &Db,
    opts: &CollectFindingsOptions,
) -> sqlx::Result<Vec<IssueLivenessWaitingPathInput>> {
    let company_filter = match opts.company_id {
        Some(cid) => format!("AND company_id = '{}'", cid),
        None => String::new(),
    };

    // Sub-query 1: open recovery issues (stranded + escalation) — issue_ids that are "blocking/related"
    let sql1 = format!(
        "SELECT company_id, id, status::text AS status_text, origin_kind, origin_id \
         FROM issues \
         WHERE hidden_at IS NULL \
           AND origin_kind = ANY($1) \
           AND status::text != ALL($2) {}",
        company_filter
    );
    let rows1 = sqlx::query(&sql1)
        .bind(&[STRANDED_RECOVERY_ORIGIN_KIND, ESCALATION_ORIGIN_KIND][..])
        .bind(TERMINAL_ISSUE_STATUSES)
        .fetch_all(db.pool())
        .await?;

    let mut out = Vec::new();
    for row in rows1 {
        let origin_kind: String = row.try_get("origin_kind")?;
        let origin_id: Option<String> = row.try_get("origin_id").ok();
        let company_id: Uuid = row.try_get("company_id")?;
        let status_text: String = row.try_get("status_text")?;
        if origin_kind == ESCALATION_ORIGIN_KIND {
            // parse incident_key → { company_id, issue_id, state, leaf_issue_id }
            if let Some(key) = origin_id {
                if let Some(parsed) = parse_issue_graph_liveness_incident_key(Some(&key)) {
                    if parsed.company_id == company_id.to_string() {
                        // issue_id 和 leaf_issue_id 都算 open recovery issue
                        let main_id = Uuid::parse_str(&parsed.issue_id).ok();
                        let leaf_id = Uuid::parse_str(&parsed.leaf_issue_id).ok();
                        if let Some(main_id) = main_id {
                            out.push(IssueLivenessWaitingPathInput {
                                company_id,
                                issue_id: main_id,
                                status: Some(status_text.clone()),
                            });
                        }
                        if let Some(leaf_id) = leaf_id {
                            out.push(IssueLivenessWaitingPathInput {
                                company_id,
                                issue_id: leaf_id,
                                status: Some(status_text.clone()),
                            });
                        }
                    }
                }
            }
        } else if let Some(origin_id_str) = origin_id {
            // stranded_issue_recovery：origin_id = source_issue_id
            if let Ok(issue_id) = Uuid::parse_str(&origin_id_str) {
                out.push(IssueLivenessWaitingPathInput {
                    company_id,
                    issue_id,
                    status: Some(status_text),
                });
            }
        }
    }

    // Sub-query 2: active / escalated issue_recovery_actions — collect source_issue_ids under analysis
    let issue_ids_filter = match opts.company_id {
        Some(cid) => format!("AND company_id = '{}'", cid),
        None => String::new(),
    };
    let sql2 = format!(
        "SELECT DISTINCT company_id, source_issue_id AS issue_id \
         FROM issue_recovery_actions \
         WHERE status::text = ANY($1) {} \
         ORDER BY source_issue_id",
        issue_ids_filter
    );
    let rows2 = sqlx::query(&sql2)
        .bind(OPEN_RECOVERY_ACTION_STATUSES)
        .fetch_all(db.pool())
        .await?;
    for row in rows2 {
        out.push(IssueLivenessWaitingPathInput {
            company_id: row.try_get("company_id")?,
            issue_id: row.try_get("issue_id")?,
            status: None,
        });
    }
    Ok(out)
}

// ============================================================================
// Helpers (pure)
// ============================================================================

/// 从 heartbeat run context_snapshot 提取 issue_id（与 Node `issueIdFromRunContext` 对齐）。
///
/// 优先 `issueId`，回退 `taskId`。两者都缺失返回 None。
fn issue_id_from_run_context(ctx: Option<&Value>) -> Option<Uuid> {
    let ctx = ctx?;
    let obj = ctx.as_object()?;
    if let Some(s) = obj.get("issueId").and_then(|v| v.as_str()) {
        if let Ok(id) = Uuid::parse_str(s) {
            return Some(id);
        }
    }
    if let Some(s) = obj.get("taskId").and_then(|v| v.as_str()) {
        if let Ok(id) = Uuid::parse_str(s) {
            return Some(id);
        }
    }
    None
}

/// 从 wakeup request payload 提取 issue_id（与 Node `issueIdFromWakePayload` 对齐）。
///
/// 优先 `issueId`，回退 `_paperclipWakeContext.issueId`，再回退 `_paperclipWakeContext.taskId`。
fn issue_id_from_wake_payload(payload: Option<&Value>) -> Option<Uuid> {
    let payload = payload?;
    let obj = payload.as_object()?;
    if let Some(s) = obj.get("issueId").and_then(|v| v.as_str()) {
        if let Ok(id) = Uuid::parse_str(s) {
            return Some(id);
        }
    }
    if let Some(nested) = obj
        .get(DEFERRED_WAKE_CONTEXT_KEY)
        .and_then(|v| v.as_object())
    {
        if let Some(s) = nested.get("issueId").and_then(|v| v.as_str()) {
            if let Ok(id) = Uuid::parse_str(s) {
                return Some(id);
            }
        }
        if let Some(s) = nested.get("taskId").and_then(|v| v.as_str()) {
            if let Ok(id) = Uuid::parse_str(s) {
                return Some(id);
            }
        }
    }
    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn issue_id_from_run_context_prefers_issue_id() {
        let ctx = json!({"issueId": "11111111-1111-1111-1111-111111111111", "taskId": "22222222-2222-2222-2222-222222222222"});
        let id = issue_id_from_run_context(Some(&ctx)).unwrap();
        assert_eq!(id.to_string(), "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn issue_id_from_run_context_falls_back_to_task_id() {
        let ctx = json!({"taskId": "22222222-2222-2222-2222-222222222222"});
        let id = issue_id_from_run_context(Some(&ctx)).unwrap();
        assert_eq!(id.to_string(), "22222222-2222-2222-2222-222222222222");
    }

    #[test]
    fn issue_id_from_run_context_returns_none_when_missing() {
        let ctx = json!({"foo": "bar"});
        assert!(issue_id_from_run_context(Some(&ctx)).is_none());
        assert!(issue_id_from_run_context(None).is_none());
    }

    #[test]
    fn issue_id_from_wake_payload_handles_deferred_wake_context() {
        let payload = json!({
            "_paperclipWakeContext": {"taskId": "33333333-3333-3333-3333-333333333333"}
        });
        let id = issue_id_from_wake_payload(Some(&payload)).unwrap();
        assert_eq!(id.to_string(), "33333333-3333-3333-3333-333333333333");
    }

    #[test]
    fn issue_id_from_wake_payload_prefers_top_level_issue_id() {
        let payload = json!({
            "issueId": "11111111-1111-1111-1111-111111111111",
            "_paperclipWakeContext": {"issueId": "22222222-2222-2222-2222-222222222222"}
        });
        let id = issue_id_from_wake_payload(Some(&payload)).unwrap();
        assert_eq!(id.to_string(), "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn constants_match_node() {
        assert_eq!(
            EXECUTION_PATH_HEARTBEAT_RUN_STATUSES,
            &["queued", "running", "scheduled_retry"]
        );
        assert_eq!(
            QUEUED_WAKE_STATUSES,
            &["queued", "deferred_issue_execution"]
        );
        assert_eq!(
            PENDING_APPROVAL_STATUSES,
            &["pending", "revision_requested"]
        );
        assert_eq!(OPEN_RECOVERY_ACTION_STATUSES, &["active", "escalated"]);
        assert_eq!(TERMINAL_ISSUE_STATUSES, &["done", "cancelled"]);
        assert_eq!(DEFERRED_WAKE_CONTEXT_KEY, "_paperclipWakeContext");
        assert_eq!(ESCALATION_ORIGIN_KIND, "harness_liveness_escalation");
        assert_eq!(STRANDED_RECOVERY_ORIGIN_KIND, "stranded_issue_recovery");
    }
}
