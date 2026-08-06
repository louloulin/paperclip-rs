//! `enqueueStrandedIssueRecovery` 顶级 recovery action（Round 315）。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `enqueueStrandedIssueRecovery`：
//! 为 stranded issue 创建一个 wakeup request，让 agent 重新执行。
//!
//! 关键行为：
//! - 创建一个 agent_wakeup_requests row：
//!   - source = "automation"
//!   - trigger_detail = "system"
//!   - reason = input.reason（assignment_recovery / issue_continuation_needed / etc.）
//!   - payload 含 issueId + retryOfRunId（可选）+ extraContext
//!   - requested_by_actor_type = "system"
//! - 如果 retryOfRunId 提供：wake 创建成功后更新 heartbeat_run.run_id（标记 retry 来源）
//!
//! 设计：
//! - 纯 DB I/O 模块：通过 pc-repos AgentRepo::create_wakeup_request
//! - 单一职责：只创建 wake，不做其他副作用
//! - 幂等性：依赖 idempotency_key（如 caller 提供）
//! - 与 `enqueueInitialAssignedTodoDispatch` 配套：后者用于首次 dispatch
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::agent::{
    AgentRepo, HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType,
    WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::Db;

// ============================================================================
// Public types
// ============================================================================

/// `enqueue_stranded_issue_recovery` 输入。
#[derive(Debug, Clone)]
pub struct EnqueueStrandedRecoveryInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub agent_id: Uuid,
    /// Node `reason` 字段：assignment_recovery / issue_continuation_needed / etc.
    pub reason: String,
    /// Node `retryReason` 字段（写入 payload.wakeReason + contextSnapshot）。
    pub retry_reason: String,
    /// Node `source` 字段（issue.assignment_recovery / issue.interaction_continuation_recovery 等）。
    pub source: String,
    /// 可选：被重试的 run id。
    pub retry_of_run_id: Option<Uuid>,
    /// 可选：附加 context（写入 payload 和 context snapshot）。
    pub extra_context: Option<Value>,
    /// 可选：idempotency key（重复请求去重）。
    pub idempotency_key: Option<String>,
}

/// `enqueue_stranded_issue_recovery` 输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueStrandedRecoveryResult {
    /// wake request id。
    pub wake_request_id: Option<Uuid>,
    /// heartbeat_run id（如果 retry_of_run_id 提供且创建成功）。
    pub run_id: Option<Uuid>,
    /// 跳过原因（None 表示成功 enqueue）。
    pub skipped_reason: Option<EnqueueStrandedSkipReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnqueueStrandedSkipReason {
    /// agent 不存在或跨公司。
    InvalidAgent,
    /// 创建 wake 失败（FK 违反 / 约束）。
    WakeCreationFailed,
    /// 未提供 extra_context 路径上的简化失败。
    NoIdempotencyKeyForRetry,
}

/// 同 `enqueue_stranded_issue_recovery` 的简化版：首次 dispatch `todo` issue 时使用。
///
/// 与 Node `enqueueInitialAssignedTodoDispatch` 对齐：
/// - source = "assignment"
/// - trigger_detail = "system"
/// - reason = "issue_assigned"
/// - payload.mutation = "assigned_todo_liveness_dispatch"
#[derive(Debug, Clone)]
pub struct EnqueueInitialDispatchInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub agent_id: Uuid,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 为 stranded issue 创建 recovery wakeup request。
///
/// 与 Node `enqueueStrandedIssueRecovery` 1:1 对齐：
/// 1. 验证 agent 存在且同公司
/// 2. 构造 payload + context snapshot（合并 retry_of_run_id + extra_context）
/// 3. 调 AgentRepo::create_wakeup_request
/// 4. 如果 retry_of_run_id 提供：把 wake 创建的 run_id 关联回 heartbeat_run（标记 retry）
///
/// 返回 `EnqueueStrandedRecoveryResult`：成功 → wake_request_id + run_id；失败 → skipped_reason。
pub async fn enqueue_stranded_issue_recovery(
    db: &Db,
    input: EnqueueStrandedRecoveryInput,
) -> sqlx::Result<EnqueueStrandedRecoveryResult> {
    let mut result = EnqueueStrandedRecoveryResult {
        wake_request_id: None,
        run_id: None,
        skipped_reason: None,
    };

    // Step 1: 验证 agent
    let agent_row: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, company_id FROM agents WHERE id = $1")
            .bind(input.agent_id)
            .fetch_optional(db.pool())
            .await?;
    let (_, agent_company_id) = match agent_row {
        Some(r) => r,
        None => {
            result.skipped_reason = Some(EnqueueStrandedSkipReason::InvalidAgent);
            return Ok(result);
        }
    };
    if agent_company_id != input.company_id {
        result.skipped_reason = Some(EnqueueStrandedSkipReason::InvalidAgent);
        return Ok(result);
    }

    // Step 2: 构造 payload
    let mut payload = json!({
        "issueId": input.issue_id,
    });
    if let Some(retry_of_run_id) = input.retry_of_run_id {
        payload["retryOfRunId"] = json!(retry_of_run_id);
    }
    if let Some(extra) = &input.extra_context {
        if let (Some(p_obj), Some(e_obj)) = (payload.as_object_mut(), extra.as_object()) {
            for (k, v) in e_obj {
                p_obj.insert(k.clone(), v.clone());
            }
        }
    }

    // Step 3: 调 AgentRepo 创建 wake
    let repo = AgentRepo::new(db);
    let wake = repo
        .create_wakeup_request(NewAgentWakeupRequest {
            company_id: input.company_id,
            agent_id: input.agent_id,
            source: HeartbeatInvocationSource::Automation,
            trigger_detail: Some(WakeupTriggerDetail::System),
            reason: Some(input.reason.clone()),
            payload: Some(payload.clone()),
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
            eprintln!("enqueue_stranded_issue_recovery: wake create failed: {e}");
            e
        })?;

    result.wake_request_id = Some(wake.id);

    // Step 4: 如果 retry_of_run_id 提供，关联 wake 到原 run（标记 retry 关系）
    if let Some(retry_of_run_id) = input.retry_of_run_id {
        let updated: Option<(Uuid,)> = sqlx::query_as(
            "UPDATE heartbeat_runs SET retry_of_run_id = $1, updated_at = now() \
             WHERE id = $2 AND company_id = $3 RETURNING id",
        )
        .bind(retry_of_run_id)
        .bind(retry_of_run_id)
        .bind(input.company_id)
        .fetch_optional(db.pool())
        .await?;
        if let Some((run_id,)) = updated {
            result.run_id = Some(run_id);
        }
    }

    Ok(result)
}

/// 首次 dispatch 一个 `todo` issue（与 Node `enqueueInitialAssignedTodoDispatch` 对齐）。
///
/// 用于 todo issue 首次进入 assigned 状态时的 wake 触发。
pub async fn enqueue_initial_assigned_todo_dispatch(
    db: &Db,
    input: EnqueueInitialDispatchInput,
) -> sqlx::Result<EnqueueStrandedRecoveryResult> {
    let mut result = EnqueueStrandedRecoveryResult {
        wake_request_id: None,
        run_id: None,
        skipped_reason: None,
    };

    // 验证 agent
    let agent_row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM agents WHERE id = $1 AND company_id = $2 AND status != 'offline'",
    )
    .bind(input.agent_id)
    .bind(input.company_id)
    .fetch_optional(db.pool())
    .await?;
    if agent_row.is_none() {
        result.skipped_reason = Some(EnqueueStrandedSkipReason::InvalidAgent);
        return Ok(result);
    }

    let payload = json!({
        "issueId": input.issue_id,
        "mutation": "assigned_todo_liveness_dispatch",
    });

    let repo = AgentRepo::new(db);
    let wake = repo
        .create_wakeup_request(NewAgentWakeupRequest {
            company_id: input.company_id,
            agent_id: input.agent_id,
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
        .await
        .map_err(|e| {
            eprintln!("enqueue_initial_assigned_todo_dispatch: failed: {e}");
            e
        })?;

    result.wake_request_id = Some(wake.id);
    Ok(result)
}
