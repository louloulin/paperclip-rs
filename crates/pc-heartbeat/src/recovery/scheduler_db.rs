//! Recovery scheduler 的 DB 接入层。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `ensureSourceScopedStrandedRecoveryAction`
//! **DB 部分**：
//! 1. 从 DB 读 issue + latest heartbeat run
//! 2. 在 DB 上解析路由（invokability 检查 + manager ladder）
//! 3. 调用 `decide_recovery_scheduler_plan` 生成 plan
//! 4. 通过 `persist_source_scoped_recovery_action` + `persist_recovery_wake` 写库并 dispatch wake
//!
//! 边界：
//! - 与纯计划层 `scheduler.rs` 分开：纯计划层无副作用，本文件才做 DB I/O
//! - 调用方只需提供 issue_id + 必要 override + wake template

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use pc_core::agent_eligibility::is_agent_status_invokable;
use pc_repos::agent::{AgentRepo, NewAgentWakeupRequest};
use pc_repos::issue::{IssueRepo, UpsertRecoveryAction};
use pc_repos::Db;

use super::orchestrator::{
    ensure_source_scoped_recovery_action, persist_recovery_wake,
    persist_source_scoped_recovery_action, recovery_action_wake_input, RecoveryDispatchIntent,
    RecoveryOrchestrationResult,
};
use super::scheduler::{
    decide_recovery_cause, decide_recovery_scheduler_plan, read_context_retry_reason,
    SchedulerContext, SchedulerRoutingHints, SchedulerRunInput,
};
use super::source_scoped_recovery_action::StrandedRecoveryCause;
use crate::wake_dedup::WakeSnapshot;

/// DB 接入层调度输入。
#[derive(Debug, Clone)]
pub struct SchedulerDbInput {
    pub issue_id: Uuid,
    pub previous_status: Option<String>,
    pub recovery_cause_override: Option<StrandedRecoveryCause>,
    pub recovery_owner_agent_id: Option<Uuid>,
    pub successful_run_handoff_evidence: Option<Value>,
    pub workspace_validation_fingerprint_override: Option<String>,
}

/// DB 接入层输出：含完整 orchestration 结果（含 dispatched wake）。
#[derive(Debug, Clone)]
pub struct SchedulerDbResult {
    pub cause: StrandedRecoveryCause,
    pub result: RecoveryOrchestrationResult,
}

/// 主入口：保证 source issue 拥有最新的 source-scoped recovery action。
///
/// 与 Node `ensureSourceScopedStrandedRecoveryAction` 行为对齐：
/// - 读 issue / latest run / 所有 candidate agents
/// - 决定 cause + routing hints
/// - 写 recovery action（如已有 active action 则 upsert 覆盖）
/// - 必要时 dispatch wake
pub async fn ensure_source_scoped_recovery_action_for_issue(
    db: &Db,
    input: SchedulerDbInput,
    existing_wake: Option<&WakeSnapshot>,
    wake_template: NewAgentWakeupRequest,
) -> sqlx::Result<Option<SchedulerDbResult>> {
    let Some(issue) = IssueRepo::new(db).get(input.issue_id).await? else {
        return Ok(None);
    };
    let Some(latest_run_row) = load_latest_run_row(db, issue.company_id, input.issue_id).await?
    else {
        return Ok(None);
    };
    let agents = AgentRepo::new(db).list_by_company(issue.company_id).await?;
    let routing = resolve_stranded_recovery_routing_db(
        &issue,
        &latest_run_row,
        &agents,
        input.recovery_owner_agent_id,
    );
    let ctx = SchedulerContext {
        company_id: issue.company_id,
        source_issue_id: issue.id,
        recovery_cause_override: input.recovery_cause_override,
        successful_run_handoff_evidence: input.successful_run_handoff_evidence,
        workspace_validation_fingerprint_override: input.workspace_validation_fingerprint_override,
    };
    let run_input = build_run_input(&latest_run_row);
    let now: DateTime<Utc> = Utc::now();
    let candidate = decide_recovery_scheduler_plan(&ctx, &run_input, &routing, now);
    let upsert = build_upsert_from_candidate(&ctx, &candidate, &issue.assignee_agent_id);
    let result = ensure_source_scoped_recovery_action(
        db,
        &AgentRepo::new(db),
        &upsert,
        existing_wake,
        wake_template,
    )
    .await?;
    Ok(Some(SchedulerDbResult {
        cause: candidate.cause,
        result,
    }))
}

/// 仅扫描 + 写 recovery_action 的 sweep（不升级 issue）。
///
/// 与 Node `reconcileStrandedAssignedIssues` 精简版对齐：
/// - SELECTs (todo/in_progress/in_review) 且有 assignee_agent_id 的 issue
/// - 对每个 candidate 跳过仍在 active execution path 上的 issue
/// - 其余调用 `ensure_source_scoped_recovery_action_for_issue`
///
/// 返回每个 candidate 的最终结果（cause + dispatch intent）。
pub async fn reconcile_stranded_assigned_issues_for_company(
    db: &Db,
    company_id: Uuid,
    existing_wake: Option<&WakeSnapshot>,
    wake_template: NewAgentWakeupRequest,
    max_candidates: i64,
) -> sqlx::Result<ReconcileSweepResult> {
    let candidates = IssueRepo::new(db)
        .list_stranded_candidates(company_id, max_candidates)
        .await?;
    let mut result = ReconcileSweepResult::default();
    for issue in candidates {
        let has_active_path = IssueRepo::new(db)
            .has_active_execution_path(issue.id)
            .await?;
        if has_active_path {
            result.skipped += 1;
            continue;
        }
        let db_input = SchedulerDbInput {
            issue_id: issue.id,
            previous_status: Some(issue.status.clone()),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        };
        match ensure_source_scoped_recovery_action_for_issue(
            db,
            db_input,
            existing_wake,
            wake_template.clone(),
        )
        .await
        {
            Ok(Some(scheduler_result)) => {
                result.dispatched += 1;
                result.outcomes.push(ReconcileSweepOutcome {
                    issue_id: issue.id,
                    cause: scheduler_result.cause,
                    dispatch_intent: scheduler_result.result.persisted.dispatch.clone(),
                });
            }
            Ok(None) => {
                result.skipped += 1;
            }
            Err(error) => {
                result.failed += 1;
                result.errors.push((issue.id, error.to_string()));
            }
        }
    }
    Ok(result)
}

/// 扫描 + 升级 二合一 sweep：先写 recovery action，再把 source issue 切到 blocked。
///
/// 与 Node `reconcileStrandedAssignedIssues` 完整副作用对齐：
/// - SELECTs stranded candidates
/// - 对每个 candidate 跳过仍在 active execution path 上的 issue
/// - 其余先调 `ensure_source_scoped_recovery_action_for_issue`（写 recovery action）
/// - 再调 `escalate_stranded_assigned_issue`（切 source issue 为 blocked + 写 escalation comment）
pub async fn reconcile_and_escalate_stranded_for_company(
    db: &Db,
    company_id: Uuid,
    existing_wake: Option<&WakeSnapshot>,
    wake_template: NewAgentWakeupRequest,
    max_candidates: i64,
) -> sqlx::Result<ReconcileAndEscalateSweepResult> {
    use super::escalate_db::{escalate_stranded_assigned_issue_with_comment, EscalateDbInput};
    use super::pause_hold_guard::is_automatic_recovery_suppressed_by_pause_hold;
    let candidates = IssueRepo::new(db)
        .list_stranded_candidates(company_id, max_candidates)
        .await?;
    let mut result = ReconcileAndEscalateSweepResult::default();
    for issue in candidates {
        // Guard 1: skip if any active pause-hold is set on this issue or its ancestors
        let suppressed = is_automatic_recovery_suppressed_by_pause_hold(db, company_id, issue.id)
            .await
            .unwrap_or(None);
        if suppressed.is_some() {
            result.skipped += 1;
            continue;
        }
        // Guard 2: skip if there's still an active execution path
        let has_active_path = IssueRepo::new(db)
            .has_active_execution_path(issue.id)
            .await?;
        if has_active_path {
            result.skipped += 1;
            continue;
        }
        let issue_latest_run = load_latest_run_row(db, issue.company_id, issue.id).await?;
        let monitor_run = if issue.status == "in_review" {
            if let Some(participant_agent_id) = current_review_participant_agent_id(&issue) {
                load_latest_run_row_for_agent(db, issue.company_id, issue.id, participant_agent_id)
                    .await?
            } else {
                None
            }
        } else {
            issue_latest_run.clone()
        };
        if let Some(run) = monitor_run.as_ref() {
            let now = Utc::now();
            let is_unsuccessful_terminal = matches!(
                run.status.as_str(),
                "interrupted" | "failed" | "cancelled" | "timed_out"
            );
            let classification = is_unsuccessful_terminal.then(|| {
                super::adapter_failure_classification::classify_adapter_failure(
                    run.error.as_deref(),
                    run.error_code.as_deref(),
                    run.result_json.as_ref(),
                    now,
                )
            });
            if let Some(Some(
                super::adapter_failure_classification::AdapterFailureRecoveryClassification::ProviderQuota {
                    retry_at,
                    parsed_reset_time,
                },
            )) = classification
            {
                let target_agent_id = if issue.status == "in_review" {
                    current_review_participant_agent_id(&issue)
                } else {
                    issue.assignee_agent_id
                };
                if target_agent_id.is_some() && target_agent_id == run.agent_id {
                    let monitored = super::schedule_provider_quota_recovery_monitor::schedule_provider_quota_recovery_monitor(
                        db,
                        super::schedule_provider_quota_recovery_monitor::ScheduleProviderQuotaRecoveryMonitorInput {
                            company_id: issue.company_id,
                            issue_id: issue.id,
                            latest_run_id: run.id,
                            target_agent_id: target_agent_id.expect("checked above"),
                            retry_at,
                            parsed_reset_time,
                            now: Some(now),
                        },
                    )
                    .await?;
                    if monitored.is_some() {
                        super::schedule_provider_quota_recovery_monitor::persist_provider_quota_recovery_classification(
                            db,
                            issue.company_id,
                            run.id,
                            retry_at,
                        )
                        .await?;
                        result.provider_quota_monitored += 1;
                    } else {
                        result.skipped += 1;
                    }
                    continue;
                }
            }
        }
        // Guard 3: continuation retry backoff gate (only when an assignee agent exists).
        //
        // Aligns with Node `enqueueStrandedIssueRecovery` main decision gate:
        // - latest run's error_code drives the summary lookup
        // - if should_skip_due_to_backoff: skip this round (don't escalate, don't schedule)
        // - if should_escalate_due_to_retry_limit: force escalate path (skip scheduler,
        //   escalate_db will internally re-invoke scheduler for the recovery action)
        // - otherwise: normal schedule + escalate path
        let mut force_escalate_only = false;
        if let Some(assignee_agent_id) = issue.assignee_agent_id {
            use super::continuation_retry_summary::{
                load_continuation_retry_summary, should_escalate_due_to_retry_limit,
                should_skip_due_to_backoff, CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS,
                CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS,
            };
            let summary = load_continuation_retry_summary(
                db,
                issue.company_id,
                issue.id,
                assignee_agent_id,
                issue_latest_run
                    .as_ref()
                    .and_then(|r| r.error_code.as_deref()),
                None,
                10,
            )
            .await?;
            if should_skip_due_to_backoff(
                &summary,
                CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS,
                Utc::now(),
            ) {
                result.skipped += 1;
                continue;
            }
            if should_escalate_due_to_retry_limit(
                &summary,
                CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS,
            ) {
                force_escalate_only = true;
            }
        }
        let previous_status = issue.status.clone();
        let db_input = SchedulerDbInput {
            issue_id: issue.id,
            previous_status: Some(issue.status.clone()),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        };
        let scheduler_result = if force_escalate_only {
            // Retry limit exceeded: skip the dedicated scheduler call; escalate_db
            // will internally call ensure_source_scoped_recovery_action_for_issue
            // via should_attempt_source_escalation gate (non-blocked issue path).
            None
        } else {
            match ensure_source_scoped_recovery_action_for_issue(
                db,
                db_input,
                existing_wake,
                wake_template.clone(),
            )
            .await
            {
                Ok(Some(r)) => Some(r),
                Ok(None) => None,
                Err(error) => {
                    result.failed += 1;
                    result.errors.push((issue.id, error.to_string()));
                    continue;
                }
            }
        };
        let cause = scheduler_result.as_ref().map(|r| r.cause);
        // Pre-escalation action_id from scheduler_result; backfilled from
        // escalate_result.recovery_action_id below for force_escalate_only path
        // where the outer scheduler call was skipped.
        let mut action_id: Option<Uuid> = scheduler_result
            .as_ref()
            .map(|r| r.result.persisted.action.id);
        let escalation_comment = execution_review_escalation_comment(&issue, monitor_run.as_ref());
        let escalation = escalate_stranded_assigned_issue_with_comment(
            db,
            EscalateDbInput {
                issue_id: issue.id,
                previous_status,
                recovery_cause_override: None,
                recovery_owner_agent_id: None,
                successful_run_handoff_evidence: None,
                workspace_validation_fingerprint_override: None,
            },
            escalation_comment,
            existing_wake,
            wake_template.clone(),
        )
        .await;
        match escalation {
            Ok(Some(escalate_result)) => {
                result.dispatched += 1;
                if action_id.is_none() {
                    action_id = escalate_result.recovery_action_id;
                }
                result.outcomes.push(ReconcileAndEscalateOutcome {
                    issue_id: issue.id,
                    cause,
                    recovery_action_id: action_id,
                    escalate_outcome: escalate_result.outcome,
                });
            }
            Ok(None) => {
                result.skipped += 1;
            }
            Err(error) => {
                result.failed += 1;
                result.errors.push((issue.id, error.to_string()));
            }
        }
    }
    Ok(result)
}

fn execution_review_escalation_comment(
    issue: &pc_repos::issue::IssueRow,
    latest_run: Option<&LatestRunRow>,
) -> Option<String> {
    if issue.status != "in_review" {
        return None;
    }
    let latest_run = latest_run?;
    let retry_reason = latest_run
        .context_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|context| context.get("retryReason"))
        .and_then(Value::as_str);
    let failed_automatic_recovery = retry_reason == Some("execution_review_participant_recovery")
        && matches!(
            latest_run.status.as_str(),
            "interrupted" | "failed" | "cancelled" | "timed_out"
        );
    if !failed_automatic_recovery {
        return None;
    }

    Some(
        super::build_execution_review_participant_recovery_comment::build_execution_review_participant_recovery_comment(
            &super::build_recovery_issue_in_place_escalation_comment::EscalationRunView {
                id: latest_run.id,
                agent_id: latest_run.agent_id,
                status: latest_run.status.clone(),
                error: latest_run.error.clone(),
                error_code: latest_run.error_code.clone(),
                context_snapshot: latest_run.context_snapshot.clone(),
            },
        ),
    )
}

/// DB-only 等价：仅写 recovery action，不 dispatch wake。
pub async fn persist_source_scoped_recovery_action_for_issue(
    db: &Db,
    input: SchedulerDbInput,
) -> sqlx::Result<Option<super::orchestrator::PersistedRecoveryAction>> {
    let Some(issue) = IssueRepo::new(db).get(input.issue_id).await? else {
        return Ok(None);
    };
    let Some(latest_run_row) = load_latest_run_row(db, issue.company_id, input.issue_id).await?
    else {
        return Ok(None);
    };
    let agents = AgentRepo::new(db).list_by_company(issue.company_id).await?;
    let routing = resolve_stranded_recovery_routing_db(
        &issue,
        &latest_run_row,
        &agents,
        input.recovery_owner_agent_id,
    );
    let ctx = SchedulerContext {
        company_id: issue.company_id,
        source_issue_id: issue.id,
        recovery_cause_override: input.recovery_cause_override,
        successful_run_handoff_evidence: input.successful_run_handoff_evidence,
        workspace_validation_fingerprint_override: input.workspace_validation_fingerprint_override,
    };
    let run_input = build_run_input(&latest_run_row);
    let now: DateTime<Utc> = Utc::now();
    let candidate = decide_recovery_scheduler_plan(&ctx, &run_input, &routing, now);
    let upsert = build_upsert_from_candidate(&ctx, &candidate, &issue.assignee_agent_id);
    let persisted = persist_source_scoped_recovery_action(db, &upsert).await?;
    Ok(Some(persisted))
}

/// DB-only 等价：仅按 recovery action dispatch wake。
pub async fn dispatch_wake_for_recovery_action(
    db: &Db,
    action: &pc_repos::issue::IssueRecoveryActionRow,
    existing_wake: Option<&WakeSnapshot>,
    wake_template: NewAgentWakeupRequest,
) -> sqlx::Result<Option<crate::wake_dispatch::WakeDispatchOutcome>> {
    persist_recovery_wake(&AgentRepo::new(db), action, existing_wake, wake_template).await
}

/// 计算给定 cause 的 wake input 是否有效（用于单元测试 / 路由短路）。
pub fn wake_input_for(
    action: &pc_repos::issue::IssueRecoveryActionRow,
) -> Option<crate::wake_dedup::WakeInput> {
    recovery_action_wake_input(action)
}

// ----------------------------------------------------------------------------
// Sweep result types
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ReconcileSweepResult {
    pub dispatched: i64,
    pub skipped: i64,
    pub failed: i64,
    pub outcomes: Vec<ReconcileSweepOutcome>,
    pub errors: Vec<(Uuid, String)>,
}

#[derive(Debug, Clone)]
pub struct ReconcileSweepOutcome {
    pub issue_id: Uuid,
    pub cause: StrandedRecoveryCause,
    pub dispatch_intent: super::orchestrator::RecoveryDispatchIntent,
}

#[derive(Debug, Clone, Default)]
pub struct ReconcileAndEscalateSweepResult {
    pub dispatched: i64,
    pub provider_quota_monitored: i64,
    pub skipped: i64,
    pub failed: i64,
    pub outcomes: Vec<ReconcileAndEscalateOutcome>,
    pub errors: Vec<(Uuid, String)>,
}

#[derive(Debug, Clone)]
pub struct ReconcileAndEscalateOutcome {
    pub issue_id: Uuid,
    pub cause: Option<StrandedRecoveryCause>,
    pub recovery_action_id: Option<Uuid>,
    pub escalate_outcome: super::escalate_db::EscalateOutcome,
}

// ----------------------------------------------------------------------------
// Internal helpers
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct LatestRunRow {
    id: Uuid,
    agent_id: Option<Uuid>,
    status: String,
    error_code: Option<String>,
    error: Option<String>,
    context_snapshot: Option<Value>,
    result_json: Option<Value>,
    liveness_state: Option<String>,
}

pub(crate) async fn load_latest_run_row(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Option<LatestRunRow>> {
    let row = sqlx::query(
        "SELECT id, agent_id, status, error_code, error, context_snapshot, result_json, liveness_state \
         FROM heartbeat_runs \
         WHERE company_id = $1 AND context_snapshot ->> 'issueId' = $2::text \
         ORDER BY started_at DESC NULLS LAST, created_at DESC, id DESC \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_optional(db.pool())
    .await?;

    let Some(row) = row else { return Ok(None) };
    Ok(Some(LatestRunRow {
        id: row.try_get("id")?,
        agent_id: row.try_get("agent_id")?,
        status: row.try_get("status")?,
        error_code: row.try_get("error_code")?,
        error: row.try_get("error")?,
        context_snapshot: row.try_get("context_snapshot")?,
        result_json: row.try_get("result_json")?,
        liveness_state: row.try_get("liveness_state")?,
    }))
}

async fn load_latest_run_row_for_agent(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    agent_id: Uuid,
) -> sqlx::Result<Option<LatestRunRow>> {
    let row = sqlx::query(
        "SELECT id, agent_id, status, error_code, error, context_snapshot, result_json, liveness_state \
         FROM heartbeat_runs \
         WHERE company_id = $1 AND agent_id = $2 \
           AND context_snapshot ->> 'issueId' = $3::text \
         ORDER BY started_at DESC NULLS LAST, created_at DESC, id DESC \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(agent_id)
    .bind(issue_id.to_string())
    .fetch_optional(db.pool())
    .await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(LatestRunRow {
        id: row.try_get("id")?,
        agent_id: row.try_get("agent_id")?,
        status: row.try_get("status")?,
        error_code: row.try_get("error_code")?,
        error: row.try_get("error")?,
        context_snapshot: row.try_get("context_snapshot")?,
        result_json: row.try_get("result_json")?,
        liveness_state: row.try_get("liveness_state")?,
    }))
}

fn current_review_participant_agent_id(issue: &pc_repos::issue::IssueRow) -> Option<Uuid> {
    let state = issue.execution_state.as_ref()?.as_object()?;
    if state.get("status").and_then(Value::as_str) != Some("pending") {
        return None;
    }
    let participant = state.get("currentParticipant")?.as_object()?;
    if participant.get("type").and_then(Value::as_str) != Some("agent") {
        return None;
    }
    participant
        .get("agentId")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn build_run_input<'a>(row: &'a LatestRunRow) -> SchedulerRunInput<'a> {
    SchedulerRunInput {
        run_id: Some(row.id),
        agent_id: row.agent_id,
        status: Some(row.status.as_str()),
        error_code: row.error_code.as_deref(),
        error: row.error.as_deref(),
        context_snapshot: row.context_snapshot.as_ref(),
        result_json: row.result_json.as_ref(),
        liveness_state: row.liveness_state.as_deref(),
        started_at: None,
        created_at: None,
    }
}

fn resolve_stranded_recovery_routing_db(
    issue: &pc_repos::issue::IssueRow,
    latest_run: &LatestRunRow,
    agents: &[pc_repos::agent::AgentRow],
    preferred_owner_agent_id: Option<Uuid>,
) -> SchedulerRoutingHints {
    let original_agent_id = latest_run.agent_id.or(issue.assignee_agent_id);
    let return_owner_agent_id = issue.assignee_agent_id.or(original_agent_id);
    let cause = decide_recovery_cause(&build_run_input(latest_run));
    let route_to_original = matches!(
        cause,
        StrandedRecoveryCause::ProcessLost
            | StrandedRecoveryCause::SuccessfulRunMissingState
            | StrandedRecoveryCause::CodexOutputInactivityMonitor
    );
    if cause == StrandedRecoveryCause::ProviderQuota {
        let owner_agent_id = resolve_invokable_recovery_agent(agents, original_agent_id);
        if owner_agent_id.is_none() {
            let fallback = resolve_manager_ladder_owner(
                agents,
                preferred_owner_agent_id,
                issue.assignee_agent_id,
            );
            return SchedulerRoutingHints {
                owner_agent_id: fallback,
                return_owner_agent_id: original_agent_id,
                previous_owner_agent_id: issue.assignee_agent_id,
                routing_fallback_reason: Some(
                    "The original assignee is not invokable; quota recovery fell through to the manager ladder.".to_owned(),
                ),
            };
        }
        return SchedulerRoutingHints {
            owner_agent_id: None,
            return_owner_agent_id: original_agent_id,
            previous_owner_agent_id: issue.assignee_agent_id,
            routing_fallback_reason: None,
        };
    }
    if route_to_original {
        let owner_agent_id = resolve_invokable_recovery_agent(agents, original_agent_id);
        if let Some(owner) = owner_agent_id {
            return SchedulerRoutingHints {
                owner_agent_id: Some(owner),
                return_owner_agent_id: original_agent_id,
                previous_owner_agent_id: issue.assignee_agent_id,
                routing_fallback_reason: None,
            };
        }
        let fallback =
            resolve_manager_ladder_owner(agents, preferred_owner_agent_id, issue.assignee_agent_id);
        return SchedulerRoutingHints {
            owner_agent_id: fallback,
            return_owner_agent_id: original_agent_id,
            previous_owner_agent_id: issue.assignee_agent_id,
            routing_fallback_reason: Some(
                "The original assignee is not invokable; recovery fell through to the manager ladder.".to_owned(),
            ),
        };
    }
    let owner_agent_id =
        resolve_manager_ladder_owner(agents, preferred_owner_agent_id, issue.assignee_agent_id);
    SchedulerRoutingHints {
        owner_agent_id,
        return_owner_agent_id,
        previous_owner_agent_id: issue.assignee_agent_id,
        routing_fallback_reason: None,
    }
}

fn resolve_invokable_recovery_agent(
    agents: &[pc_repos::agent::AgentRow],
    candidate_id: Option<Uuid>,
) -> Option<Uuid> {
    let id = candidate_id?;
    let agent = agents.iter().find(|a| a.id == id)?;
    if is_agent_status_invokable(&agent.status) {
        Some(id)
    } else {
        None
    }
}

fn resolve_manager_ladder_owner(
    agents: &[pc_repos::agent::AgentRow],
    preferred_owner_agent_id: Option<Uuid>,
    fallback_chain_id: Option<Uuid>,
) -> Option<Uuid> {
    let mut candidate_ids: Vec<Uuid> = Vec::new();
    if let Some(id) = preferred_owner_agent_id {
        candidate_ids.push(id);
    }
    if let Some(id) = fallback_chain_id {
        candidate_ids.push(id);
    }
    let cto_ceo: Vec<Uuid> = agents
        .iter()
        .filter(|a| a.role == "cto" || a.role == "ceo")
        .map(|a| a.id)
        .collect();
    candidate_ids.extend(cto_ceo);
    if let Some(id) = fallback_chain_id {
        candidate_ids.push(id);
    }
    let mut seen = std::collections::HashSet::new();
    for id in candidate_ids {
        if !seen.insert(id) {
            continue;
        }
        if let Some(agent) = agents.iter().find(|a| a.id == id) {
            if is_agent_status_invokable(&agent.status) {
                return Some(id);
            }
        }
    }
    None
}

fn build_upsert_from_candidate(
    ctx: &SchedulerContext,
    candidate: &super::scheduler::SchedulerCandidate,
    previous_owner_agent_id: &Option<Uuid>,
) -> UpsertRecoveryAction {
    super::source_scoped_recovery_action::plan_to_upsert_recovery_action(
        &candidate.plan,
        ctx.company_id,
        ctx.source_issue_id,
        None,
        candidate.routing.owner_agent_id,
        *previous_owner_agent_id,
        candidate.routing.return_owner_agent_id,
        candidate.evidence.clone(),
    )
}

// ----------------------------------------------------------------------------
// Display helpers (for diagnostics / tests)
// ----------------------------------------------------------------------------

pub fn dispatch_intent_label(intent: &RecoveryDispatchIntent) -> &'static str {
    match intent {
        RecoveryDispatchIntent::WakeOwner { .. } => "wake_owner",
        RecoveryDispatchIntent::MonitorOnly => "monitor_only",
        RecoveryDispatchIntent::ManualRepair => "manual_repair_required",
        RecoveryDispatchIntent::BoardEscalation => "board_escalation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_intent_labels_align_with_node_literals() {
        assert_eq!(
            dispatch_intent_label(&RecoveryDispatchIntent::WakeOwner {
                agent_id: Uuid::nil()
            }),
            "wake_owner"
        );
        assert_eq!(
            dispatch_intent_label(&RecoveryDispatchIntent::MonitorOnly),
            "monitor_only"
        );
        assert_eq!(
            dispatch_intent_label(&RecoveryDispatchIntent::ManualRepair),
            "manual_repair_required"
        );
        assert_eq!(
            dispatch_intent_label(&RecoveryDispatchIntent::BoardEscalation),
            "board_escalation"
        );
    }

    #[test]
    fn retry_reason_extraction_matches_scheduler_helper() {
        let snapshot = serde_json::json!({ "retryReason": "issue_continuation_needed" });
        assert_eq!(
            read_context_retry_reason(Some(&snapshot)).as_deref(),
            Some("issue_continuation_needed")
        );
    }
}
