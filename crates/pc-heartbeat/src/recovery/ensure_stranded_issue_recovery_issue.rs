//! `ensureStrandedIssueRecoveryIssue` —— Node `services/recovery/service.ts:2712`。
//!
//! 顶层 orchestrator：当 stranded assigned issue 没有 invokable 执行路径时，
//! 自动创建一个"recovery issue"指派给可用的 manager/creator/executive owner。
//!
//! 业务语义（与 Node 一致）：
//! 1. 若 input.issue 本身已是 `stranded_issue_recovery` → return None
//! 2. 若已有 open stranded_issue_recovery（同 source）→ return existing
//! 3. 若无 invokable owner（resolve_stranded_issue_recovery_owner_agent_id = None）→ return None
//! 4. 否则：
//!    - 用 build_stranded_issue_recovery_description 生成 description
//!    - INSERT 一条新 issue（origin_kind="stranded_issue_recovery", parent_id=source.id）
//!    - 若发生 23505 unique conflict（race）→ 重查 + 返回 raced recovery
//!    - INSERT 一条 agent_wakeup_request（issue_assigned）
//!    - 返回新创建的 issue
//!
//! 设计原则：
//! - 编排入口：调用前面 6 个模块的 helper（highest cohesion, low coupling）
//! - 单事务：先 INSERT issue → INSERT wake（wake 用独立连接，匹配 Node 行为）
//! - title 模式：stranded_assigned_issue → "Recover stalled issue ..."，
//!   successful_run_missing_state → "Recover missing next step ..."
//! - fingerprint: `"stranded_issue_recovery:<company_id>:<source_id>:<cause>:<run_id>"`

use crate::recovery::build_stranded_issue_recovery_description::{
    build_stranded_issue_recovery_description, AgentShortView,
    BuildStrandedIssueRecoveryDescriptionInput, LatestRunView,
};
use crate::recovery::model_profile_hint::{
    recovery_assignee_adapter_overrides, with_recovery_model_profile_hint,
    RecoveryModelProfileWorkClass,
};
use crate::recovery::resolve_recovery_owner_agent::{
    fetch_agent_org_row, resolve_stranded_issue_recovery_owner_agent_id,
};
use crate::recovery::source_scoped_recovery_action::StrandedRecoveryCause;
use crate::recovery::stranded_issue_recovery_queries::{
    find_open_stranded_issue_recovery_issue, is_stranded_issue_recovery_issue,
    is_unique_stranded_issue_recovery_conflict, STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
};
use pc_repos::agent::{
    AgentRepo, HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType,
    WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::company::CompanyRepo;
use pc_repos::issue::IssueRow;
use pc_repos::Db;
use serde_json::{json, Value};
use uuid::Uuid;

/// `ensure_stranded_issue_recovery_issue` 的输入。
#[derive(Debug, Clone)]
pub struct EnsureStrandedIssueRecoveryInput<'a> {
    pub issue: &'a IssueRow,
    pub latest_run: Option<&'a LatestRunView>,
    pub previous_status: &'a str,
    pub recovery_cause: Option<StrandedRecoveryCause>,
    pub successful_run_handoff_evidence: Option<&'a Value>,
}

/// 读 company 的 issue_prefix；缺省 "PAP"。
async fn get_company_issue_prefix(db: &Db, company_id: Uuid) -> sqlx::Result<String> {
    let company = CompanyRepo::new(db).get(company_id).await?;
    Ok(company
        .map(|c| c.issue_prefix)
        .unwrap_or_else(|| "PAP".to_string()))
}

/// 构造 recovery issue 的 title。
fn build_recovery_title(
    cause: StrandedRecoveryCause,
    source_identifier: Option<&str>,
    source_title: &str,
) -> String {
    let label = source_identifier.unwrap_or(source_title);
    if matches!(cause, StrandedRecoveryCause::SuccessfulRunMissingState) {
        format!("Recover missing next step {label}")
    } else {
        format!("Recover stalled issue {label}")
    }
}

/// 构造 origin_fingerprint（与 Node `"<origin_kind>:<company_id>:<source_id>:<cause>:<run_id|no-run>"` 对齐）。
fn build_origin_fingerprint(
    source_company_id: Uuid,
    source_issue_id: Uuid,
    cause: StrandedRecoveryCause,
    latest_run_id: Option<Uuid>,
) -> String {
    [
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        &source_company_id.to_string(),
        &source_issue_id.to_string(),
        cause.as_str(),
        &latest_run_id
            .map(|u| u.to_string())
            .unwrap_or_else(|| "no-run".to_string()),
    ]
    .join(":")
}

/// 在事务内 INSERT stranded_issue_recovery 记录。
///
/// 返回新创建的 IssueRow；若发生 unique constraint conflict → 返回 Err。
async fn insert_stranded_recovery_issue(
    db: &Db,
    source: &IssueRow,
    owner_agent_id: Uuid,
    description: &str,
    title: &str,
    recovery_cause: StrandedRecoveryCause,
    latest_run_id: Option<Uuid>,
) -> sqlx::Result<IssueRow> {
    let status = "todo";
    let work_mode = "standard";
    let priority = if source.priority.is_empty() {
        "medium".to_string()
    } else {
        source.priority.clone()
    };
    let adapter_overrides =
        recovery_assignee_adapter_overrides(RecoveryModelProfileWorkClass::StatusOnly);
    let execution_policy = json!({"mode": "normal", "commentRequired": false, "stages": []});
    let origin_fingerprint =
        build_origin_fingerprint(source.company_id, source.id, recovery_cause, latest_run_id);
    let origin_run_id_str: Option<String> = latest_run_id.map(|u| u.to_string());
    let billing_code: Option<String> = source.billing_code.clone();
    let request_depth = source.request_depth + 1;
    let responsible_user_id: Option<String> = source.responsible_user_id.clone();

    sqlx::query_as::<_, IssueRow>(
        "INSERT INTO issues \
         (company_id, parent_id, project_id, project_workspace_id, goal_id, \
          title, description, status, work_mode, priority, \
          assignee_agent_id, responsible_user_id, request_depth, billing_code, \
          assignee_adapter_overrides, execution_policy, \
          origin_kind, origin_id, origin_run_id, origin_fingerprint) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20) \
         RETURNING id, company_id, project_id, project_workspace_id, goal_id, parent_id, title, description, status, work_mode, harness_kind, priority, assignee_agent_id, assignee_user_id, checkout_run_id, execution_run_id, execution_agent_name_key, execution_locked_at, created_by_agent_id, created_by_user_id, responsible_user_id, issue_number, identifier, origin_kind, origin_id, origin_run_id, origin_fingerprint, request_depth, billing_code, assignee_adapter_overrides, execution_policy, execution_state, monitor_next_check_at, monitor_wake_requested_at, monitor_last_triggered_at, monitor_attempt_count, monitor_notes, monitor_scheduled_by, execution_workspace_id, execution_workspace_preference, execution_workspace_settings, source_trust, unblock_descriptor, blocked_transition_at, blocked_owner_notified_at, started_at, completed_at, cancelled_at, hidden_at, created_at, updated_at",
    )
    .bind(source.company_id)
    .bind(source.id)
    .bind(source.project_id)
    .bind(source.project_workspace_id)
    .bind(source.goal_id)
    .bind(title)
    .bind(description)
    .bind(status)
    .bind(work_mode)
    .bind(priority)
    .bind(owner_agent_id)
    .bind(responsible_user_id)
    .bind(request_depth)
    .bind(billing_code)
    .bind(serde_json::json!({"modelProfile": adapter_overrides.model_profile}))
    .bind(execution_policy)
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(source.id.to_string())
    .bind(origin_run_id_str)
    .bind(origin_fingerprint)
    .fetch_one(db.pool())
    .await
}

/// enqueue recovery wake（独立连接，不在 tx 中）。
async fn enqueue_recovery_wake(
    db: &Db,
    owner_agent_id: Uuid,
    recovery: &IssueRow,
    source_issue_id: Uuid,
    latest_run_id: Option<Uuid>,
    recovery_cause: StrandedRecoveryCause,
) -> sqlx::Result<()> {
    let payload_map = with_recovery_model_profile_hint(
        json!({
            "issueId": recovery.id,
            "sourceIssueId": source_issue_id,
            "strandedRunId": latest_run_id.map(|u| u.to_string()),
            "recoveryCause": recovery_cause.as_str(),
        })
        .as_object()
        .expect("json!() of an object always yields an object"),
        RecoveryModelProfileWorkClass::StatusOnly,
    );
    let payload = Value::Object(payload_map);

    AgentRepo::new(db)
        .create_wakeup_request(NewAgentWakeupRequest {
            company_id: recovery.company_id,
            agent_id: owner_agent_id,
            source: HeartbeatInvocationSource::Assignment,
            trigger_detail: Some(WakeupTriggerDetail::System),
            reason: Some("issue_assigned".to_string()),
            payload: Some(payload),
            status: WakeupRequestStatus::Queued,
            coalesced_count: 0,
            requested_by_actor_type: Some(WakeupActorType::System),
            requested_by_actor_id: None,
            idempotency_key: None,
            run_id: None,
            error: None,
        })
        .await?;
    Ok(())
}

/// 主入口：确保 stranded issue recovery issue 存在。
///
/// 返回 Some(IssueRow) 当 issue 已存在或新建成功；返回 None 当短路条件触发。
pub async fn ensure_stranded_issue_recovery_issue(
    db: &Db,
    input: EnsureStrandedIssueRecoveryInput<'_>,
) -> sqlx::Result<Option<IssueRow>> {
    // 1. 短路：input.issue 本身已是 recovery issue
    if is_stranded_issue_recovery_issue(input.issue) {
        return Ok(None);
    }

    // 2. 短路：已有 open stranded_issue_recovery
    if let Some(existing) =
        find_open_stranded_issue_recovery_issue(db, input.issue.company_id, input.issue.id).await?
    {
        return Ok(Some(existing));
    }

    // 3. 解析 owner agent
    let Some(owner_agent_id) =
        resolve_stranded_issue_recovery_owner_agent_id(db, input.issue, None).await?
    else {
        return Ok(None);
    };

    // 4. 准备 description 输入
    let prefix = get_company_issue_prefix(db, input.issue.company_id).await?;
    let source_assignee = match input.issue.assignee_agent_id {
        Some(assignee_id) => fetch_agent_org_row(db, assignee_id)
            .await?
            .map(|a| AgentShortView {
                id: a.id,
                name: a.name,
            }),
        None => None,
    };
    let recovery_cause = input
        .recovery_cause
        .unwrap_or(StrandedRecoveryCause::RuntimeFailure);
    let latest_run_id = input.latest_run.map(|r| r.id);

    let description =
        build_stranded_issue_recovery_description(&BuildStrandedIssueRecoveryDescriptionInput {
            issue: input.issue,
            latest_run: input.latest_run,
            previous_status: input.previous_status,
            prefix: &prefix,
            recovery_cause: Some(recovery_cause),
            successful_run_handoff_evidence: input.successful_run_handoff_evidence,
            source_assignee: source_assignee.as_ref(),
        workspace_validation_fingerprint: None,
        });

    // 5. INSERT issue（处理 unique conflict race）
    let title = build_recovery_title(
        recovery_cause,
        input.issue.identifier.as_deref(),
        &input.issue.title,
    );
    let recovery = match insert_stranded_recovery_issue(
        db,
        input.issue,
        owner_agent_id,
        &description,
        &title,
        recovery_cause,
        latest_run_id,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            if !is_unique_stranded_issue_recovery_conflict(&e) {
                return Err(e);
            }
            // Race：另一个并发调用已创建
            return Ok(find_open_stranded_issue_recovery_issue(
                db,
                input.issue.company_id,
                input.issue.id,
            )
            .await?);
        }
    };

    // 6. enqueue wake
    enqueue_recovery_wake(
        db,
        owner_agent_id,
        &recovery,
        input.issue.id,
        latest_run_id,
        recovery_cause,
    )
    .await?;

    Ok(Some(recovery))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_for_successful_run_missing_state_uses_missing_next_step() {
        let title = build_recovery_title(
            StrandedRecoveryCause::SuccessfulRunMissingState,
            Some("PAP-1"),
            "source title",
        );
        assert!(title.starts_with("Recover missing next step "));
        assert!(title.contains("PAP-1"));
    }

    #[test]
    fn title_for_stranded_uses_stalled_issue() {
        let title = build_recovery_title(
            StrandedRecoveryCause::RuntimeFailure,
            Some("PAP-2"),
            "source title",
        );
        assert!(title.starts_with("Recover stalled issue "));
        assert!(title.contains("PAP-2"));
    }

    #[test]
    fn title_falls_back_to_source_title_when_no_identifier() {
        let title =
            build_recovery_title(StrandedRecoveryCause::RuntimeFailure, None, "source title");
        assert!(title.contains("source title"));
    }

    #[test]
    fn fingerprint_contains_company_source_cause_run() {
        let company = Uuid::from_bytes([1; 16]);
        let source = Uuid::from_bytes([2; 16]);
        let run = Uuid::from_bytes([3; 16]);
        let fp = build_origin_fingerprint(
            company,
            source,
            StrandedRecoveryCause::RuntimeFailure,
            Some(run),
        );
        assert!(fp.starts_with("stranded_issue_recovery:"));
        assert!(fp.contains(&company.to_string()));
        assert!(fp.contains(&source.to_string()));
        assert!(fp.contains("runtime_failure"));
        assert!(fp.ends_with(&run.to_string()));
    }

    #[test]
    fn fingerprint_uses_no_run_when_run_missing() {
        let fp = build_origin_fingerprint(
            Uuid::from_bytes([1; 16]),
            Uuid::from_bytes([2; 16]),
            StrandedRecoveryCause::RuntimeFailure,
            None,
        );
        assert!(fp.ends_with("no-run"));
    }
}
