//! Recovery 升级决策的 DB 接入层。
//!
//! 对齐 Node `services/recovery/service.ts` 的：
//! - `escalateStrandedAssignedIssue`（含其内部对 `ensureSourceScopedStrandedRecoveryAction` 的调用）
//! - `escalateStrandedRecoveryIssueInPlace`
//!
//! 边界：
//! - 调用纯计划层 `decide_escalation` 决定升级路径
//! - 若选择 SourceEscalate：调 scheduler 写 recovery_action + 更新 issue 为 blocked + 写 comment
//! - 若选择 RecoveryInPlace：直接更新 issue 为 blocked + 写 comment
//! - 全部副作用在 DB 上

use serde_json::Value;
use uuid::Uuid;

use pc_repos::agent::NewAgentWakeupRequest;
use pc_repos::issue::{IssueRepo, IssueRow};
use pc_repos::Db;

use super::build_recovery_issue_in_place_escalation_comment::{
    build_recovery_issue_in_place_escalation_comment,
    BuildRecoveryIssueInPlaceEscalationCommentInput,
};
use super::escalate::{
    decide_escalation, should_attempt_source_escalation, EscalationDecision, IssueSnapshot,
    RecoveryInPlacePlan, SourceEscalationPlan,
};
use super::get_company_issue_prefix::get_company_issue_prefix;
use super::load_latest_heartbeat_run_for_issue::load_latest_heartbeat_run_for_issue;
use super::scheduler_db::{ensure_source_scoped_recovery_action_for_issue, SchedulerDbInput};
use super::source_scoped_recovery_action::StrandedRecoveryCause;
use crate::wake_dedup::WakeSnapshot;

/// DB 升级入口输入。
#[derive(Debug, Clone)]
pub struct EscalateDbInput {
    pub issue_id: Uuid,
    pub previous_status: String,
    pub recovery_cause_override: Option<StrandedRecoveryCause>,
    pub recovery_owner_agent_id: Option<Uuid>,
    pub successful_run_handoff_evidence: Option<Value>,
    pub workspace_validation_fingerprint_override: Option<String>,
}

/// DB 升级入口输出。
#[derive(Debug, Clone)]
pub struct EscalateDbResult {
    pub outcome: EscalateOutcome,
    pub updated_issue: IssueRow,
    pub comment_id: Option<Uuid>,
    pub recovery_action_id: Option<Uuid>,
}

/// DB 升级对外结果枚举。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalateOutcome {
    SourceEscalated,
    RecoveryInPlace,
    Skipped,
    Missing,
}

/// 主入口：升级 stranded issue。
///
/// 完整流程（与 Node 对齐）：
/// 1. 读取 issue 行
/// 2. 决定升级路径（pure：SourceEscalate / RecoveryInPlace / Skip）
/// 3. 若 SourceEscalate：调 scheduler 写 recovery action
/// 4. 更新 issue 为 blocked（必要时切换 assignee_agent_id 到 recovery owner）
/// 5. 写 escalation comment（带 dedup：如果 comment body 已含 marker 则跳过）
///
/// `existing_wake_snapshot` 和 `wake_template` 仅在 SourceEscalate 需要 dispatch wake 时使用。
pub async fn escalate_stranded_assigned_issue(
    db: &Db,
    input: EscalateDbInput,
    existing_wake: Option<&WakeSnapshot>,
    wake_template: NewAgentWakeupRequest,
) -> sqlx::Result<Option<EscalateDbResult>> {
    escalate_stranded_assigned_issue_with_comment(db, input, None, existing_wake, wake_template)
        .await
}

/// Node `escalateStrandedAssignedIssue(input.comment)` 对应入口。
///
/// `comment` 仅覆盖 source escalation 的业务说明前缀；recovery action、owner 与 next action
/// 仍由统一计划层追加，确保 marker 幂等语义不被调用方破坏。
pub async fn escalate_stranded_assigned_issue_with_comment(
    db: &Db,
    input: EscalateDbInput,
    comment: Option<String>,
    existing_wake: Option<&WakeSnapshot>,
    wake_template: NewAgentWakeupRequest,
) -> sqlx::Result<Option<EscalateDbResult>> {
    let Some(issue) = IssueRepo::new(db).get(input.issue_id).await? else {
        return Ok(None);
    };
    let snapshot = IssueSnapshot {
        id: issue.id,
        company_id: issue.company_id,
        origin_kind: Some(issue.origin_kind.clone()),
        origin_id: None,
        status: issue.status.clone(),
        assignee_agent_id: issue.assignee_agent_id,
    };
    // 用专用判定函数确认是否值得调用 scheduler
    let needs_scheduler = should_attempt_source_escalation(&snapshot);
    let scheduler_result = if needs_scheduler {
        ensure_source_scoped_recovery_action_for_issue(
            db,
            SchedulerDbInput {
                issue_id: input.issue_id,
                previous_status: Some(input.previous_status.clone()),
                recovery_cause_override: input.recovery_cause_override,
                recovery_owner_agent_id: input.recovery_owner_agent_id,
                successful_run_handoff_evidence: input.successful_run_handoff_evidence.clone(),
                workspace_validation_fingerprint_override: input
                    .workspace_validation_fingerprint_override
                    .clone(),
            },
            existing_wake,
            wake_template,
        )
        .await?
    } else {
        None
    };
    let (candidate_owned, action_id) = if let Some(ref result) = scheduler_result {
        let cand = candidate_from_persisted(&result.result.persisted);
        let action_id = result.result.persisted.action.id;
        (Some(cand), Some(action_id))
    } else {
        (None, None)
    };
    let decision = if needs_scheduler {
        decide_escalation(
            &snapshot,
            candidate_owned.as_ref(),
            action_id,
            &input.previous_status,
        )
    } else {
        // Without scheduler call, re-run decide_escalation directly to get RecoveryInPlace / Skip
        decide_escalation(&snapshot, None, None, &input.previous_status)
    };
    match decision {
        EscalationDecision::Skip(_) => Ok(Some(EscalateDbResult {
            outcome: EscalateOutcome::Skipped,
            updated_issue: issue,
            comment_id: None,
            recovery_action_id: action_id,
        })),
        EscalationDecision::RecoveryInPlace(plan) => {
            let (updated, comment_id) = apply_in_place_escalation(db, &issue, &plan).await?;
            Ok(Some(EscalateDbResult {
                outcome: EscalateOutcome::RecoveryInPlace,
                updated_issue: updated,
                comment_id,
                recovery_action_id: None,
            }))
        }
        EscalationDecision::SourceEscalate(plan) => {
            let plan = apply_comment_override(plan, comment.as_deref());
            let (updated, comment_id) = apply_source_escalation(db, &issue, &plan).await?;
            Ok(Some(EscalateDbResult {
                outcome: EscalateOutcome::SourceEscalated,
                updated_issue: updated,
                comment_id,
                recovery_action_id: Some(plan.recovery_action_id),
            }))
        }
    }
}

fn apply_comment_override(
    mut plan: SourceEscalationPlan,
    comment: Option<&str>,
) -> SourceEscalationPlan {
    let Some(comment) = comment else {
        return plan;
    };
    let owner = plan
        .owner_agent_id
        .map(|id| format!("agent `{id}`"))
        .unwrap_or_else(|| {
            "board escalation, because Paperclip could not find an invokable recovery owner"
                .to_owned()
        });
    plan.comment_body = format!(
        "{comment}\n\n- Recovery action: `{}`\n- Recovery owner: {owner}\n- Next action: the recovery owner should either restore a live execution path or record the manual resolution on the source issue.",
        plan.recovery_action_id,
    );
    plan
}

/// 仅做 in-place 升级的便利入口（对齐 Node `escalateStrandedRecoveryIssueInPlace`）。
///
/// 完整流程（与 Node 对齐）：
/// 1. 读取 issue 行
/// 2. 决定升级路径（pure：RecoveryInPlace / Skip）
/// 3. 若 RecoveryInPlace：
///    - 加载 latest heartbeat_run（任意状态），缺失则 None
///    - 取 company.issue_prefix（缺失 fallback "PAP"）
///    - 用 `build_recovery_issue_in_place_escalation_comment` 生成完整 markdown body
///    - 写 system comment + 切 issue 为 blocked
pub async fn escalate_stranded_recovery_issue_in_place(
    db: &Db,
    issue_id: Uuid,
    previous_status: String,
) -> sqlx::Result<Option<EscalateDbResult>> {
    let Some(issue) = IssueRepo::new(db).get(issue_id).await? else {
        return Ok(None);
    };
    let snapshot = IssueSnapshot {
        id: issue.id,
        company_id: issue.company_id,
        origin_kind: Some(issue.origin_kind.clone()),
        origin_id: None,
        status: issue.status.clone(),
        assignee_agent_id: issue.assignee_agent_id,
    };
    let decision = decide_escalation(&snapshot, None, None, &previous_status);
    match decision {
        EscalationDecision::RecoveryInPlace(plan) => {
            // Round 329: 加载 latest run + company prefix，生成完整 markdown body
            let latest_run = load_latest_heartbeat_run_for_issue(db, issue_id).await?;
            let prefix = get_company_issue_prefix(db, issue.company_id).await?;
            let enriched_body = build_recovery_issue_in_place_escalation_comment(
                &BuildRecoveryIssueInPlaceEscalationCommentInput {
                    issue_identifier: issue.identifier.clone(),
                    issue_id: issue.id,
                    previous_status: previous_status.clone(),
                    latest_run,
                    prefix,
                },
            );
            let enriched_plan = RecoveryInPlacePlan {
                issue_id: plan.issue_id,
                company_id: plan.company_id,
                previous_status: plan.previous_status.clone(),
                comment_body: enriched_body,
                activity_source: plan.activity_source.clone(),
            };
            let (updated, comment_id) =
                apply_in_place_escalation(db, &issue, &enriched_plan).await?;
            Ok(Some(EscalateDbResult {
                outcome: EscalateOutcome::RecoveryInPlace,
                updated_issue: updated,
                comment_id,
                recovery_action_id: None,
            }))
        }
        _ => Ok(Some(EscalateDbResult {
            outcome: EscalateOutcome::Skipped,
            updated_issue: issue,
            comment_id: None,
            recovery_action_id: None,
        })),
    }
}

// ----------------------------------------------------------------------------
// Internal helpers
// ----------------------------------------------------------------------------

async fn apply_source_escalation(
    db: &Db,
    issue: &IssueRow,
    plan: &SourceEscalationPlan,
) -> sqlx::Result<(IssueRow, Option<Uuid>)> {
    let repo = IssueRepo::new(db);
    let updated = repo
        .update(
            issue.id,
            None,
            None,
            Some("blocked"),
            None,
            Some(plan.next_assignee_agent_id),
        )
        .await?
        .unwrap_or_else(|| issue.clone());
    let mut comment_id = None;
    if plan.should_post_comment {
        let dedup_hit =
            comment_already_references_marker(db, issue.id, &plan.comment_marker).await?;
        if !dedup_hit {
            let row = repo
                .create_comment(
                    plan.company_id,
                    issue.id,
                    None,
                    Some("system"),
                    &plan.comment_body,
                )
                .await?;
            comment_id = Some(row.id);
        }
    }
    Ok((updated, comment_id))
}

async fn apply_in_place_escalation(
    db: &Db,
    issue: &IssueRow,
    plan: &RecoveryInPlacePlan,
) -> sqlx::Result<(IssueRow, Option<Uuid>)> {
    let repo = IssueRepo::new(db);
    let updated = repo
        .update(issue.id, None, None, Some("blocked"), None, Some(None))
        .await?
        .unwrap_or_else(|| issue.clone());
    let row = repo
        .create_comment(
            plan.company_id,
            issue.id,
            None,
            Some("system"),
            &plan.comment_body,
        )
        .await?;
    Ok((updated, Some(row.id)))
}

async fn comment_already_references_marker(
    db: &Db,
    issue_id: Uuid,
    marker: &str,
) -> sqlx::Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments              WHERE issue_id=$1 AND deleted_at IS NULL              AND body LIKE '%' || $2 || '%'",
    )
    .bind(issue_id)
    .bind(marker)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|(c,)| c > 0).unwrap_or(false))
}

/// Rebuild SchedulerCandidate from a persisted recovery action row.
///
/// This is needed because `PersistedRecoveryAction` only stores the action row,
/// not the original SchedulerCandidate. For escalate dedup we only need the
/// cause / owner_agent_id / return_owner_agent_id fields.
pub(crate) fn candidate_from_persisted(
    persisted: &super::orchestrator::PersistedRecoveryAction,
) -> super::scheduler::SchedulerCandidate {
    let cause = match persisted.action.cause.as_str() {
        "process_lost" => StrandedRecoveryCause::ProcessLost,
        "workspace_validation_failed" => StrandedRecoveryCause::WorkspaceValidationFailed,
        "configuration_incomplete" => StrandedRecoveryCause::ConfigurationIncomplete,
        "provider_quota" => StrandedRecoveryCause::ProviderQuota,
        "codex_output_inactivity_monitor" => StrandedRecoveryCause::CodexOutputInactivityMonitor,
        "execution_review_participant_recovery" => {
            StrandedRecoveryCause::ExecutionReviewParticipantRecovery
        }
        "successful_run_missing_state" => StrandedRecoveryCause::SuccessfulRunMissingState,
        _ => StrandedRecoveryCause::RuntimeFailure,
    };
    super::scheduler::SchedulerCandidate {
        cause,
        plan: super::source_scoped_recovery_action::build_source_scoped_recovery_action_plan(
            persisted.action.company_id,
            persisted.action.source_issue_id,
            cause,
            persisted.action.owner_agent_id,
            persisted.action.return_owner_agent_id,
            None,
        ),
        routing: super::scheduler::SchedulerRoutingHints {
            owner_agent_id: persisted.action.owner_agent_id,
            return_owner_agent_id: persisted.action.return_owner_agent_id,
            previous_owner_agent_id: persisted.action.previous_owner_agent_id,
            routing_fallback_reason: None,
        },
        evidence: persisted.action.evidence.clone(),
        retry_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn candidate_from_persisted_maps_process_lost_cause() {
        let action = pc_repos::issue::IssueRecoveryActionRow {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            source_issue_id: Uuid::nil(),
            recovery_issue_id: None,
            kind: "stranded_assigned_issue".into(),
            status: "active".into(),
            owner_type: "agent".into(),
            owner_agent_id: Some(Uuid::nil()),
            owner_user_id: None,
            previous_owner_agent_id: None,
            return_owner_agent_id: Some(Uuid::nil()),
            cause: "process_lost".into(),
            fingerprint: "fp".into(),
            evidence: Value::Null,
            next_action: "retry".into(),
            wake_policy: None,
            monitor_policy: None,
            attempt_count: 1,
            max_attempts: None,
            timeout_at: None,
            last_attempt_at: None,
            outcome: None,
            resolution_note: None,
            resolved_at: None,
            created_at: pc_core::Timestamp::from_dt(Utc::now()),
            updated_at: pc_core::Timestamp::from_dt(Utc::now()),
        };
        let persisted = super::super::orchestrator::PersistedRecoveryAction {
            action,
            dispatch: super::super::orchestrator::RecoveryDispatchIntent::WakeOwner {
                agent_id: Uuid::nil(),
            },
        };
        let cand = candidate_from_persisted(&persisted);
        assert_eq!(cand.cause, StrandedRecoveryCause::ProcessLost);
        assert_eq!(cand.routing.owner_agent_id, Some(Uuid::nil()));
        assert_eq!(cand.routing.return_owner_agent_id, Some(Uuid::nil()));
    }

    #[test]
    fn candidate_from_persisted_maps_unknown_cause_to_runtime_failure() {
        let mut action = pc_repos::issue::IssueRecoveryActionRow {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            source_issue_id: Uuid::nil(),
            recovery_issue_id: None,
            kind: "stranded_assigned_issue".into(),
            status: "active".into(),
            owner_type: "agent".into(),
            owner_agent_id: Some(Uuid::nil()),
            owner_user_id: None,
            previous_owner_agent_id: None,
            return_owner_agent_id: None,
            cause: "unknown_cause".into(),
            fingerprint: "fp".into(),
            evidence: Value::Null,
            next_action: "retry".into(),
            wake_policy: None,
            monitor_policy: None,
            attempt_count: 1,
            max_attempts: None,
            timeout_at: None,
            last_attempt_at: None,
            outcome: None,
            resolution_note: None,
            resolved_at: None,
            created_at: pc_core::Timestamp::from_dt(Utc::now()),
            updated_at: pc_core::Timestamp::from_dt(Utc::now()),
        };
        // Make sure owner_agent_id is consistent
        action.owner_agent_id = None;
        let persisted = super::super::orchestrator::PersistedRecoveryAction {
            action,
            dispatch: super::super::orchestrator::RecoveryDispatchIntent::BoardEscalation,
        };
        let cand = candidate_from_persisted(&persisted);
        assert_eq!(cand.cause, StrandedRecoveryCause::RuntimeFailure);
    }
}
