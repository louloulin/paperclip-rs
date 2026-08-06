//! `finalizeAgentAfterSourceResolvedRun` —— Node `services/recovery/service.ts:1648` 对齐。
//!
//! 业务语义：
//! - 当 stale active run 被 fold 终止后，同步更新 agent 状态：
//!   - 若 agent 还有其他 running runs → 保持 'running'
//!   - 若没有 → 根据 run 的 final_status 设为 'idle' (succeeded/cancelled)
//!   - paused/terminated 状态不被覆盖
//! - 更新 last_heartbeat_at = now()
//!
//! 设计意图：
//! - 直接 SQL：count 其他 running runs + update agent status（条件：status NOT IN (paused, terminated)）
//! - 简洁：单事务，无 advisory lock（agent 状态更新并发冲突风险低）
//!
//! 调用方：fold path（R342 接入 create_or_update_stale_run_evaluation_full）

use uuid::Uuid;

use pc_repos::Db;

/// `finalize_agent_after_source_resolved_run` 入参。
#[derive(Debug, Clone)]
pub struct FinalizeAgentInput {
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub agent_id: Uuid,
    pub final_run_status: String, // "succeeded" | "cancelled"
}

/// 同步 agent 状态（fold 后）。
///
/// 流程：
/// 1. 查 agent 还有几个 running runs（排除被 fold 的 run_id）
/// 2. 决定 next_status：
///    - count > 0 → "running"（保持）
///    - count == 0 + final_run_status in (succeeded, cancelled) → "idle"
///    - 其他 → 不变
/// 3. UPDATE agents WHERE id AND company_id AND status NOT IN (paused, terminated)
///
/// 与 Node `finalizeAgentAfterSourceResolvedRun` 完全对齐。
pub async fn finalize_agent_after_source_resolved_run(
    db: &Db,
    run_id: Uuid,
    company_id: Uuid,
    agent_id: Uuid,
    final_run_status: &str,
) -> sqlx::Result<()> {
    // Step 1: 查其他 running runs（排除被 fold 的 run_id）
    let (running_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_runs \
         WHERE agent_id = $1 AND company_id = $2 AND status = 'running' AND id != $3",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(run_id)
    .fetch_one(db.pool())
    .await?;

    // Step 2: 决定 next_status
    let next_status = if running_count > 0 {
        "running"
    } else if final_run_status == "succeeded" || final_run_status == "cancelled" {
        "idle"
    } else {
        return Ok(()); // 其他 status 不变
    };

    // Step 3: UPDATE agents（条件：status NOT IN paused/terminated）
    sqlx::query(
        "UPDATE agents \
         SET status = $1, last_heartbeat_at = now(), updated_at = now() \
         WHERE id = $2 AND company_id = $3 \
           AND status NOT IN ('paused', 'terminated')",
    )
    .bind(next_status)
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // 单元测试有限：实际 DB 测试在 round342 集成测试中
}
