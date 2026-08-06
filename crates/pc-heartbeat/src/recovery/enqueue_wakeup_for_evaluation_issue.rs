//! `enqueue_wakeup_for_evaluation_issue` —— evaluation issue 创建后唤醒 reviewer。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `createOrUpdateStaleRunEvaluation`
//! 末尾的 `deps.enqueueWakeup(ownerAgentId, ...)` 块。
//!
//! 触发场景：当 heartbeat 检测到 stale active run 且为 critical level 时，创建
//! `stale_active_run_evaluation` evaluation issue 并指派给 reviewer agent，
//! 然后通过本模块唤醒 reviewer 立即处理。
//!
//! 设计：
//! - 复用 `enqueue_stranded_issue_recovery` 的核心路径（调 `AgentRepo::create_wakeup_request`）
//! - 但 payload 语义不同：staleRunId / sourceIssueId 而非 retryOfRunId
//! - source = "assignment", reason = "issue_assigned"（与 Node 一致）
//!
//! 业务约束：
//! - ownerAgentId 为空 → skip（与 Node `if (ownerAgentId)` 一致）
//! - agent 跨公司 → skip
//! - 重复请求靠 idempotency_key 去重（如果 caller 提供）
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use pc_repos::agent::{
    AgentRepo, HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType,
    WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::Db;

// ============================================================================
// Public types
// ============================================================================

/// `enqueue_wakeup_for_evaluation_issue` 输入。
#[derive(Debug, Clone)]
pub struct EnqueueEvaluationWakeInput {
    pub company_id: Uuid,
    /// Evaluation issue id（被创建出来需要 review 的 issue）。
    pub evaluation_issue_id: Uuid,
    /// Reviewer agent id（evaluation issue.assignee_agent_id）。
    pub owner_agent_id: Uuid,
    /// Stale run id（reviewer 需要查的 run）。
    pub stale_run_id: Uuid,
    /// Source issue id（被 stale run 处理的 issue）。
    pub source_issue_id: Option<Uuid>,
    /// 可选：idempotency key。
    pub idempotency_key: Option<String>,
}

/// `enqueue_wakeup_for_evaluation_issue` 输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueEvaluationWakeResult {
    pub wake_request_id: Option<Uuid>,
    pub skipped_reason: Option<EnqueueEvaluationWakeSkipReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnqueueEvaluationWakeSkipReason {
    /// ownerAgentId 为 None（reviewer 未指定）。
    NoOwnerAgent,
    /// agent 不存在或跨公司。
    InvalidAgent,
    /// agent 当前状态为 offline。
    AgentOffline,
    /// 创建 wake 失败（DB 约束等）。
    WakeCreationFailed,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 为 evaluation issue 创建 reviewer wakeup。
///
/// 与 Node `createOrUpdateStaleRunEvaluation` 末尾的 `deps.enqueueWakeup` 对齐：
/// - source = "assignment"
/// - trigger_detail = "system"
/// - reason = "issue_assigned"
/// - payload 含 evaluation_issue_id + stale_run_id + source_issue_id（可选）
///
/// 返回 `EnqueueEvaluationWakeResult`：成功 → wake_request_id；失败 → skipped_reason。
///
/// 注意：ownerAgentId 为 None 是合法的（reviewer 自动分配失败）→ skip。
pub async fn enqueue_wakeup_for_evaluation_issue(
    db: &Db,
    input: EnqueueEvaluationWakeInput,
) -> sqlx::Result<EnqueueEvaluationWakeResult> {
    let mut result = EnqueueEvaluationWakeResult {
        wake_request_id: None,
        skipped_reason: None,
    };

    // 1. ownerAgentId 必填
    if input.owner_agent_id.is_nil() {
        result.skipped_reason = Some(EnqueueEvaluationWakeSkipReason::NoOwnerAgent);
        return Ok(result);
    }

    // 2. 验证 agent
    let agent_row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT company_id, status::text FROM agents WHERE id = $1")
            .bind(input.owner_agent_id)
            .fetch_optional(db.pool())
            .await?;
    let (agent_company_id, agent_status) = match agent_row {
        Some(r) => r,
        None => {
            result.skipped_reason = Some(EnqueueEvaluationWakeSkipReason::InvalidAgent);
            return Ok(result);
        }
    };
    if agent_company_id != input.company_id {
        result.skipped_reason = Some(EnqueueEvaluationWakeSkipReason::InvalidAgent);
        return Ok(result);
    }
    if agent_status == "offline" {
        result.skipped_reason = Some(EnqueueEvaluationWakeSkipReason::AgentOffline);
        return Ok(result);
    }

    // 3. 构造 payload（与 Node 对齐）
    let mut payload = json!({
        "issueId": input.evaluation_issue_id,
        "staleRunId": input.stale_run_id,
    });
    if let Some(sid) = input.source_issue_id {
        payload["sourceIssueId"] = json!(sid);
    }

    // 4. 创建 wake
    let repo = AgentRepo::new(db);
    let wake = repo
        .create_wakeup_request(NewAgentWakeupRequest {
            company_id: input.company_id,
            agent_id: input.owner_agent_id,
            source: HeartbeatInvocationSource::Assignment,
            trigger_detail: Some(WakeupTriggerDetail::System),
            reason: Some("issue_assigned".to_string()),
            payload: Some(payload),
            status: WakeupRequestStatus::Queued,
            coalesced_count: 0,
            requested_by_actor_type: Some(WakeupActorType::System),
            requested_by_actor_id: None,
            idempotency_key: input.idempotency_key.clone(),
            run_id: None,
            error: None,
        })
        .await
        .map_err(|e| {
            eprintln!("enqueue_wakeup_for_evaluation_issue: wake create failed: {e}");
            e
        })?;

    result.wake_request_id = Some(wake.id);
    Ok(result)
}
