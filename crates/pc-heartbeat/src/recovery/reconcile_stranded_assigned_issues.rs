//! `reconcileStrandedAssignedIssues` 顶级 recovery 编排器（骨架）。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `reconcileStrandedAssignedIssues`：
//! 扫描 assigned issues (todo/in_progress/in_review) 检查是否 stranded，
//! 如果是则根据 issue 状态触发不同 recovery action。
//!
//! Round 313 范围：骨架 + 候选查询 + 5 个早期 skip 过滤器。
//! 完整实现（包括 escalation / enqueueStrandedIssueRecovery /
//! enqueueInitialAssignedTodoDispatch）由 Round 314-315 渐进完成。
//!
//! 业务规则（与 Node 1:1）：
//! 1. Candidate query: status IN ('todo','in_progress','in_review')
//!    AND (assignee_agent_id IS NOT NULL OR status='in_review')
//!    AND assignee_user_id IS NULL
//!    AND issueCreatedAtGte 过滤可选
//! 2. parseIssueExecutionState（仅 in_review）
//! 3. resolve participantAgent（in_review → participantAgent，否则 → issue.assigneeAgentId）
//! 4. agentId 为空 → skip
//! 5. agent 不可 invoke（且 status != in_review）→ skip
//! 6. hasActiveExecutionPath → skip
//! 7. hasPendingWakeInteraction → skip
//! 8. isAutomaticRecoverySuppressedByPauseHold → skip
//! 9. 后续：latestRun + provider quota + escalation + enqueue（Round 314-315）
//!
//! 设计：
//! - 纯函数（决策）vs DB 副作用 分层：早期 skip 决策在本函数内；
//!   后续 recovery action 委托给独立模块（避免单文件膨胀）
//! - 返回 `StrandedReconcileResult` 聚合所有 counter
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

use super::continuation_observation::{
    get_latest_accepted_continuation_interaction, has_successful_run_since,
};
use super::pause_hold_guard::is_automatic_recovery_suppressed_by_pause_hold;

// ============================================================================
// Public types
// ============================================================================

/// `reconcileStrandedAssignedIssues` 选项。
#[derive(Debug, Clone, Default)]
pub struct ReconcileStrandedOptions {
    /// 仅扫描创建时间 >= 该阈值的 issues（与 Node `issueCreatedAtGte` 对齐）。
    pub issue_created_at_gte: Option<DateTime<Utc>>,
    /// 限定单个公司（None = 全公司）。
    pub company_id: Option<Uuid>,
    /// 注入的 "now"（便于测试）。
    pub now: Option<DateTime<Utc>>,
}

/// 已解析的 execution state（与 Node `parseIssueExecutionState` 对齐）。
#[derive(Debug, Clone)]
pub struct ParsedExecutionState {
    pub status: String, // "pending" | "running" | "completed" | "failed"
    pub current_stage_id: Option<String>,
    pub current_stage_type: Option<String>,
    pub current_participant: Option<ExecutionStateParticipant>,
}

/// execution state 中的 participant。
#[derive(Debug, Clone)]
pub struct ExecutionStateParticipant {
    pub participant_type: String, // "agent" | "user"
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
}

/// 候选 issue（query_issue_candidates 输出）。
#[derive(Debug, Clone)]
pub struct StrandedCandidate {
    pub id: Uuid,
    pub company_id: Uuid,
    pub status: String,
    pub assignee_agent_id: Option<Uuid>,
    pub execution_state: Option<Value>,
    pub created_at: DateTime<Utc>,
}

/// 单个 candidate 的早期 skip 决策结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrandedSkipReason {
    /// 没有可用的 agentId（in_review 无 participant 且 issue 无 assignee）。
    NoAgentId,
    /// agent 不存在 / 跨公司 / 不可 invoke（且 status != in_review）。
    AgentNotInvokable,
    /// 已有 active execution path。
    ActiveExecutionPath,
    /// 已有 pending wake interaction。
    PendingWakeInteraction,
    /// pause-hold 抑制闸门触发。
    PauseHoldSuppressed,
}

/// 单个 candidate 的早期决策结果。
#[derive(Debug, Clone)]
pub enum StrandedEarlyDecision {
    /// 进入后续流程（Round 314+ 处理）。
    Proceed {
        agent_id: Uuid,
        parsed_execution_state: Option<ParsedExecutionState>,
    },
    /// 被早期过滤器跳过。
    Skip(StrandedSkipReason),
}

/// 聚合 counter。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrandedReconcileResult {
    pub candidates_scanned: u32,
    pub candidates_proceeded: u32,
    pub skipped: u32,
    pub skipped_no_agent: u32,
    pub skipped_agent_not_invokable: u32,
    pub skipped_active_execution: u32,
    pub skipped_pending_wake: u32,
    pub skipped_pause_hold: u32,
    // Round 314+ 预留位
    pub assignment_dispatched: u32,
    pub dispatch_requeued: u32,
    pub continuation_requeued: u32,
    pub productive_continuation_observed: u32,
    pub successful_continuation_observed: u32,
    pub orphan_blockers_assigned: u32,
    pub successful_run_handoff_escalated: u32,
    pub review_participant_requeued: u32,
    pub escalated: u32,
    pub waiting_on_review_resolved: u32,
    pub provider_quota_monitored: u32,
    pub recent_progress_exempted: u32,
    pub issue_ids: Vec<Uuid>,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 顶级入口：扫描 stranded assigned issues 并做早期 skip 决策。
///
/// 与 Node `reconcileStrandedAssignedIssues` 行为对齐的子集：
/// - 候选查询 ✅
/// - 早期 skip 决策（agent_id / invokable / active_execution / pending_wake / pause_hold）✅
/// - 后续 recovery action（escalate / enqueue / provider_quota）⏳ Round 314-315
///
/// 返回 `StrandedReconcileResult`：counter + 已 proceed 的 issue_ids。
pub async fn reconcile_stranded_assigned_issues(
    db: &Db,
    opts: ReconcileStrandedOptions,
) -> sqlx::Result<StrandedReconcileResult> {
    let mut result = StrandedReconcileResult::default();

    let candidates = query_stranded_candidates(db, &opts).await?;
    result.candidates_scanned = candidates.len() as u32;

    for candidate in candidates {
        let agent_id = resolve_agent_id_for_candidate(&candidate);
        let Some(agent_id) = agent_id else {
            result.skipped += 1;
            result.skipped_no_agent += 1;
            continue;
        };

        // Step 2: agent 是否 invokable
        // （简化：暂用 has_active_agents 启发式；Round 314 替换为完整 isAgentInvokable）
        let agent_invokable = is_agent_invokable_simple(db, &candidate, agent_id).await?;
        if candidate.status != "in_review" && !agent_invokable {
            result.skipped += 1;
            result.skipped_agent_not_invokable += 1;
            continue;
        }

        // Step 3: active execution path
        if has_active_execution_path_for_issue(db, &candidate, agent_id).await? {
            result.skipped += 1;
            result.skipped_active_execution += 1;
            continue;
        }

        // Step 4: pending wake interaction
        if has_pending_wake_interaction_for_issue(db, candidate.company_id, candidate.id).await? {
            result.skipped += 1;
            result.skipped_pending_wake += 1;
            continue;
        }

        // Step 5: pause hold 抑制
        if is_automatic_recovery_suppressed_by_pause_hold(db, candidate.company_id, candidate.id)
            .await?
            .is_some()
        {
            result.skipped += 1;
            result.skipped_pause_hold += 1;
            continue;
        }

        // 全部早期过滤器通过，先做 continuation observation（Round 314）
        if let Some(interaction) =
            get_latest_accepted_continuation_interaction(db, candidate.company_id, candidate.id)
                .await?
        {
            let resolved_at = interaction.effective_resolution_time();
            let successful_run = has_successful_run_since(
                db,
                candidate.company_id,
                agent_id,
                candidate.id,
                resolved_at,
                Some(interaction.id),
            )
            .await?;
            if successful_run.is_some() {
                result.successful_continuation_observed += 1;
                result.issue_ids.push(candidate.id);
                continue;
            }
            // 没有 successful run since resolution → 进入 continuation requeue 路径
            // （Round 315 实现具体的 enqueue；本轮仅记录 productive_continuation_observed 计数）
            result.productive_continuation_observed += 1;
            result.issue_ids.push(candidate.id);
            continue;
        }

        // 全部早期过滤器通过，进入后续流程
        let parsed = if candidate.status == "in_review" {
            parse_issue_execution_state(candidate.execution_state.as_ref())
        } else {
            None
        };
        result.candidates_proceeded += 1;
        result.issue_ids.push(candidate.id);
        let _ = parsed;
        let _ = agent_id;
    }

    Ok(result)
}

// ============================================================================
// Query: candidates
// ============================================================================

/// 查询候选 issues。
///
/// 与 Node 行为对齐：
/// - status IN ('todo','in_progress','in_review')
/// - (assignee_agent_id IS NOT NULL OR status='in_review')
/// - assignee_user_id IS NULL
/// - issue_created_at_gte 过滤（可选）
async fn query_stranded_candidates(
    db: &Db,
    opts: &ReconcileStrandedOptions,
) -> sqlx::Result<Vec<StrandedCandidate>> {
    let mut conds: Vec<String> = vec![
        "assignee_user_id IS NULL".to_string(),
        "status IN ('todo', 'in_progress', 'in_review')".to_string(),
        "(assignee_agent_id IS NOT NULL OR status = 'in_review')".to_string(),
    ];
    if let Some(cid) = opts.company_id {
        conds.push(format!("company_id = '{}'", cid));
    }
    if let Some(gte) = opts.issue_created_at_gte {
        conds.push(format!("created_at >= '{}'", gte.to_rfc3339()));
    }
    let sql = format!(
        "SELECT id, company_id, status::text, assignee_agent_id, execution_state, created_at \
         FROM issues \
         WHERE {} \
         ORDER BY created_at ASC, id ASC",
        conds.join(" AND "),
    );
    let rows = sqlx::query(&sql).fetch_all(db.pool()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(StrandedCandidate {
            id: row.try_get("id")?,
            company_id: row.try_get("company_id")?,
            status: row.try_get("status")?,
            assignee_agent_id: row.try_get("assignee_agent_id").ok().flatten(),
            execution_state: row.try_get("execution_state").ok().flatten(),
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(out)
}

// ============================================================================
// Helpers
// ============================================================================

/// 解析 agent_id：in_review → participantAgent，否则 → issue.assigneeAgentId。
fn resolve_agent_id_for_candidate(candidate: &StrandedCandidate) -> Option<Uuid> {
    if candidate.status == "in_review" {
        let parsed = parse_issue_execution_state(candidate.execution_state.as_ref());
        if let Some(p) = parsed {
            if let Some(participant) = &p.current_participant {
                if participant.participant_type == "agent" {
                    return participant.agent_id;
                }
            }
        }
        // in_review 无 participant → 退化用 assignee_agent_id
        candidate.assignee_agent_id
    } else {
        candidate.assignee_agent_id
    }
}

/// 解析 issue.execution_state JSON。
///
/// 与 Node `parseIssueExecutionState` 对齐（简化版）：
/// - JSON 格式：`{"status": "pending", "currentParticipant": {"type":"agent","agentId":"..."}, ...}`
/// - 解析失败 → 返回 None（与 Node 行为一致）
pub fn parse_issue_execution_state(raw: Option<&Value>) -> Option<ParsedExecutionState> {
    let value = raw?;
    let obj = value.as_object()?;
    let status = obj.get("status")?.as_str()?.to_string();
    let current_stage_id = obj
        .get("currentStageId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_stage_type = obj
        .get("currentStageType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_participant = obj.get("currentParticipant").and_then(|p| {
        let ptype = p.get("type")?.as_str()?.to_string();
        let agent_id = p
            .get("agentId")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let user_id = p
            .get("userId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Some(ExecutionStateParticipant {
            participant_type: ptype,
            agent_id,
            user_id,
        })
    });
    Some(ParsedExecutionState {
        status,
        current_stage_id,
        current_stage_type,
        current_participant,
    })
}

/// 简化版 agent invokable 检查：
/// - agent 必须存在
/// - agent.company_id == issue.company_id
/// - agent.status != 'offline'
///
/// （与 Node `isAgentInvokable` 对齐的最小可用版本，Round 314 扩展）
async fn is_agent_invokable_simple(
    db: &Db,
    candidate: &StrandedCandidate,
    agent_id: Uuid,
) -> sqlx::Result<bool> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT company_id, status::text FROM agents WHERE id = $1")
            .bind(agent_id)
            .fetch_optional(db.pool())
            .await?;
    let (agent_company_id, agent_status) = match row {
        Some(r) => r,
        None => return Ok(false),
    };
    if agent_company_id != candidate.company_id {
        return Ok(false);
    }
    Ok(agent_status != "offline")
}

/// 简化版 active execution path 检查：
/// - 有 heartbeat_runs 中 status IN ('queued','running','scheduled_retry') + context_snapshot 含 issueId
/// - 或 issue.execution_run_id IS NOT NULL
async fn has_active_execution_path_for_issue(
    db: &Db,
    candidate: &StrandedCandidate,
    _agent_id: Uuid,
) -> sqlx::Result<bool> {
    // 路径 1: heartbeat_runs
    let row1: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM heartbeat_runs \
         WHERE company_id = $1 \
           AND status::text = ANY($2) \
           AND (context_snapshot->>'issueId' = $3 OR context_snapshot->>'taskId' = $3) \
         LIMIT 1",
    )
    .bind(candidate.company_id)
    .bind(&["queued", "running", "scheduled_retry"][..])
    .bind(candidate.id.to_string())
    .fetch_optional(db.pool())
    .await?;
    if row1.is_some() {
        return Ok(true);
    }

    // 路径 2: issues.execution_run_id
    let row2: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT execution_run_id FROM issues WHERE id = $1")
            .bind(candidate.id)
            .fetch_optional(db.pool())
            .await?;
    Ok(row2.and_then(|(opt,)| opt).is_some())
}

/// pending wake interaction 检查：
/// - agent_wakeup_requests 中 status='queued' / 'deferred_issue_execution'
async fn has_pending_wake_interaction_for_issue(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM agent_wakeup_requests \
         WHERE company_id = $1 \
           AND status::text = ANY($2) \
           AND payload->>'issueId' = $3 \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(&["queued", "deferred_issue_execution"][..])
    .bind(issue_id.to_string())
    .fetch_optional(db.pool())
    .await?;
    Ok(row.is_some())
}
