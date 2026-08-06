//! Recovery 调度的纯计划层。
//! 对齐 Node `services/recovery/service.ts` 中
//! `resolveStrandedRecoveryCause` / `resolveStrandedRecoveryRouting` /
//! `ensureSourceScopedStrandedRecoveryAction` 的**决策部分**。
//!
//! 边界：
//! - 不进行数据库写入
//! - 不进行 agent invokability 真实查询（由调用层在 DB 上完成）
//! - 不进行 wake dispatch（由 orchestrator 处理）
//! - 纯函数 + 强类型，输入 → 计划候选
//!
//! 调度流程：
//! 1. `decide_recovery_cause` — 根据 latest run 的 error / error_code / resultJson 推 cause
//! 2. `build_routing_for_cause` — 根据 cause + 调用方传入的 routing hints 决定 owner / return owner
//! 3. `decide_recovery_scheduler_plan` — 串联 1+2，调用 `build_source_scoped_recovery_action_plan`
//! 4. 编排层（`orchestrator.rs`）负责写入 DB 与 wake dispatch

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::adapter_failure_classification::{
    classify_adapter_failure, AdapterFailureRecoveryClassification,
};
use super::source_scoped_recovery_action::{
    build_source_scoped_recovery_action_plan, SourceScopedRecoveryActionPlan, StrandedRecoveryCause,
};

/// Constants mirroring Node `service.ts`.
pub const STRANDED_RECOVERY_RUN_ERROR_FAMILY_KEY: &str = "errorFamily";
pub const STRANDED_RECOVERY_PROVIDER_QUOTA_FAMILY: &str = "provider_quota";
pub const STRANDED_RECOVERY_WORKSPACE_VALIDATION_KEY: &str = "workspaceValidation";
pub const STRANDED_RECOVERY_WORKSPACE_VALIDATION_REASON_KEY: &str = "reason";
pub const STRANDED_RECOVERY_WORKSPACE_VALIDATION_FINGERPRINT_KEY: &str = "fingerprint";
pub const STRANDED_RECOVERY_GIT_WORKTREE_INCOHERENCE_REASON: &str =
    "git_worktree_branch_incoherence";
pub const STRANDED_RECOVERY_EXECUTION_REVIEW_PARTICIPANT_REASON: &str =
    "execution_review_participant_recovery";
pub const STRANDED_RECOVERY_SUCCESSFUL_RUN_MISSING_STATE_REASON: &str =
    "successful_run_missing_state";

/// 调度器输入：latest heartbeat run 的最小可观察快照。
#[derive(Debug, Clone, Default)]
pub struct SchedulerRunInput<'a> {
    pub run_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub status: Option<&'a str>,
    pub error_code: Option<&'a str>,
    pub error: Option<&'a str>,
    pub context_snapshot: Option<&'a Value>,
    pub result_json: Option<&'a Value>,
    pub liveness_state: Option<&'a str>,
    pub started_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
}

/// 调度器路由输入：调用层预先在 DB 上查询好的 agent hints。
///
/// 字段含义：
/// - `owner_agent_id`：当前 cause 决定出的可直接 wake 的 agent；
///   `None` 通常意味着该 cause 需要 monitor_only 或 board escalation。
/// - `return_owner_agent_id`：cause 解除（如 quota 重置）后真正接手 run 的 agent。
/// - `previous_owner_agent_id`：原始 assignee，用于 plan evidence。
/// - `routing_fallback_reason`：当 owner 走 manager ladder 而非 original 时记录原因。
#[derive(Debug, Clone, Default)]
pub struct SchedulerRoutingHints {
    pub owner_agent_id: Option<Uuid>,
    pub return_owner_agent_id: Option<Uuid>,
    pub previous_owner_agent_id: Option<Uuid>,
    pub routing_fallback_reason: Option<String>,
}

/// 调度器附加上下文：issue 实体标识 + 显式 cause override。
#[derive(Debug, Clone)]
pub struct SchedulerContext {
    pub company_id: Uuid,
    pub source_issue_id: Uuid,
    pub recovery_cause_override: Option<StrandedRecoveryCause>,
    pub successful_run_handoff_evidence: Option<Value>,
    pub workspace_validation_fingerprint_override: Option<String>,
}

/// 调度器最终输出：可直接交给 `plan_to_upsert_recovery_action` 写入 DB。
#[derive(Debug, Clone)]
pub struct SchedulerCandidate {
    pub cause: StrandedRecoveryCause,
    pub plan: SourceScopedRecoveryActionPlan,
    pub routing: SchedulerRoutingHints,
    pub evidence: Value,
    pub retry_at: Option<DateTime<Utc>>,
}

/// 纯函数：从 run 输入推断 recovery cause。
///
/// 与 Node `resolveStrandedRecoveryCause` 对齐：
/// 1. 显式 override 优先（在 `decide_recovery_scheduler_plan` 中处理）
/// 2. retry_reason 是 successful_run_missing_state / execution_review_participant_recovery → 对应 cause
/// 3. workspaceValidation 非空 payload → WorkspaceValidationFailed
/// 4. adapter 失败分类为 ConfigurationIncomplete → ConfigurationIncomplete
/// 5. provider_quota 分类为 ProviderQuota
/// 6. error_code = process_lost → ProcessLost
/// 7. error_code = codex_output_inactivity_monitor → CodexOutputInactivityMonitor
/// 8. 通用 provider_quota 嗅探（error_code=adapter_failed + usage/limit）→ ProviderQuota
/// 9. 其余 → RuntimeFailure
pub fn decide_recovery_cause(input: &SchedulerRunInput) -> StrandedRecoveryCause {
    if let Some(reason) = read_context_retry_reason(input.context_snapshot) {
        if reason == STRANDED_RECOVERY_SUCCESSFUL_RUN_MISSING_STATE_REASON {
            return StrandedRecoveryCause::SuccessfulRunMissingState;
        }
        if reason == STRANDED_RECOVERY_EXECUTION_REVIEW_PARTICIPANT_REASON {
            return StrandedRecoveryCause::ExecutionReviewParticipantRecovery;
        }
    }
    if let Some(payload) = read_workspace_validation_payload(input.result_json) {
        if !payload.is_null() {
            return StrandedRecoveryCause::WorkspaceValidationFailed;
        }
    }
    if let Some(classification) =
        classify_adapter_failure(input.error, input.error_code, input.result_json, Utc::now())
    {
        return match classification {
            AdapterFailureRecoveryClassification::ConfigurationIncomplete => {
                StrandedRecoveryCause::ConfigurationIncomplete
            }
            AdapterFailureRecoveryClassification::ProviderQuota { .. } => {
                StrandedRecoveryCause::ProviderQuota
            }
        };
    }
    if input.error_code == Some("process_lost") {
        return StrandedRecoveryCause::ProcessLost;
    }
    if input.error_code == Some("codex_output_inactivity_monitor") {
        return StrandedRecoveryCause::CodexOutputInactivityMonitor;
    }
    if is_provider_quota_recovery(input) {
        return StrandedRecoveryCause::ProviderQuota;
    }
    StrandedRecoveryCause::RuntimeFailure
}

/// 纯函数：根据 cause 决定 routing。
///
/// 输入已经包含「调用方在 DB 上确认过的可唤醒 owner」；
/// 这里只根据 cause 把 wake_policy / monitor_policy 字段组织好。
pub fn build_routing_for_cause(
    cause: StrandedRecoveryCause,
    hints: &SchedulerRoutingHints,
) -> SchedulerRoutingHints {
    let mut next = hints.clone();
    if cause == StrandedRecoveryCause::ProviderQuota && next.owner_agent_id.is_none() {
        if next.routing_fallback_reason.is_none() {
            next.routing_fallback_reason = Some(
                "provider_quota requires waiting for the original assignee to become invokable again."
                    .to_owned(),
            );
        }
    }
    if cause == StrandedRecoveryCause::ConfigurationIncomplete && next.owner_agent_id.is_some() {
        next.owner_agent_id = None;
        if next.routing_fallback_reason.is_none() {
            next.routing_fallback_reason = Some(
                "configuration_incomplete requires a manual secret binding before any wake."
                    .to_owned(),
            );
        }
    }
    next
}

/// 纯函数：完整调度入口。
///
/// 串联：cause → routing → plan → evidence → candidate。
pub fn decide_recovery_scheduler_plan(
    ctx: &SchedulerContext,
    run: &SchedulerRunInput,
    routing: &SchedulerRoutingHints,
    now: DateTime<Utc>,
) -> SchedulerCandidate {
    let cause = ctx
        .recovery_cause_override
        .unwrap_or_else(|| decide_recovery_cause(run));
    let routing = build_routing_for_cause(cause, routing);
    let plan = build_source_scoped_recovery_action_plan(
        ctx.company_id,
        ctx.source_issue_id,
        cause,
        routing.owner_agent_id,
        routing.return_owner_agent_id,
        ctx.workspace_validation_fingerprint_override
            .as_ref()
            .map(|s| s.as_str())
            .or_else(|| read_workspace_validation_fingerprint(run.result_json)),
    );
    let evidence = build_scheduler_evidence(ctx, run, &routing);
    let retry_at = if cause == StrandedRecoveryCause::ProviderQuota {
        derive_provider_quota_retry_at(run, now)
    } else {
        None
    };
    SchedulerCandidate {
        cause,
        plan,
        routing,
        evidence,
        retry_at,
    }
}

// ----------------------------------------------------------------------------
// Helper pure functions (mirror Node service.ts private helpers)
// ----------------------------------------------------------------------------

pub fn read_recovery_run_error_family(result_json: Option<&Value>) -> Option<String> {
    result_json
        .and_then(|v| v.get(STRANDED_RECOVERY_RUN_ERROR_FAMILY_KEY))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub fn is_provider_quota_recovery(run: &SchedulerRunInput) -> bool {
    if run.error_code == Some("provider_quota") {
        return true;
    }
    if read_recovery_run_error_family(run.result_json).as_deref()
        == Some(STRANDED_RECOVERY_PROVIDER_QUOTA_FAMILY)
    {
        return true;
    }
    if run.error_code != Some("adapter_failed") {
        return false;
    }
    let lower = run.error.unwrap_or("").to_ascii_lowercase();
    lower.contains("usage limit")
        || lower.contains("rate limit")
        || lower.contains("quota exceeded")
        || lower.contains("quota reset")
        || lower.contains("try again after")
}

pub fn read_workspace_validation_payload<'a>(result_json: Option<&'a Value>) -> Option<&'a Value> {
    let payload = result_json.and_then(|v| v.get(STRANDED_RECOVERY_WORKSPACE_VALIDATION_KEY))?;
    if let Value::Object(map) = payload {
        if map.is_empty() {
            return None;
        }
    }
    Some(payload)
}

pub fn read_workspace_validation_reason<'a>(result_json: Option<&'a Value>) -> Option<&'a str> {
    let payload = read_workspace_validation_payload(result_json)?;
    payload
        .get(STRANDED_RECOVERY_WORKSPACE_VALIDATION_REASON_KEY)
        .and_then(|v| v.as_str())
}

pub fn read_workspace_validation_fingerprint<'a>(
    result_json: Option<&'a Value>,
) -> Option<&'a str> {
    let payload = read_workspace_validation_payload(result_json)?;
    let raw = payload
        .get(STRANDED_RECOVERY_WORKSPACE_VALIDATION_FINGERPRINT_KEY)
        .and_then(|v| v.as_str())?;
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    }
}

pub fn read_context_retry_reason(context_snapshot: Option<&Value>) -> Option<String> {
    let value = context_snapshot?.get("retryReason")?;
    let raw = value.as_str()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn build_scheduler_evidence(
    ctx: &SchedulerContext,
    run: &SchedulerRunInput,
    routing: &SchedulerRoutingHints,
) -> Value {
    let mut evidence = serde_json::Map::new();
    evidence.insert(
        "companyId".into(),
        Value::String(ctx.company_id.to_string()),
    );
    evidence.insert(
        "sourceIssueId".into(),
        Value::String(ctx.source_issue_id.to_string()),
    );
    if let Some(agent_id) = run.agent_id {
        evidence.insert("runAgentId".into(), Value::String(agent_id.to_string()));
    }
    if let Some(status) = run.status {
        evidence.insert("runStatus".into(), Value::String(status.to_owned()));
    }
    if let Some(error_code) = run.error_code {
        evidence.insert("runErrorCode".into(), Value::String(error_code.to_owned()));
    }
    if let Some(family) = read_recovery_run_error_family(run.result_json) {
        evidence.insert("runErrorFamily".into(), Value::String(family));
    }
    if let Some(payload) = read_workspace_validation_payload(run.result_json) {
        evidence.insert("workspaceValidation".into(), payload.clone());
    }
    if let Some(retry_reason) = read_context_retry_reason(run.context_snapshot) {
        evidence.insert("retryReason".into(), Value::String(retry_reason));
    }
    if let Some(liveness_state) = run.liveness_state {
        evidence.insert(
            "livenessState".into(),
            Value::String(liveness_state.to_owned()),
        );
    }
    if let Some(previous_owner) = routing.previous_owner_agent_id {
        evidence.insert(
            "previousOwnerAgentId".into(),
            Value::String(previous_owner.to_string()),
        );
    }
    if let Some(reason) = &routing.routing_fallback_reason {
        evidence.insert(
            "routingFallbackReason".into(),
            Value::String(reason.clone()),
        );
    }
    if let Some(srh) = &ctx.successful_run_handoff_evidence {
        evidence.insert("successfulRunHandoff".into(), srh.clone());
    }
    Value::Object(evidence)
}

fn derive_provider_quota_retry_at(
    run: &SchedulerRunInput,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let value = run.result_json?;
    for key in [
        "retryNotBefore",
        "transientRetryNotBefore",
        "providerQuotaRetryNotBefore",
    ] {
        if let Some(candidate) = value.get(key).and_then(Value::as_str) {
            if let Ok(parsed) = candidate.parse::<DateTime<Utc>>() {
                if parsed > now {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

// ----------------------------------------------------------------------------
// Convenience serializable enums (mirror Node string literals)
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerDispatchKind {
    WakeOwner,
    MonitorOnly,
    ManualRepair,
    BoardEscalation,
}

impl SchedulerDispatchKind {
    pub fn from_wake_policy(plan: &SourceScopedRecoveryActionPlan) -> Self {
        match plan.wake_policy.get("type").and_then(|v| v.as_str()) {
            Some("wake_owner") => Self::WakeOwner,
            Some("monitor_only") => Self::MonitorOnly,
            Some("manual_repair_required") => Self::ManualRepair,
            _ => Self::BoardEscalation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uuid_v4(seed: u8) -> Uuid {
        let bytes = [seed; 16];
        Uuid::from_bytes(bytes)
    }

    fn run_input<'a>(
        error_code: Option<&'a str>,
        error: Option<&'a str>,
        result_json: &'a Value,
    ) -> SchedulerRunInput<'a> {
        SchedulerRunInput {
            run_id: Some(uuid_v4(1)),
            agent_id: Some(uuid_v4(2)),
            status: Some("failed"),
            error_code,
            error,
            context_snapshot: None,
            result_json: Some(result_json),
            liveness_state: None,
            started_at: None,
            created_at: None,
        }
    }

    #[test]
    fn decide_cause_routes_provider_quota_via_error_code() {
        let run = run_input(
            Some("provider_quota"),
            None,
            &*Box::leak(Box::new(serde_json::json!({}))),
        );
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::ProviderQuota
        );
    }

    #[test]
    fn decide_cause_routes_provider_quota_via_error_family() {
        let run = run_input(
            Some("adapter_failed"),
            None,
            &*Box::leak(Box::new(json!({ "errorFamily": "provider_quota" }))),
        );
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::ProviderQuota
        );
    }

    #[test]
    fn decide_cause_routes_process_lost_directly() {
        let run = run_input(
            Some("process_lost"),
            None,
            &*Box::leak(Box::new(serde_json::json!({}))),
        );
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::ProcessLost
        );
    }

    #[test]
    fn decide_cause_routes_codex_output_inactivity_monitor() {
        let run = run_input(
            Some("codex_output_inactivity_monitor"),
            None,
            &*Box::leak(Box::new(serde_json::json!({}))),
        );
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::CodexOutputInactivityMonitor
        );
    }

    #[test]
    fn decide_cause_routes_workspace_validation_payload() {
        let run = run_input(
            Some("workspace_validation_failed"),
            None,
            &*Box::leak(Box::new(
                json!({ "workspaceValidation": { "reason": "git_worktree_branch_incoherence" } }),
            )),
        );
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::WorkspaceValidationFailed
        );
    }

    #[test]
    fn decide_cause_routes_configuration_incomplete_via_classifier() {
        let run = run_input(
            Some("adapter_failed"),
            Some("missing API key"),
            &*Box::leak(Box::new(serde_json::json!({}))),
        );
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::ConfigurationIncomplete
        );
    }

    #[test]
    fn decide_cause_routes_execution_review_participant_via_retry_reason() {
        let snapshot = json!({ "retryReason": "execution_review_participant_recovery" });
        let run = SchedulerRunInput {
            context_snapshot: Some(&snapshot),
            ..run_input(
                Some("adapter_failed"),
                None,
                &*Box::leak(Box::new(serde_json::json!({}))),
            )
        };
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::ExecutionReviewParticipantRecovery
        );
    }

    #[test]
    fn decide_cause_routes_successful_run_missing_state_via_retry_reason() {
        let snapshot = json!({ "retryReason": "successful_run_missing_state" });
        let run = SchedulerRunInput {
            context_snapshot: Some(&snapshot),
            ..run_input(
                Some("interrupted"),
                None,
                &*Box::leak(Box::new(serde_json::json!({}))),
            )
        };
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::SuccessfulRunMissingState
        );
    }

    #[test]
    fn decide_cause_falls_back_to_runtime_failure() {
        let run = run_input(
            Some("adapter_failed"),
            Some("unexpected crash"),
            &*Box::leak(Box::new(serde_json::json!({}))),
        );
        assert_eq!(
            decide_recovery_cause(&run),
            StrandedRecoveryCause::RuntimeFailure
        );
    }

    #[test]
    fn routing_for_quota_without_owner_keeps_return_owner() {
        let hints = SchedulerRoutingHints {
            owner_agent_id: None,
            return_owner_agent_id: Some(uuid_v4(3)),
            previous_owner_agent_id: Some(uuid_v4(2)),
            routing_fallback_reason: None,
        };
        let routed = build_routing_for_cause(StrandedRecoveryCause::ProviderQuota, &hints);
        assert!(routed.owner_agent_id.is_none());
        assert_eq!(routed.return_owner_agent_id, Some(uuid_v4(3)));
        assert!(routed.routing_fallback_reason.is_some());
    }

    #[test]
    fn routing_for_configuration_incomplete_clears_owner() {
        let hints = SchedulerRoutingHints {
            owner_agent_id: Some(uuid_v4(7)),
            return_owner_agent_id: Some(uuid_v4(2)),
            previous_owner_agent_id: Some(uuid_v4(2)),
            routing_fallback_reason: None,
        };
        let routed =
            build_routing_for_cause(StrandedRecoveryCause::ConfigurationIncomplete, &hints);
        assert!(routed.owner_agent_id.is_none());
    }

    #[test]
    fn plan_evidence_includes_workspace_validation_payload() {
        let ctx = SchedulerContext {
            company_id: uuid_v4(10),
            source_issue_id: uuid_v4(11),
            recovery_cause_override: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        };
        let run = run_input(
            Some("workspace_validation_failed"),
            None,
            &*Box::leak(Box::new(
                json!({ "workspaceValidation": { "reason": "git_worktree_branch_incoherence" } }),
            )),
        );
        let routing = SchedulerRoutingHints::default();
        let now = Utc::now();
        let candidate = decide_recovery_scheduler_plan(&ctx, &run, &routing, now);
        assert_eq!(
            candidate.cause,
            StrandedRecoveryCause::WorkspaceValidationFailed
        );
        assert!(candidate.evidence.get("workspaceValidation").is_some());
        assert!(candidate.evidence.get("routingFallbackReason").is_none());
    }

    #[test]
    fn plan_candidate_dispatches_wake_when_owner_present() {
        let ctx = SchedulerContext {
            company_id: uuid_v4(10),
            source_issue_id: uuid_v4(11),
            recovery_cause_override: Some(StrandedRecoveryCause::ProcessLost),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        };
        let run = run_input(
            Some("process_lost"),
            None,
            &*Box::leak(Box::new(serde_json::json!({}))),
        );
        let routing = SchedulerRoutingHints {
            owner_agent_id: Some(uuid_v4(7)),
            return_owner_agent_id: Some(uuid_v4(2)),
            previous_owner_agent_id: Some(uuid_v4(2)),
            routing_fallback_reason: None,
        };
        let candidate = decide_recovery_scheduler_plan(&ctx, &run, &routing, Utc::now());
        assert_eq!(candidate.plan.kind, "stranded_assigned_issue");
        assert_eq!(
            SchedulerDispatchKind::from_wake_policy(&candidate.plan),
            SchedulerDispatchKind::WakeOwner
        );
    }

    #[test]
    fn plan_candidate_dispatches_monitor_only_for_quota_without_owner() {
        let ctx = SchedulerContext {
            company_id: uuid_v4(10),
            source_issue_id: uuid_v4(11),
            recovery_cause_override: Some(StrandedRecoveryCause::ProviderQuota),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        };
        let run = run_input(
            Some("provider_quota"),
            None,
            &*Box::leak(Box::new(
                json!({ "retryNotBefore": "2099-01-01T00:00:00Z" }),
            )),
        );
        let routing = SchedulerRoutingHints {
            owner_agent_id: None,
            return_owner_agent_id: Some(uuid_v4(2)),
            previous_owner_agent_id: Some(uuid_v4(2)),
            routing_fallback_reason: None,
        };
        let candidate = decide_recovery_scheduler_plan(&ctx, &run, &routing, Utc::now());
        assert_eq!(
            SchedulerDispatchKind::from_wake_policy(&candidate.plan),
            SchedulerDispatchKind::MonitorOnly
        );
        assert!(candidate.plan.monitor_policy.is_some());
        assert!(candidate.retry_at.is_some());
    }

    #[test]
    fn explicit_cause_override_wins() {
        let ctx = SchedulerContext {
            company_id: uuid_v4(10),
            source_issue_id: uuid_v4(11),
            recovery_cause_override: Some(StrandedRecoveryCause::ConfigurationIncomplete),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        };
        let run = run_input(
            Some("provider_quota"),
            None,
            &*Box::leak(Box::new(serde_json::json!({}))),
        );
        let candidate = decide_recovery_scheduler_plan(
            &ctx,
            &run,
            &SchedulerRoutingHints::default(),
            Utc::now(),
        );
        assert_eq!(
            candidate.cause,
            StrandedRecoveryCause::ConfigurationIncomplete
        );
    }
}
